use std::{path::PathBuf, sync::Arc};

use sqlx::PgPool;

use super::super::{
    artwork::{ArtworkCache, ArtworkService},
    availability::ProviderRegistry,
    catalogue::{CatalogueProvider, CompletionService, DedupMode, LibraryService, SearchService},
    delivery::{DeliveryDestinations, DeliveryService, ListenSender, ListenSource},
    discovery::DiscoveryService,
    history::ListenService,
    library::{LibraryRegistry, ScanService},
    library_state::LibraryStateService,
    playlists::PlaylistService,
    queue::RestoreQueueService,
    sources::SourceQueries,
    storage::contracts::{
        ArtworkStore, CatalogueWrite, DeliveryStore, LibraryRead, LibraryStore, ListenStore,
        PlaylistStore, RestoreQueueStore, SourceLookup, StateStore,
    },
    storage::postgres::{PostgresLibraryStore, PostgresTrackRepository},
};
use crate::features::metadata::{
    MetadataDocuments, MetadataService, capabilities::CapabilitySnapshot, freshness::Jitter,
    selection::FieldOrder, storage::MetadataStore,
};

pub struct LibraryWiring {
    pub pool: PgPool,
    pub providers: Vec<Arc<dyn CatalogueProvider>>,
    pub enabled_providers: Vec<&'static str>,
    pub dedup_mode: DedupMode,
    pub media_roots: Vec<PathBuf>,
    pub artwork_host_suffixes: Vec<String>,
    pub w4: W4Dependencies,
}

pub struct W4Dependencies {
    pub metadata: Option<MetadataWiring>,
    pub artwork_cache: Option<ArtworkCacheWiring>,
    pub delivery: Option<DeliveryWiring>,
}

pub struct MetadataWiring {
    pub store: Arc<dyn MetadataStore>,
    pub documents: Arc<dyn MetadataDocuments>,
    pub capabilities: Arc<CapabilitySnapshot>,
    pub order: FieldOrder,
    pub adapter_version: String,
    pub jitter: Arc<dyn Jitter>,
}

pub struct ArtworkCacheWiring {
    pub store: Arc<dyn ArtworkStore>,
    pub root: Option<PathBuf>,
    pub budget_bytes: u64,
}

pub struct DeliveryWiring {
    pub store: Arc<dyn DeliveryStore>,
    pub source: Arc<dyn ListenSource>,
    pub sender: Arc<dyn ListenSender>,
    pub destinations: Arc<dyn DeliveryDestinations>,
}

pub struct MusicServices {
    pub search: Arc<SearchService>,
    pub library: Arc<LibraryService>,
    pub completion: Arc<CompletionService>,
    pub playlists: Arc<PlaylistService>,
    pub queue: Arc<RestoreQueueService>,
    pub listen: Arc<ListenService>,
    pub artwork: Arc<ArtworkService>,
    pub activity: Arc<LibraryStateService>,
    pub scanner: Arc<ScanService>,
    pub sources: Arc<SourceQueries>,
    pub library_registry: Arc<LibraryRegistry>,
    pub discovery: Arc<DiscoveryService>,
    pub delivery: Option<Arc<DeliveryService>>,
    pub metadata: Option<Arc<MetadataService>>,
    pub subjects: Arc<dyn crate::features::metadata::SubjectSource>,
}

