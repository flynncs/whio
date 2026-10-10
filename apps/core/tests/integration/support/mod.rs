// fixtures are a toolbox; each suite uses a subset
#![allow(dead_code, unused_imports)]

use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
};

use async_trait::async_trait;
use axum::Router;
use sqlx::PgPool;
use uuid::Uuid;
use whio_core::{
    app::AppState,
    facades::SourceTags,
    features::identity::service::CredentialService,
    features::music_library::artwork::ArtworkService,
    features::music_library::catalogue::{
        CatalogueCandidate, CatalogueProvider, CatalogueSearch, CompletionService, LibraryService,
        ProviderError as CatalogueProviderError, SearchService,
    },
    features::music_library::domain::ProviderKind,
    features::music_library::playlists::PlaylistService,
    features::music_library::queue::RestoreQueueService,
    features::music_library::sources::SourceQueries,
    features::music_library::test_support::{
        CatalogueWrite, DuplicateReport, LibraryRead, PostgresTrackRepository, SourceLookup,
        StateStore,
    },
    features::playback::providers::ProviderError,
    features::playback::{PlaybackProvider, PlaybackSource, ResolvedPlayback, StreamingService},
    platform::request::RequestContext,
};

pub const TEST_REQUEST_ID: &str = "550e8400-e29b-41d4-a716-446655440000";

pub mod credentials;
pub use credentials::{
    NATIVE_AUTH_QUERY, credential_service, credential_service_with_known_api_key,
    credential_service_with_known_app_password,
    credential_service_with_known_app_password_and_throttle,
    credential_service_with_storage_failure,
};

mod media;
pub use media::{delivery, pictured_flac};

pub mod worker;

// the single place test suites assemble app state; new services are wired
// here once instead of in every suite
pub fn router(
    credential: Arc<CredentialService>,
    search: Arc<SearchService>,
    library: Arc<LibraryService>,
    playback: Arc<StreamingService>,
) -> Router {
    router_with_source_tags(credential, search, library, playback, SourceTags::Off)
}

pub fn router_with_source_tags(
    credential: Arc<CredentialService>,
    search: Arc<SearchService>,
    library: Arc<LibraryService>,
    playback: Arc<StreamingService>,
    tags: SourceTags,
) -> Router {
    whio_core::app::router(
        AppState::new(credential, search, library, playback).with_source_tags(tags),
    )
}

pub fn router_with_artwork(
    credential: Arc<CredentialService>,
    search: Arc<SearchService>,
    library: Arc<LibraryService>,
    playback: Arc<StreamingService>,
    artwork: Arc<ArtworkService>,
) -> Router {
    whio_core::app::router(
        AppState::new(credential, search, library, playback).with_artwork(artwork),
    )
}

pub fn router_with_completion(
    credential: Arc<CredentialService>,
    search: Arc<SearchService>,
    library: Arc<LibraryService>,
    completion: Arc<CompletionService>,
    playback: Arc<StreamingService>,
) -> Router {
    whio_core::app::router(
        AppState::new(credential, search, library, playback).with_completion(completion),
    )
}

pub fn router_with_playlists(
    credential: Arc<CredentialService>,
    search: Arc<SearchService>,
    library: Arc<LibraryService>,
    playlists: Arc<PlaylistService>,
    playback: Arc<StreamingService>,
) -> Router {
    whio_core::app::router(
        AppState::new(credential, search, library, playback).with_playlists(playlists),
    )
}

pub fn router_with_queue(
    credential: Arc<CredentialService>,
    search: Arc<SearchService>,
    library: Arc<LibraryService>,
    queue: Arc<RestoreQueueService>,
    playback: Arc<StreamingService>,
) -> Router {
    whio_core::app::router(AppState::new(credential, search, library, playback).with_queue(queue))
}

struct StubProvider {
    responses: Mutex<VecDeque<Result<Vec<CatalogueCandidate>, CatalogueProviderError>>>,
}

