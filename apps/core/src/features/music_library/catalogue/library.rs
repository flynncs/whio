use std::sync::Arc;

use uuid::Uuid;

use crate::features::music_library::{
    availability::ProviderRegistry,
    {
        domain::{
            AlbumEntry, AlbumId, AlbumListFilter, AlbumOrder, AlbumSummary, ArtistId,
            ArtistListFilter, ArtistSort, ArtistSummary, Availability, GenreSummary, LibraryState,
            PageLimit, SourceIdentity, Track, TrackId, TrackListFilter, TrackOrder, TrackPage,
            ValidationError,
        },
        storage::contracts::{LibraryRead, SourceLookup, StateStore, TrackRepositoryError},
    },
};

use super::{CatalogueError, SearchHit};

pub struct LibraryService {
    library: Arc<dyn LibraryRead>,
    source_lookup: Arc<dyn SourceLookup>,
    state: Arc<dyn StateStore>,
    registry: ProviderRegistry,
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
        }
    }

    pub fn with_registry(mut self, registry: ProviderRegistry) -> Self {
        self.registry = registry;
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

    pub async fn sources(&self, track_id: &TrackId) -> Result<Vec<SourceIdentity>, CatalogueError> {
        self.source_lookup
            .find_sources(track_id)
            .await
            .map_err(CatalogueError::Repository)
    }
}
