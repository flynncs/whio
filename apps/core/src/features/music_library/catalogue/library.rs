use std::sync::Arc;

use chrono::Utc;
use uuid::Uuid;

use crate::features::metadata::{MetadataError, MetadataProfile, MetadataService, RefreshReason};
use crate::features::music_library::{
    artwork::{ArtworkCache, ArtworkError, ArtworkRole, CoverArtTarget},
    availability::ProviderRegistry,
    {
        domain::{
            AlbumEntry, AlbumId, AlbumListFilter, AlbumOrder, AlbumSummary, ArtistId,
            ArtistInformation, ArtistListFilter, ArtistSort, ArtistSummary, Availability,
            EnrichmentSubject, GenreSummary, LibraryState, PageLimit, RefreshDemand, RefreshStatus,
            SourceIdentity, Track, TrackId, TrackListFilter, TrackOrder, TrackPage, TrackView,
            ValidationError,
        },
        storage::contracts::{LibraryRead, SourceLookup, StateStore, TrackRepositoryError},
    },
};

use super::{CatalogueError, ProviderError, SearchHit};

pub struct LibraryService {
    library: Arc<dyn LibraryRead>,
    source_lookup: Arc<dyn SourceLookup>,
    state: Arc<dyn StateStore>,
    registry: ProviderRegistry,
    metadata: Option<Arc<MetadataService>>,
    artwork: Option<Arc<ArtworkCache>>,
}

pub struct AlbumDetail {
    pub album: AlbumSummary,
    pub entries: Vec<AlbumEntry>,
}

pub struct ArtistDetail {
    pub artist: ArtistSummary,
    pub albums: Vec<AlbumSummary>,
}

fn catalogue_error(error: TrackRepositoryError) -> CatalogueError {
    match error {
        TrackRepositoryError::UnknownLibraryId { id } => {
            tracing::debug!(library_id = %id, "rejected an unregistered library id");

            CatalogueError::Invalid(ValidationError::Invalid {
                field: "library_id",
            })
        }
        error => CatalogueError::Repository(error),
    }
}

fn metadata_failure(error: MetadataError) -> CatalogueError {
    match error {
        MetadataError::Disabled => CatalogueError::Provider(ProviderError::Disabled),
        MetadataError::Unavailable
        | MetadataError::DisabledSource
        | MetadataError::ProviderUnavailable => {
            CatalogueError::Provider(ProviderError::ProviderUnavailable)
        }
        MetadataError::InvalidData => {
            CatalogueError::Provider(ProviderError::malformed(MetadataError::InvalidData))
        }
    }
}

fn artwork_failure(error: ArtworkError) -> CatalogueError {
    match error {
        ArtworkError::Repository(error) => CatalogueError::Repository(error),
        error => CatalogueError::Provider(ProviderError::request(error)),
    }
}

impl LibraryService {
    pub fn new(
        library: Arc<dyn LibraryRead>,
        sources: Arc<dyn SourceLookup>,
        state: Arc<dyn StateStore>,
    ) -> Self {
        Self {
            library,
            source_lookup: sources,
            state,
            registry: ProviderRegistry::builtin(),
            metadata: None,
            artwork: None,
        }
    }

    pub fn with_registry(mut self, registry: ProviderRegistry) -> Self {
        self.registry = registry;
        self
    }

    pub fn with_metadata(mut self, metadata: Arc<MetadataService>) -> Self {
        self.metadata = Some(metadata);
        self
    }

    pub fn with_artwork_cache(mut self, cache: Arc<ArtworkCache>) -> Self {
        self.artwork = Some(cache);
        self
    }

    pub fn availability(&self, sources: &[SourceIdentity]) -> Availability {
        self.registry.availability(sources)
    }

    pub async fn track(&self, track_id: &TrackId) -> Result<Option<Track>, CatalogueError> {
        self.library
            .find_track(track_id)
            .await
            .map_err(CatalogueError::Repository)
    }