#[async_trait]
impl CatalogueProvider for StubProvider {
    async fn search_tracks(
        &self,
        _search: &CatalogueSearch,
        _context: &RequestContext,
    ) -> Result<Vec<CatalogueCandidate>, CatalogueProviderError> {
        self.responses
            .lock()
            .unwrap()
            .pop_front()
            .expect("controlled provider has a response for each search call")
    }
}

#[async_trait]
impl PlaybackProvider for StubProvider {
    fn kind(&self) -> ProviderKind {
        ProviderKind::YoutubeMusic
    }

    async fn resolve(
        &self,
        _source: &PlaybackSource,
        _user: Option<&Uuid>,
        _context: &RequestContext,
    ) -> Result<Option<ResolvedPlayback>, ProviderError> {
        Ok(None)
    }
}

// a real isolated SQL store backs storage-dependent HTTP fixtures
pub struct StorePorts {
    pub store: Arc<PostgresTrackRepository>,
}

impl StorePorts {
    pub fn new(pool: PgPool) -> Self {
        Self {
            store: Arc::new(PostgresTrackRepository::new(pool)),
        }
    }

    pub fn library(&self) -> Arc<dyn LibraryRead> {
        Arc::clone(&self.store) as Arc<dyn LibraryRead>
    }

    pub fn catalogue(&self) -> Arc<dyn CatalogueWrite> {
        Arc::clone(&self.store) as Arc<dyn CatalogueWrite>
    }

    pub fn sources(&self) -> Arc<dyn SourceLookup> {
        Arc::clone(&self.store) as Arc<dyn SourceLookup>
    }

    pub fn state(&self) -> Arc<dyn StateStore> {
        Arc::clone(&self.store) as Arc<dyn StateStore>
    }

    pub fn reports(&self) -> Arc<dyn DuplicateReport> {
        Arc::clone(&self.store) as Arc<dyn DuplicateReport>
    }

    pub fn catalogue_service(
        &self,
        providers: Vec<Arc<dyn CatalogueProvider>>,
    ) -> (Arc<SearchService>, Arc<LibraryService>) {
        let search = Arc::new(SearchService::with_providers(
            self.library(),
            self.catalogue(),
            self.sources(),
            self.state(),
            providers,
        ));
        let library = Arc::new(LibraryService::new(
            self.library(),
            self.sources(),
            self.state(),
        ));

        (search, library)
    }

    pub fn bare_catalogue(&self) -> (Arc<SearchService>, Arc<LibraryService>) {
        let search = Arc::new(SearchService::new(
            self.library(),
            self.catalogue(),
            self.sources(),
            self.state(),
        ));
        let library = Arc::new(LibraryService::new(
            self.library(),
            self.sources(),
            self.state(),
        ));

        (search, library)
    }

    pub fn source_queries(&self) -> Arc<SourceQueries> {
        Arc::new(SourceQueries::new(self.sources()))
    }

    pub fn streaming(&self, providers: Vec<Arc<dyn PlaybackProvider>>) -> Arc<StreamingService> {
        Arc::new(StreamingService::new(
            self.source_queries(),
            providers,
            delivery(),
        ))
    }
}

// Each HTTP test supplies a disposable database through #[sqlx::test].

pub fn services(
    pool: PgPool,
    responses: Vec<Result<Vec<CatalogueCandidate>, CatalogueProviderError>>,
) -> (
    Arc<SearchService>,
    Arc<LibraryService>,
    Arc<StreamingService>,
) {
    let has_catalogue_responses = !responses.is_empty();
    let provider = Arc::new(StubProvider {
        responses: Mutex::new(responses.into()),
    });
    let ports = StorePorts::new(pool);
    let (search, library) = if has_catalogue_responses {
        ports.catalogue_service(vec![Arc::clone(&provider) as Arc<dyn CatalogueProvider>])
    } else {
        ports.bare_catalogue()
    };
    let playback = ports.streaming(vec![provider as Arc<dyn PlaybackProvider>]);
    (search, library, playback)
}

pub fn app(
    pool: PgPool,
    responses: Vec<Result<Vec<CatalogueCandidate>, CatalogueProviderError>>,
) -> Router {
    let (search, library, playback) = services(pool, responses);

    router(credential_service(), search, library, playback)
}
