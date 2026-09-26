use std::sync::Arc;

use async_trait::async_trait;
use uuid::Uuid;
use whio_resolver_api::{
    apis::{
        Error as TransportError, configuration::Configuration, default_api::ResolvePlaybackError,
    },
    models::{PlaybackResolveRequest, SourceIdentity as SourceIdentityDto},
};

use crate::{
    features::music_library::domain::ProviderKind,
    features::playback::providers::ProviderError,
    features::playback::{PlayableMedia, PlaybackProvider, PlaybackSource, ResolvedPlayback},
    platform::request::RequestContext,
};

pub struct YtmAdapter {
    transport: Option<Arc<Configuration>>,
}

impl YtmAdapter {
    pub fn new(transport: Option<Arc<Configuration>>) -> Self {
        Self { transport }
    }
    pub fn disabled() -> Self {
        Self { transport: None }
    }
}

#[async_trait]
impl PlaybackProvider for YtmAdapter {
    fn kind(&self) -> ProviderKind {
        ProviderKind::YoutubeMusic
    }

    async fn resolve(
        &self,
        source: &PlaybackSource,
        _user: Option<&Uuid>,
        context: &RequestContext,
    ) -> Result<Option<ResolvedPlayback>, ProviderError> {
        let PlaybackSource::Youtube(source) = source else {
            return Ok(None);
        };
        let Some(transport) = self.transport.as_ref() else {
            return Err(ProviderError::Disabled);
        };
        let dto = SourceIdentityDto::new(
            source.identity().provider_id().as_str().to_owned(),
            source.identity().external_id().to_owned(),
        );
        let response = whio_resolver_api::apis::default_api::resolve_playback(
            transport,
            PlaybackResolveRequest::new(dto),
            Some(context.request_id().as_str()),
        )
        .await
        .map_err(map_playback_error)?;
        let media: PlayableMedia = response
            .try_into()
            .map_err(ProviderError::InvalidPlaybackData)?;
        Ok(Some(ResolvedPlayback::Remote(media)))
    }
}

fn map_playback_error(error: TransportError<ResolvePlaybackError>) -> ProviderError {
    match error {
        TransportError::Reqwest(error) => ProviderError::request(error),
        TransportError::Serde(error) => ProviderError::malformed(error),
        TransportError::SerdePathToError(error) => ProviderError::malformed(error),
        TransportError::Io(error) => ProviderError::request(error),
        TransportError::ResponseError(response) => match response.status {
            reqwest::StatusCode::BAD_REQUEST => ProviderError::InvalidRequest,
            reqwest::StatusCode::NOT_FOUND => ProviderError::SourceNotFound,
            reqwest::StatusCode::UNPROCESSABLE_ENTITY => ProviderError::UnsupportedProvider,
            reqwest::StatusCode::INTERNAL_SERVER_ERROR => ProviderError::Internal,
            reqwest::StatusCode::BAD_GATEWAY => ProviderError::ResolutionFailed,
            reqwest::StatusCode::SERVICE_UNAVAILABLE => ProviderError::ProviderUnavailable,
            _ => ProviderError::ProviderUnavailable,
        },
    }
}

mod mapping {
    use crate::features::playback::{
        MediaMetadata, PlayableMedia, PlaybackUrl, ValidationError as PlaybackValidationError,
    };
    use chrono::Utc;
    use whio_resolver_api::models::{
        MediaMetadata as MediaMetadataDto, PlaybackResolveResponse as PlaybackResolveResponseDto,
    };

    impl TryFrom<PlaybackResolveResponseDto> for PlayableMedia {
        type Error = PlaybackValidationError;

        fn try_from(response: PlaybackResolveResponseDto) -> Result<Self, Self::Error> {
            let PlaybackResolveResponseDto {
                url,
                headers,
                expires_at,
                media,
            } = response;
            let MediaMetadataDto {
                content_type,
                content_length_bytes,
                codec,
                bitrate_kbps,
                duration_ms,
            } = *media;

            let url = PlaybackUrl::new(url)?;
            let expires_at = expires_at.flatten().map(|value| value.with_timezone(&Utc));
            let content_length_bytes = optional_u64(
                content_length_bytes,
                PlaybackValidationError::InvalidContentLength,
            )?;
            let duration_ms = optional_u64(duration_ms, PlaybackValidationError::InvalidDuration)?;
            let metadata = MediaMetadata::new(
                content_type.flatten(),
                content_length_bytes,
                codec.flatten(),
                bitrate_kbps.flatten(),
                duration_ms,
            )?;

            Ok(PlayableMedia::new(
                url,
                filter_headers(headers),
                expires_at,
                metadata,
            ))
        }
    }

    fn filter_headers(
        headers: std::collections::HashMap<String, String>,
    ) -> std::collections::HashMap<String, String> {
        headers
            .into_iter()
            .filter(|(name, _)| {
                !matches!(
                    name.to_ascii_lowercase().as_str(),
                    "host"
                        | "content-length"
                        | "content-range"
                        | "range"
                        | "connection"
                        | "cookie"
                        | "authorization"
                        | "proxy-authenticate"
                        | "proxy-authorization"
                        | "transfer-encoding"
                        | "upgrade"
                        | "trailer"
                        | "te"
                )
            })
            .collect()
    }

    fn optional_u64(
        value: Option<Option<i32>>,
        error: PlaybackValidationError,
    ) -> Result<Option<u64>, PlaybackValidationError> {
        value
            .flatten()
            .map(u64::try_from)
            .transpose()
            .map_err(|_| error)
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use std::collections::HashMap;
        fn playback_response() -> PlaybackResolveResponseDto {
            PlaybackResolveResponseDto {
                url: "https://media.example.test/audio".to_owned(),
                headers: HashMap::from([("User-Agent".to_owned(), "whio-test".to_owned())]),
                expires_at: Some(Some("2026-08-08T12:00:00+12:00".parse().unwrap())),
                media: Box::new(MediaMetadataDto {
                    content_type: Some(Some("audio/webm".to_owned())),
                    content_length_bytes: Some(Some(1_024)),
                    codec: Some(Some("opus".to_owned())),
                    bitrate_kbps: Some(Some(128.5)),
                    duration_ms: Some(Some(337_250)),
                }),
            }
        }
        #[test]
        fn valid_playback_dto_becomes_valid_domain_media() {
            let playback = PlayableMedia::try_from(playback_response()).unwrap();
            let (url, headers, expires_at, metadata) = playback.into_parts();

            assert_eq!(url.as_url().as_str(), "https://media.example.test/audio");
            assert_eq!(headers["User-Agent"], "whio-test");
            assert_eq!(
                expires_at.unwrap().to_rfc3339(),
                "2026-08-08T00:00:00+00:00"
            );
            assert_eq!(metadata.content_type(), Some("audio/webm"));
            assert_eq!(metadata.content_length_bytes(), Some(1_024));
            assert_eq!(metadata.codec(), Some("opus"));
            assert_eq!(metadata.bitrate_kbps(), Some(128.5));
            assert_eq!(metadata.duration_ms(), Some(337_250));
        }

        #[test]
        fn negative_playback_measurements_are_rejected() {
            let mut response = playback_response();
            response.media.content_length_bytes = Some(Some(-1));

            assert!(matches!(
                PlayableMedia::try_from(response),
                Err(PlaybackValidationError::InvalidContentLength)
            ));

            let mut response = playback_response();
            response.media.duration_ms = Some(Some(-1));

            assert!(matches!(
                PlayableMedia::try_from(response),
                Err(PlaybackValidationError::InvalidDuration)
            ));
        }
    }
}
