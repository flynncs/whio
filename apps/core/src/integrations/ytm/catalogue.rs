use std::sync::Arc;

use async_trait::async_trait;
use uuid::Uuid;
use whio_resolver_api::{
    apis::{
        Error as TransportError,
        configuration::Configuration,
        default_api::{FetchCatalogueAlbumError, SearchCatalogueError},
    },
    models::{CatalogueEntityRequest, CatalogueSearchRequest, SourceIdentity as SourceIdentityDto},
};

use crate::{
    features::music_library::catalogue::ProviderError,
    features::music_library::catalogue::{
        CatalogueCandidate, CatalogueProvider, CatalogueSearch, FetchedAlbum,
    },
    features::music_library::domain::{SourceIdentity, YOUTUBE_MUSIC_PROVIDER_ID},
    platform::request::RequestContext,
};

pub struct YtmCatalogue {
    transport: Option<Arc<Configuration>>,
}

impl YtmCatalogue {
    pub fn new(transport: Option<Arc<Configuration>>) -> Self {
        Self { transport }
    }
    pub fn disabled() -> Self {
        Self { transport: None }
    }
}

#[async_trait]
impl CatalogueProvider for YtmCatalogue {
    async fn search(
        &self,
        search: &CatalogueSearch,
        _user: Option<&Uuid>,
        context: &RequestContext,
    ) -> Result<Vec<CatalogueCandidate>, ProviderError> {
        let Some(transport) = self.transport.as_ref() else {
            return Err(ProviderError::Disabled);
        };
        let mut body = CatalogueSearchRequest::new(
            search.query().as_str().to_owned(),
            i32::from(search.limit().get()),
        );
        body.offset = Some(search.offset().get() as i32);
        let response = whio_resolver_api::apis::default_api::search_catalogue(
            transport,
            body,
            Some(context.request_id().as_str()),
        )
        .await
        .map_err(map_search_error)?;
        response
            .results
            .into_iter()
            .map(|candidate| {
                candidate
                    .try_into()
                    .map_err(ProviderError::InvalidCatalogueData)
            })
            .collect()
    }

    async fn fetch_album(
        &self,
        source: &SourceIdentity,
        context: &RequestContext,
    ) -> Result<FetchedAlbum, ProviderError> {
        if source.provider_id().as_str() != YOUTUBE_MUSIC_PROVIDER_ID {
            return Err(ProviderError::UnsupportedProvider);
        }
        let Some(transport) = self.transport.as_ref() else {
            return Err(ProviderError::Disabled);
        };
        let body = CatalogueEntityRequest::new(SourceIdentityDto::new(
            source.provider_id().as_str().to_owned(),
            source.external_id().to_owned(),
        ));
        let response = whio_resolver_api::apis::default_api::fetch_catalogue_album(
            transport,
            body,
            Some(context.request_id().as_str()),
        )
        .await
        .map_err(map_album_error)?;
        let candidates = response
            .results
            .into_iter()
            .map(|candidate| {
                candidate
                    .try_into()
                    .map_err(ProviderError::InvalidCatalogueData)
            })
            .collect::<Result<Vec<_>, _>>()?;
        let reported_count = response
            .reported_count
            .flatten()
            .and_then(|count| u32::try_from(count).ok());

        Ok(FetchedAlbum::new(candidates, reported_count))
    }
}

fn map_search_error(error: TransportError<SearchCatalogueError>) -> ProviderError {
    match error {
        TransportError::Reqwest(error) => ProviderError::request(error),
        TransportError::Serde(error) => ProviderError::malformed(error),
        TransportError::SerdePathToError(error) => ProviderError::malformed(error),
        TransportError::Io(error) => ProviderError::request(error),
        TransportError::ResponseError(response) => match response.status {
            reqwest::StatusCode::BAD_REQUEST => ProviderError::InvalidRequest,
            reqwest::StatusCode::INTERNAL_SERVER_ERROR => ProviderError::Internal,
            reqwest::StatusCode::SERVICE_UNAVAILABLE => ProviderError::ProviderUnavailable,
            _ => ProviderError::ProviderUnavailable,
        },
    }
}

fn map_album_error(error: TransportError<FetchCatalogueAlbumError>) -> ProviderError {
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
            reqwest::StatusCode::SERVICE_UNAVAILABLE => ProviderError::ProviderUnavailable,
            _ => ProviderError::ProviderUnavailable,
        },
    }
}

mod mapping {
    use whio_resolver_api::models::{
        CatalogueAlbum as CatalogueAlbumDto, CatalogueArtist as CatalogueArtistDto,
        CatalogueCandidate as CatalogueCandidateDto, SourceIdentity as SourceIdentityDto,
    };