pub fn construct(wiring: LibraryWiring) -> MusicServices {
    let LibraryWiring {
        pool,
        providers,
        enabled_providers,
        dedup_mode,
        media_roots,
        artwork_host_suffixes,
        w4,
    } = wiring;

    let registry = ProviderRegistry::with_enabled(enabled_providers);
    let delivery = w4.delivery.map(|delivery| {
        Arc::new(DeliveryService::new(
            delivery.store,
            delivery.source,
            delivery.sender,
            delivery.destinations,
        ))
    });
    let store = PostgresTrackRepository::new(pool.clone()).with_registry(registry.clone());
    let store = match delivery.as_ref() {
        Some(delivery) => store.with_delivery(Arc::clone(delivery)),
        None => store,
    };
    let store = Arc::new(store);
    let library_store: Arc<dyn LibraryStore> = Arc::new(PostgresLibraryStore::new(pool.clone()));
    let library_registry = Arc::new(LibraryRegistry::new(library_store));
    let library: Arc<dyn LibraryRead> = Arc::clone(&store) as Arc<dyn LibraryRead>;
    let writes: Arc<dyn CatalogueWrite> = Arc::clone(&store) as Arc<dyn CatalogueWrite>;
    let sources: Arc<dyn SourceLookup> = Arc::clone(&store) as Arc<dyn SourceLookup>;
    let state: Arc<dyn StateStore> = Arc::clone(&store) as Arc<dyn StateStore>;
    let listens: Arc<dyn ListenStore> = Arc::clone(&store) as Arc<dyn ListenStore>;

    let metadata = w4.metadata.map(|wiring| {
        Arc::new(MetadataService::new(
            pool,
            wiring.store,
            wiring.documents,
            wiring.capabilities,
            wiring.order,
            wiring.adapter_version,
            wiring.jitter,
        ))
    });

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
    let library_service = LibraryService::new(
        Arc::clone(&library),
        Arc::clone(&sources),
        Arc::clone(&state),
    )
    .with_registry(registry.clone());
    let library_service = match metadata.as_ref() {
        Some(metadata) => library_service.with_metadata(Arc::clone(metadata)),
        None => library_service,
    };
    let library_service = match w4.artwork_cache.as_ref() {
        Some(cache) => library_service.with_artwork_cache(Arc::new(ArtworkCache::new(
            Arc::clone(&cache.store),
            cache.root.clone(),
            cache.budget_bytes,
        ))),
        None => library_service,
    };
    let library_service = Arc::new(library_service);
    let completion = Arc::new(
        CompletionService::new(
            Arc::clone(&library_service),
            Arc::clone(&writes),
            Arc::clone(&sources),
            providers,
            Arc::clone(&search),
        )
        .with_registry(registry),
    );
    let playlists = Arc::new(PlaylistService::new(
        Arc::clone(&store) as Arc<dyn PlaylistStore>,
        Arc::clone(&library_service),
    ));
    let listen = Arc::new(ListenService::new(
        Arc::clone(&store) as Arc<dyn ListenStore>,
        Arc::clone(&library) as Arc<dyn LibraryRead>,
    ));
    let queue = Arc::new(RestoreQueueService::new(
        Arc::clone(&store) as Arc<dyn RestoreQueueStore>
    ));
    let scanner = ScanService::new(writes, Arc::clone(&sources), media_roots);
    let artwork = Arc::new(ArtworkService::new(
        Arc::clone(&library),
        Arc::clone(&sources),
        artwork_host_suffixes,
    ));
    let activity = Arc::new(LibraryStateService::new(
        Arc::clone(&state),
        Arc::clone(&library),
    ));
    let discovery = DiscoveryService::new(
        Arc::clone(&library),
        Arc::clone(&sources),
        Arc::clone(&state),
        Arc::clone(&listens),
    );
    let discovery = match metadata.as_ref() {
        Some(metadata) => discovery.with_metadata(Arc::clone(metadata)),
        None => discovery,
    };
    let subjects: Arc<dyn crate::features::metadata::SubjectSource> =
        Arc::new(LibrarySubjects::new(Arc::clone(&library_service)));

    MusicServices {
        search,
        library: library_service,
        completion,
        playlists,
        queue,
        listen,
        artwork,
        activity,
        scanner,
        sources: Arc::new(SourceQueries::new(sources)),
        library_registry,
        discovery: Arc::new(discovery),
        delivery,
        metadata,
        subjects,
    }
}

struct LibrarySubjects {
    library: Arc<LibraryService>,
}

impl LibrarySubjects {
    fn new(library: Arc<LibraryService>) -> Self {
        Self { library }
    }
}

#[async_trait::async_trait]
impl crate::features::metadata::SubjectSource for LibrarySubjects {
    async fn enrichment_subject(
        &self,
        kind: crate::features::metadata::domain::MetadataEntityKind,
        id: uuid::Uuid,
    ) -> Result<
        Option<crate::features::music_library::domain::EnrichmentSubject>,
        crate::features::metadata::MetadataError,
    > {
        if kind != crate::features::metadata::domain::MetadataEntityKind::Artist {
            return Ok(None);
        }

        self.library
            .enrichment_subject(&crate::features::music_library::domain::ArtistId::from_uuid(id))
            .await
            .map_err(|error| match error {
                super::super::catalogue::CatalogueError::Repository(_) => {
                    crate::features::metadata::MetadataError::Unavailable
                }
                _ => crate::features::metadata::MetadataError::InvalidData,
            })
    }
}
