use chrono::Utc;
use whio_resolver_api::models::{
    CatalogueCandidate as CatalogueCandidateDto, MediaMetadata as MediaMetadataDto,
    PlaybackResolveResponse as PlaybackResolveResponseDto,
};

use crate::{
    catalogue::CatalogueCandidate,
    playback::{
        MediaMetadata, PlayableMedia, PlaybackUrl, ValidationError as PlaybackValidationError,
    },
    tracks::{ProviderId, SourceIdentity, SourceScope, TrackMetadata, ValidationError},
};

impl TryFrom<CatalogueCandidateDto> for CatalogueCandidate {
    type Error = ValidationError;

    fn try_from(candidate: CatalogueCandidateDto) -> Result<Self, Self::Error> {
        let provider_id = ProviderId::new(candidate.source.provider_id)?;
        let source = SourceIdentity::new(
            provider_id,
            SourceScope::Global,
            candidate.source.external_id,
        )?;

        let duration_ms = candidate
            .duration_ms
            .flatten()
            .map(u64::try_from)
            .transpose()
            .map_err(|_| ValidationError::InvalidDuration)?;

        let metadata = TrackMetadata::new(candidate.title, candidate.artists, duration_ms)?;

        Ok(CatalogueCandidate::new(source, metadata))
    }
}

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

        Ok(PlayableMedia::new(url, headers, expires_at, metadata))
    }
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
    use std::collections::HashMap;

    use whio_resolver_api::models::SourceIdentity as SourceIdentityDto;

    use super::*;

    fn candidate() -> CatalogueCandidateDto {
        CatalogueCandidateDto {
            source: Box::new(SourceIdentityDto::new(
                "example_music".to_owned(),
                "example-source-id".to_owned(),
            )),
            title: "Instant Crush".to_owned(),
            artists: vec!["Daft Punk".to_owned()],
            duration_ms: Some(Some(337_000)),
        }
    }

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
    fn valid_dto_becomes_valid_domain_candidate() {
        let candidate = CatalogueCandidate::try_from(candidate()).unwrap();

        assert_eq!(candidate.source().provider_id().as_str(), "example_music");
        assert_eq!(candidate.source().external_id(), "example-source-id");
        assert_eq!(candidate.title(), "Instant Crush");
        assert_eq!(candidate.artists(), ["Daft Punk"]);
        assert_eq!(candidate.duration_ms(), Some(337_000));
    }

    #[test]
    fn invalid_dto_is_rejected_at_the_adapter_boundary() {
        let mut candidate = candidate();
        candidate.duration_ms = Some(Some(-1));

        assert!(matches!(
            CatalogueCandidate::try_from(candidate),
            Err(ValidationError::InvalidDuration)
        ));
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