    use crate::{
        features::music_library::catalogue::CatalogueCandidate,
        features::music_library::domain::{
            AlbumMetadata, ArtistCredit, ArtworkRef, ProviderId, SourceIdentity, SourceScope,
            TrackMetadata, ValidationError,
        },
    };

    fn provider_source(
        source: Option<Box<SourceIdentityDto>>,
    ) -> Result<Option<SourceIdentity>, ValidationError> {
        source
            .map(|source| {
                SourceIdentity::new(
                    ProviderId::new(source.provider_id)?,
                    SourceScope::Global,
                    source.external_id,
                )
            })
            .transpose()
    }

    fn artist_credit(artist: CatalogueArtistDto) -> Result<ArtistCredit, ValidationError> {
        Ok(ArtistCredit::new(artist.name, None)?.with_provider(provider_source(artist.source)?))
    }

    fn album_metadata(album: CatalogueAlbumDto) -> Result<AlbumMetadata, ValidationError> {
        AlbumMetadata::new(album.title)?
            .with_provider(provider_source(album.source)?)
            .with_year(album.year.flatten())?
            .with_release_type(album.release_type.flatten())
    }

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

            let artists = candidate
                .artists
                .into_iter()
                .map(artist_credit)
                .collect::<Result<Vec<_>, _>>()?;
            let metadata = TrackMetadata::new(candidate.title, artists, duration_ms)?;

            let album = match candidate.album {
                Some(album) => {
                    let mut album = album_metadata(*album)?;
                    if let Some(artist) = metadata.artists().first() {
                        album = album.with_album_artist(artist.clone());
                    }

                    Some(album)
                }
                None => None,
            };
            let artwork = candidate
                .artwork_url
                .flatten()
                .and_then(|url| ArtworkRef::url(url).ok());

            Ok(CatalogueCandidate::new(source, metadata)
                .with_album(album)
                .with_artwork(artwork))
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use whio_resolver_api::models::SourceIdentity as SourceIdentityDto;
        fn candidate() -> CatalogueCandidateDto {
            CatalogueCandidateDto {
                source: Box::new(SourceIdentityDto::new(
                    "example_music".to_owned(),
                    "example-source-id".to_owned(),
                )),
                title: "Instant Crush".to_owned(),
                artists: vec![CatalogueArtistDto {
                    name: "Daft Punk".to_owned(),
                    source: Some(Box::new(SourceIdentityDto::new(
                        "example_music".to_owned(),
                        "artist-1".to_owned(),
                    ))),
                }],
                duration_ms: Some(Some(337_000)),
                album: None,
                artwork_url: None,
            }
        }

        #[test]
        fn valid_dto_becomes_valid_domain_candidate() {
            let candidate = CatalogueCandidate::try_from(candidate()).unwrap();

            assert_eq!(candidate.source().provider_id().as_str(), "example_music");
            assert_eq!(candidate.source().external_id(), "example-source-id");
            assert_eq!(candidate.title(), "Instant Crush");
            assert_eq!(
                candidate
                    .artists()
                    .iter()
                    .map(|artist| artist.name())
                    .collect::<Vec<_>>(),
                ["Daft Punk"]
            );
            assert_eq!(candidate.duration_ms(), Some(337_000));
        }

        #[test]
        fn dto_album_and_artwork_reach_the_candidate() {
            let mut dto = candidate();
            dto.album = Some(Box::new(CatalogueAlbumDto {
                title: "Random Access Memories".to_owned(),
                source: Some(Box::new(SourceIdentityDto::new(
                    "example_music".to_owned(),
                    "album-1".to_owned(),
                ))),
                year: Some(Some(2013)),
                release_type: Some(Some("Album".to_owned())),
            }));
            dto.artwork_url = Some(Some("https://art.test/ram.jpg".to_owned()));

            let candidate = CatalogueCandidate::try_from(dto).unwrap();

            let album = candidate.album().expect("the dto album becomes metadata");
            assert_eq!(album.title(), "Random Access Memories");
            assert_eq!(album.album_artist().unwrap().name(), "Daft Punk");
            assert_eq!(album.year(), Some(2013));
            assert_eq!(album.release_type(), Some("Album"));
            assert_eq!(
                album.provider().map(|provider| provider.external_id()),
                Some("album-1")
            );
            assert_eq!(
                candidate.artists()[0]
                    .provider()
                    .map(|provider| provider.external_id()),
                Some("artist-1")
            );
            assert_eq!(
                candidate.artwork(),
                Some(&ArtworkRef::url("https://art.test/ram.jpg".to_owned()).unwrap())
            );
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
    }
}
