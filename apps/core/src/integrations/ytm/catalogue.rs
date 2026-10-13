use std::sync::Arc;

use async_trait::async_trait;
use whio_resolver_api::{
    apis::{
        Error as TransportError, configuration::Configuration, default_api::SearchCatalogueError,
    },
    models::{CatalogueEntityRequest, CatalogueSearchRequest, SourceIdentity as SourceIdentityDto},
};

use crate::{
    features::music_library::catalogue::ProviderError,
    features::music_library::catalogue::{
        CatalogueCandidate, CataloguePage, CatalogueProvider, CatalogueSearch, FetchedAlbum,
        SearchOffset,
    },
    features::music_library::domain::{
        AlbumMetadata, ArtistCredit, SourceIdentity, YOUTUBE_MUSIC_PROVIDER_ID,
    },
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

    async fn ready(&self, capability: &str) -> Result<&Configuration, ProviderError> {
        let transport = self.transport.as_deref().ok_or(ProviderError::Disabled)?;
        if !advertises(transport, capability).await? {
            return Err(ProviderError::UnsupportedProvider);
        }
        Ok(transport)
    }
}

#[async_trait]
impl CatalogueProvider for YtmCatalogue {
    async fn search_tracks(
        &self,
        search: &CatalogueSearch,
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

    async fn search_albums(
        &self,
        search: &CatalogueSearch,
        context: &RequestContext,
    ) -> Result<CataloguePage<AlbumMetadata>, ProviderError> {
        let transport = self.ready("catalogue.album-search").await?;
        let response = whio_resolver_api::apis::default_api::search_catalogue_albums(
            transport,
            search_request(search),
            Some(context.request_id().as_str()),
        )
        .await
        .map_err(map_catalogue_transport_error)?;
        let items = response
            .results
            .into_iter()
            .map(mapping::album_candidate)
            .collect::<Result<Vec<_>, _>>()?;
        let next_offset = page_offset(response.next_offset)?;
        Ok(CataloguePage::new(items, next_offset))
    }

    async fn search_artists(
        &self,
        search: &CatalogueSearch,
        context: &RequestContext,
    ) -> Result<CataloguePage<ArtistCredit>, ProviderError> {
        let transport = self.ready("catalogue.artist-search").await?;
        let response = whio_resolver_api::apis::default_api::search_catalogue_artists(
            transport,
            search_request(search),
            Some(context.request_id().as_str()),
        )
        .await
        .map_err(map_catalogue_transport_error)?;
        let items = response
            .results
            .into_iter()
            .map(mapping::artist_candidate)
            .collect::<Result<Vec<_>, _>>()?;
        let next_offset = page_offset(response.next_offset)?;
        Ok(CataloguePage::new(items, next_offset))
    }

    async fn fetch_artist_albums(
        &self,
        source: &SourceIdentity,
        context: &RequestContext,
    ) -> Result<Vec<AlbumMetadata>, ProviderError> {
        if source.provider_id().as_str() != YOUTUBE_MUSIC_PROVIDER_ID {
            return Err(ProviderError::UnsupportedProvider);
        }
        let transport = self.ready("catalogue.artist").await?;
        let body = CatalogueEntityRequest::new(SourceIdentityDto::new(
            source.provider_id().as_str().to_owned(),
            source.external_id().to_owned(),
        ));
        let response = whio_resolver_api::apis::default_api::fetch_catalogue_artist(
            transport,
            body,
            Some(context.request_id().as_str()),
        )
        .await
        .map_err(map_catalogue_transport_error)?;
        response
            .results
            .into_iter()
            .map(mapping::album_candidate)
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
        .map_err(map_catalogue_transport_error)?;
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

fn search_request(search: &CatalogueSearch) -> CatalogueSearchRequest {
    let mut request = CatalogueSearchRequest::new(
        search.query().as_str().to_owned(),
        i32::from(search.limit().get()),
    );
    request.offset = Some(search.offset().get() as i32);
    request
}

async fn advertises(transport: &Configuration, capability: &str) -> Result<bool, ProviderError> {
    let response = whio_resolver_api::apis::default_api::get_capabilities(transport, None)
        .await
        .map_err(map_catalogue_transport_error)?;
    Ok(response.providers.iter().any(|provider| {
        provider.provider_id == YOUTUBE_MUSIC_PROVIDER_ID
            && provider
                .capabilities
                .iter()
                .any(|advertised| advertised == capability)
    }))
}

fn page_offset(offset: Option<i32>) -> Result<Option<SearchOffset>, ProviderError> {
    offset
        .map(|offset| {
            u32::try_from(offset)
                .map_err(|_| invalid_offset())
                .and_then(|offset| SearchOffset::new(offset).map_err(|_| invalid_offset()))
        })
        .transpose()
}

fn invalid_offset() -> ProviderError {
    ProviderError::InvalidCatalogueData(
        crate::features::music_library::domain::ValidationError::OutOfRange {
            field: "offset",
            min: 0,
            max: i64::from(crate::features::music_library::catalogue::MAX_SEARCH_OFFSET),
        },
    )
}

fn map_catalogue_transport_error<T>(error: TransportError<T>) -> ProviderError {
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

mod mapping {
    use whio_resolver_api::models::{
        CatalogueAlbum as CatalogueAlbumDto, CatalogueAlbumCandidate as CatalogueAlbumCandidateDto,
        CatalogueArtist as CatalogueArtistDto,
        CatalogueArtistCandidate as CatalogueArtistCandidateDto,
        CatalogueCandidate as CatalogueCandidateDto, SourceIdentity as SourceIdentityDto,
    };

    use crate::{
        features::music_library::catalogue::{CatalogueCandidate, ProviderError},
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

    pub(super) fn album_candidate(
        candidate: CatalogueAlbumCandidateDto,
    ) -> Result<AlbumMetadata, ProviderError> {
        let source = provider_source(Some(candidate.source))
            .map_err(ProviderError::InvalidCatalogueData)?
            .expect("an album candidate has a source");
        let artist = candidate.artists.into_iter().next().ok_or_else(|| {
            ProviderError::InvalidCatalogueData(ValidationError::Empty { field: "artist" })
        })?;
        let artist = artist_credit(artist).map_err(ProviderError::InvalidCatalogueData)?;
        let artwork = candidate
            .artwork_url
            .flatten()
            .and_then(|url| ArtworkRef::url(url).ok());
        AlbumMetadata::new(candidate.title)
            .map_err(ProviderError::InvalidCatalogueData)?
            .with_provider(Some(source))
            .with_year(candidate.year.flatten())
            .map_err(ProviderError::InvalidCatalogueData)?
            .with_release_type(candidate.release_type.flatten())
            .map_err(ProviderError::InvalidCatalogueData)
            .map(|album| album.with_artwork(artwork).with_album_artist(artist))
    }

    pub(super) fn artist_candidate(
        candidate: CatalogueArtistCandidateDto,
    ) -> Result<ArtistCredit, ProviderError> {
        let source =
            provider_source(Some(candidate.source)).map_err(ProviderError::InvalidCatalogueData)?;
        ArtistCredit::new(candidate.name, None)
            .map(|credit| credit.with_provider(source))
            .map_err(ProviderError::InvalidCatalogueData)
    }

    fn artist_credit(artist: CatalogueArtistDto) -> Result<ArtistCredit, ValidationError> {
        Ok(ArtistCredit::new(artist.name, None)?.with_provider(provider_source(artist.source)?))
    }

    fn album_metadata(album: CatalogueAlbumDto) -> Result<AlbumMetadata, ValidationError> {
        let artwork = album
            .artwork_url
            .flatten()
            .and_then(|url| ArtworkRef::url(url).ok());
        Ok(AlbumMetadata::new(album.title)?
            .with_provider(provider_source(album.source)?)
            .with_year(album.year.flatten())?
            .with_release_type(album.release_type.flatten())?
            .with_artwork(artwork))
    }

    fn track_number(
        value: Option<i32>,
        field: &'static str,
    ) -> Result<Option<u16>, ValidationError> {
        value
            .map(|value| {
                u16::try_from(value).ok().filter(|value| *value > 0).ok_or(
                    ValidationError::OutOfRange {
                        field,
                        min: 1,
                        max: i64::from(u16::MAX),
                    },
                )
            })
            .transpose()
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

            let alternates = candidate
                .alternate_sources
                .unwrap_or_default()
                .into_iter()
                .map(|alternate| {
                    SourceIdentity::new(
                        ProviderId::new(alternate.provider_id)?,
                        SourceScope::Global,
                        alternate.external_id,
                    )
                })
                .collect::<Result<Vec<_>, _>>()?;

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

            CatalogueCandidate::new(source, metadata)
                .with_alternates(alternates)
                .with_album(album)
                .with_artwork(artwork)
                .with_track_numbers(
                    track_number(candidate.disc_number.flatten(), "disc_number")?,
                    track_number(candidate.track_number.flatten(), "track_number")?,
                )
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
                track_number: None,
                disc_number: None,
                album: None,
                artwork_url: None,
                alternate_sources: None,
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
                artwork_url: Some(Some("https://art.test/ram-album.jpg".to_owned())),
            }));
            dto.artwork_url = Some(Some("https://art.test/ram.jpg".to_owned()));

            let candidate = CatalogueCandidate::try_from(dto).unwrap();

            let album = candidate.album().expect("the dto album becomes metadata");
            assert_eq!(album.title(), "Random Access Memories");
            assert_eq!(album.album_artist().unwrap().name(), "Daft Punk");
            assert_eq!(album.year(), Some(2013));
            assert_eq!(album.release_type(), Some("album"));
            assert_eq!(
                album.artwork(),
                Some(&ArtworkRef::url("https://art.test/ram-album.jpg".to_owned()).unwrap())
            );
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
        fn dto_track_numbers_are_validated_and_reach_membership() {
            let mut dto = candidate();
            dto.track_number = Some(Some(4));
            dto.disc_number = Some(Some(2));
            let mapped = CatalogueCandidate::try_from(dto).unwrap();
            let membership = mapped.album_track(3);
            assert_eq!(membership.track_number(), Some(4));
            assert_eq!(membership.disc_number(), Some(2));
            assert_eq!(membership.position(), 3);
            for value in [0, -1, 65_536] {
                let mut dto = candidate();
                dto.track_number = Some(Some(value));
                assert!(CatalogueCandidate::try_from(dto).is_err());
            }
            let mut dto = candidate();
            dto.track_number = Some(None);
            dto.disc_number = Some(None);
            let membership = CatalogueCandidate::try_from(dto).unwrap().album_track(0);
            assert_eq!(membership.track_number(), None);
            assert_eq!(membership.disc_number(), None);
        }

        #[test]
        fn dto_alternates_reach_the_candidate() {
            let mut dto = candidate();
            dto.alternate_sources = Some(vec![SourceIdentityDto::new(
                "example_music".to_owned(),
                "album-video-id".to_owned(),
            )]);

            let candidate = CatalogueCandidate::try_from(dto).unwrap();

            assert_eq!(candidate.alternates().len(), 1);
            assert_eq!(candidate.alternates()[0].external_id(), "album-video-id");
            assert_eq!(
                candidate.alternates()[0].provider_id().as_str(),
                "example_music"
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