    pub async fn track_detail(
        &self,
        track_id: &TrackId,
        user: Option<&Uuid>,
    ) -> Result<Option<SearchHit>, CatalogueError> {
        let Some(track) = self.track(track_id).await? else {
            return Ok(None);
        };
        let album = self
            .library
            .find_album_for_track(track_id)
            .await
            .map_err(CatalogueError::Repository)?;
        let sources = self.sources(track_id).await?;
        let state = match user {
            Some(user) => self
                .state
                .find_track_state(track_id, *user)
                .await
                .map_err(CatalogueError::Repository)?,
            None => LibraryState::default(),
        };

        Ok(Some(
            SearchHit::new(track, album, sources).with_state(state),
        ))
    }

    pub async fn album_list(
        &self,
        offset: u32,
        limit: u32,
        order: AlbumOrder,
        filter: AlbumListFilter,
        user: Option<&Uuid>,
    ) -> Result<Vec<AlbumSummary>, CatalogueError> {
        self.library
            .list_albums(offset, limit, order, filter, user.copied())
            .await
            .map_err(catalogue_error)
    }

    pub async fn genres(&self) -> Result<Vec<GenreSummary>, CatalogueError> {
        self.library
            .list_genres()
            .await
            .map_err(CatalogueError::Repository)
    }

    pub async fn album_count(
        &self,
        user: Option<&Uuid>,
        filter: AlbumListFilter,
    ) -> Result<u32, CatalogueError> {
        self.library
            .count_albums(user.copied(), filter)
            .await
            .map_err(catalogue_error)
    }

    pub async fn artist_count(
        &self,
        user: Option<&Uuid>,
        filter: &ArtistListFilter,
    ) -> Result<u32, CatalogueError> {
        self.library
            .count_artists(user.copied(), filter)
            .await
            .map_err(catalogue_error)
    }

    pub async fn album_detail(
        &self,
        album_id: &AlbumId,
        user: Option<&Uuid>,
    ) -> Result<Option<AlbumDetail>, CatalogueError> {
        let Some(album) = self
            .library
            .find_album_summary(album_id, user.copied())
            .await
            .map_err(CatalogueError::Repository)?
        else {
            return Ok(None);
        };

        let entries = self
            .library
            .find_album_entries(album_id, user.copied())
            .await
            .map_err(CatalogueError::Repository)?;

        Ok(Some(AlbumDetail { album, entries }))
    }

    pub async fn artist_list(
        &self,
        offset: u32,
        limit: u32,
        sort: ArtistSort,
        filter: &ArtistListFilter,
        user: Option<&Uuid>,
    ) -> Result<Vec<ArtistSummary>, CatalogueError> {
        self.library
            .list_artists(offset, limit, sort, filter, user.copied())
            .await
            .map_err(catalogue_error)
    }

    pub async fn artist_by_exact_name(
        &self,
        name: &str,
    ) -> Result<Option<ArtistId>, CatalogueError> {
        self.library
            .find_artist_by_exact_name(name)
            .await
            .map_err(catalogue_error)
    }

    pub async fn artist_detail(
        &self,
        artist_id: &ArtistId,
        user: Option<&Uuid>,
    ) -> Result<Option<ArtistDetail>, CatalogueError> {
        let Some(artist) = self
            .library
            .find_artist(artist_id, user.copied())
            .await
            .map_err(CatalogueError::Repository)?
        else {
            return Ok(None);
        };

        let albums = self
            .library
            .find_albums_for_artist(artist_id, user.copied())
            .await
            .map_err(CatalogueError::Repository)?;
        let state = match user {
            Some(user) => self
                .state
                .find_artist_state(artist_id, *user)
                .await
                .map_err(CatalogueError::Repository)?,
            None => LibraryState::default(),
        };
        let artist = ArtistSummary::new(artist, albums.len() as u32).with_state(state);

        Ok(Some(ArtistDetail { artist, albums }))
    }

