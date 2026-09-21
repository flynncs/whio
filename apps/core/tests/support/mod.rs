// fixtures are a toolbox; each suite uses a subset
#![allow(dead_code, unused_imports)]

use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
};

use async_trait::async_trait;
use axum::Router;
use uuid::Uuid;
use whio_core::{
    AppState,
    artwork::ArtworkService,
    catalogue::{
        CatalogueCandidate, CatalogueProvider, CatalogueSearch, CompletionService, LibraryService,
        SearchService,
    },
    compat::SourceTags,
    identity::service::CredentialService,
    playback::{PlaybackProvider, PlaybackSource, ResolvedPlayback, StreamingService},
    playlists::PlaylistService,
    providers::ProviderError,
    request::RequestContext,
    tracks::{
        CatalogueWrite, DuplicateReport, InMemoryTrackRepository, LibraryRead, ProviderKind,
        SourceLookup, StateStore,
    },
};

pub mod credentials;
pub use credentials::{
    NATIVE_AUTH_QUERY, credential_service, credential_service_with_known_api_key,
    credential_service_with_known_app_password, credential_service_with_storage_failure,
};

mod media;
pub use media::{delivery, pictured_flac};

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
    whio_core::router(AppState::new(credential, search, library, playback).with_source_tags(tags))
}

pub fn router_with_artwork(
    credential: Arc<CredentialService>,
    search: Arc<SearchService>,
    library: Arc<LibraryService>,
    playback: Arc<StreamingService>,
    artwork: Arc<ArtworkService>,
) -> Router {
    whio_core::router(AppState::new(credential, search, library, playback).with_artwork(artwork))
}

pub fn router_with_completion(
    credential: Arc<CredentialService>,
    search: Arc<SearchService>,
    library: Arc<LibraryService>,
    completion: Arc<CompletionService>,
    playback: Arc<StreamingService>,
) -> Router {
    whio_core::router(
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
    whio_core::router(
        AppState::new(credential, search, library, playback).with_playlists(playlists),
    )
}

struct StubProvider {
    responses: Mutex<VecDeque<Result<Vec<CatalogueCandidate>, ProviderError>>>,
}

#[async_trait]
impl CatalogueProvider for StubProvider {
    async fn search(
        &self,
        _search: &CatalogueSearch,
        _user: Option<&Uuid>,
        _context: &RequestContext,
    ) -> Result<Vec<CatalogueCandidate>, ProviderError> {
        self.responses.lock().unwrap().pop_front().unwrap()
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

// one in-memory store, handed out as the narrow port each service declares
pub struct StorePorts {
    pub store: Arc<InMemoryTrackRepository>,
}

impl StorePorts {
    pub fn new() -> Self {
        Self {
            store: Arc::new(InMemoryTrackRepository::default()),
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

    pub fn streaming(&self, providers: Vec<Arc<dyn PlaybackProvider>>) -> Arc<StreamingService> {
        Arc::new(StreamingService::new(self.sources(), providers, delivery()))
    }
}

impl Default for StorePorts {
    fn default() -> Self {
        Self::new()
    }
}

pub fn services(
    responses: Vec<Result<Vec<CatalogueCandidate>, ProviderError>>,
) -> (
    Arc<SearchService>,
    Arc<LibraryService>,
    Arc<StreamingService>,
) {
    let provider = Arc::new(StubProvider {
        responses: Mutex::new(responses.into()),
    });
    let ports = StorePorts::new();
    let (search, library) =
        ports.catalogue_service(vec![provider.clone() as Arc<dyn CatalogueProvider>]);
    let playback = ports.streaming(vec![provider as Arc<dyn PlaybackProvider>]);
    (search, library, playback)
}

pub fn app(responses: Vec<Result<Vec<CatalogueCandidate>, ProviderError>>) -> Router {
    let (search, library, playback) = services(responses);

    router(credential_service(), search, library, playback)
}
