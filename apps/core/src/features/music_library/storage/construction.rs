use std::{path::PathBuf, sync::Arc};

use sqlx::PgPool;

use super::super::{
    artwork::ArtworkService,
    availability::ProviderRegistry,
    catalogue::{CatalogueProvider, CompletionService, DedupMode, LibraryService, SearchService},
    library::ScanService,
    library_state::LibraryStateService,
    playlists::PlaylistService,
    sources::SourceQueries,
    storage::contracts::{CatalogueWrite, LibraryRead, PlaylistStore, SourceLookup, StateStore},
    storage::postgres::PostgresTrackRepository,
};

pub struct MusicServices {
    pub search: Arc<SearchService>,
    pub library: Arc<LibraryService>,
    pub completion: Arc<CompletionService>,
    pub playlists: Arc<PlaylistService>,
    pub artwork: Arc<ArtworkService>,
    pub activity: Arc<LibraryStateService>,
    pub scanner: Arc<ScanService>,
    pub sources: Arc<SourceQueries>,
}

pub fn construct(
    pool: PgPool,
    providers: Vec<Arc<dyn CatalogueProvider>>,
    enabled_providers: Vec<&'static str>,
    dedup_mode: DedupMode,
    media_roots: Vec<PathBuf>,
    artwork_host_suffixes: Vec<String>,
) -> MusicServices {
    let registry = ProviderRegistry::with_enabled(enabled_providers);
    let store = Arc::new(PostgresTrackRepository::new(pool).with_registry(registry.clone()));
    let library: Arc<dyn LibraryRead> = Arc::clone(&store) as Arc<dyn LibraryRead>;
    let writes: Arc<dyn CatalogueWrite> = Arc::clone(&store) as Arc<dyn CatalogueWrite>;
    let sources: Arc<dyn SourceLookup> = Arc::clone(&store) as Arc<dyn SourceLookup>;
    let state: Arc<dyn StateStore> = Arc::clone(&store) as Arc<dyn StateStore>;

    let search = Arc::new(
        SearchService::with_providers(
            Arc::clone(&library),
            Arc::clone(&writes),
            Arc::clone(&sources),
            Arc::clone(&state),
            providers.clone(),
        )
        .with_dedup_mode(dedup_mode),
    );
    let library_service = Arc::new(
        LibraryService::new(
            Arc::clone(&library),
            Arc::clone(&sources),
            Arc::clone(&state),
        )
        .with_registry(registry.clone()),
    );
    let completion = Arc::new(
        CompletionService::new(
            Arc::clone(&library_service),
            Arc::clone(&writes),
            Arc::clone(&sources),
            providers,
        )
        .with_registry(registry),
    );
    let playlists = Arc::new(PlaylistService::new(
        Arc::clone(&store) as Arc<dyn PlaylistStore>,
        Arc::clone(&library_service),
    ));
    let scanner = ScanService::new(writes, Arc::clone(&sources), media_roots);
    let artwork = Arc::new(ArtworkService::new(
        Arc::clone(&library),
        Arc::clone(&sources),
        artwork_host_suffixes,
    ));
    let activity = Arc::new(LibraryStateService::new(state, library));
    MusicServices {
        search,
        library: library_service,
        completion,
        playlists,
        artwork,
        activity,
        scanner,
        sources: Arc::new(SourceQueries::new(sources)),
    }
}