    pub async fn enrichment_subject(
        &self,
        artist_id: &ArtistId,
    ) -> Result<Option<EnrichmentSubject>, CatalogueError> {
        let Some(artist) = self
            .library
            .find_artist(artist_id, None)
            .await
            .map_err(CatalogueError::Repository)?
        else {
            return Ok(None);
        };

        let identities = self
            .source_lookup
            .find_artist_sources(artist_id)
            .await
            .map_err(CatalogueError::Repository)?;

        Ok(Some(EnrichmentSubject::new(
            artist.id().clone(),
            artist.name().to_owned(),
            artist.musicbrainz_artist_id().map(str::to_owned),
            identities,
        )))
    }

    pub async fn artist_information(
        &self,
        artist_id: &ArtistId,
        user: Option<&Uuid>,
        profile: &MetadataProfile,
        language: &str,
        refresh: RefreshDemand,
    ) -> Result<Option<ArtistInformation>, CatalogueError> {
        let Some(artist) = self.artist_detail(artist_id, user).await? else {
            return Ok(None);
        };
        let metadata = self
            .metadata
            .as_ref()
            .ok_or_else(|| CatalogueError::Provider(ProviderError::Disabled))?;
        let Some(subject) = self.enrichment_subject(artist_id).await? else {
            return Ok(None);
        };

        let record = metadata
            .read_artist(&subject, profile, language, Utc::now())
            .await
            .map_err(metadata_failure)?;
        let outcome = match refresh {
            RefreshDemand::ReadOnly => None,
            RefreshDemand::RequestIfDue => Some(
                metadata
                    .request_refresh(&subject, profile, language, RefreshReason::Read)
                    .await
                    .map_err(metadata_failure)?,
            ),
            RefreshDemand::Force => Some(
                metadata
                    .request_refresh(&subject, profile, language, RefreshReason::Manual)
                    .await
                    .map_err(metadata_failure)?,
            ),
        };
        let state = metadata
            .refresh_state(&subject, profile, language)
            .await
            .map_err(metadata_failure)?;

        let mut portraits = Vec::new();
        if let Some(cache) = self.artwork.as_ref() {
            for role in [ArtworkRole::Portrait, ArtworkRole::Wide] {
                if let Some(asset) = cache
                    .ready(artist_id, role, None)
                    .await
                    .map_err(artwork_failure)?
                {
                    portraits.push(asset.descriptor);
                }
            }
        }

        Ok(Some(ArtistInformation {
            artist,
            metadata: record,
            portraits,
            cover_art: CoverArtTarget::artist(artist_id).encode(),
            refresh: RefreshStatus { state, outcome },
        }))
    }

    pub async fn track_page(
        &self,
        offset: u32,
        limit: PageLimit,
        order: TrackOrder,
        filter: &TrackListFilter,
        user: Option<&Uuid>,
    ) -> Result<TrackPage, CatalogueError> {
        self.library
            .page_tracks(offset, limit.get(), order, filter, user.copied())
            .await
            .map_err(catalogue_error)
    }

    pub async fn album_track_page(
        &self,
        album_id: &AlbumId,
        offset: u32,
        limit: PageLimit,
        order: TrackOrder,
        filter: &TrackListFilter,
        user: Option<&Uuid>,
    ) -> Result<TrackPage, CatalogueError> {
        self.library
            .page_album_tracks(album_id, offset, limit.get(), order, filter, user.copied())
            .await
            .map_err(catalogue_error)
    }

    pub async fn track_views(
        &self,
        track_ids: &[TrackId],
        user: Option<&Uuid>,
    ) -> Result<Vec<TrackView>, CatalogueError> {
        self.library
            .track_views(track_ids, user.copied())
            .await
            .map_err(CatalogueError::Repository)
    }

    pub async fn sources(&self, track_id: &TrackId) -> Result<Vec<SourceIdentity>, CatalogueError> {
        self.source_lookup
            .find_sources(track_id)
            .await
            .map_err(CatalogueError::Repository)
    }
}
