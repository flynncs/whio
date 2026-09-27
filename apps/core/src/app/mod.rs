pub mod cli;
mod config;
mod startup;
pub use startup::run;

pub mod database;
pub(crate) mod http;
pub mod resolver;
pub mod worker;

use std::sync::Arc;

use axum::{Router, extract::FromRef};

use crate::{
    facades::SourceTags,
    features::identity::service::CredentialService,
    features::music_library::artwork::ArtworkService,
    features::music_library::catalogue::{CompletionService, LibraryService, SearchService},
    features::music_library::library::{LibraryRegistry, ScanService},
    features::music_library::library_state::LibraryStateService,
    features::music_library::playlists::PlaylistService,
    features::music_library::queue::RestoreQueueService,
    features::playback::StreamingService,
};

#[derive(Clone)]
pub struct AppState {
    artwork: Arc<ArtworkService>,
    search: Arc<SearchService>,
    library: Arc<LibraryService>,
    completion: Arc<CompletionService>,
    playlists: Arc<PlaylistService>,
    queue: Arc<RestoreQueueService>,
    credential: Arc<CredentialService>,
    database: Option<Arc<database::Database>>,
    library_state: Arc<LibraryStateService>,
    streaming: Arc<StreamingService>,
    scanner: Arc<ScanService>,
    library_registry: Arc<LibraryRegistry>,
    source_tags: SourceTags,
}

impl AppState {
    pub fn new(
        credential: Arc<CredentialService>,
        search: Arc<SearchService>,
        library: Arc<LibraryService>,
        streaming: Arc<StreamingService>,
    ) -> Self {
        Self::build(credential, search, library, streaming, None)
    }

    pub fn with_database(
        credential: Arc<CredentialService>,
        search: Arc<SearchService>,
        library: Arc<LibraryService>,
        streaming: Arc<StreamingService>,
        database: Arc<database::Database>,
    ) -> Self {
        Self::build(credential, search, library, streaming, Some(database))
    }

    pub fn with_artwork(mut self, artwork: Arc<ArtworkService>) -> Self {
        self.artwork = artwork;
        self
    }

    pub fn with_library_state(mut self, library_state: Arc<LibraryStateService>) -> Self {
        self.library_state = library_state;
        self
    }

    pub fn with_completion(mut self, completion: Arc<CompletionService>) -> Self {
        self.completion = completion;
        self
    }

    pub fn with_queue(mut self, queue: Arc<RestoreQueueService>) -> Self {
        self.queue = queue;
        self
    }

    pub fn with_playlists(mut self, playlists: Arc<PlaylistService>) -> Self {
        self.playlists = playlists;
        self
    }

    pub fn with_source_tags(mut self, tags: SourceTags) -> Self {
        self.source_tags = tags;
        self
    }

    pub fn with_scanner(mut self, scanner: Arc<ScanService>) -> Self {
        self.scanner = scanner;
        self
    }

    pub fn with_library_registry(mut self, library_registry: Arc<LibraryRegistry>) -> Self {
        self.library_registry = library_registry;
        self
    }

    pub(crate) fn library_registry(&self) -> &Arc<LibraryRegistry> {
        &self.library_registry
    }

    pub fn scanner_handle(&self) -> Arc<ScanService> {
        Arc::clone(&self.scanner)
    }

    fn build(
        credential: Arc<CredentialService>,
        search: Arc<SearchService>,
        library: Arc<LibraryService>,
        streaming: Arc<StreamingService>,
        database: Option<Arc<database::Database>>,
    ) -> Self {
        let completion = Arc::new(CompletionService::disabled(Arc::clone(&library)));
        let playlists = Arc::new(PlaylistService::disabled(Arc::clone(&library)));
        let queue = Arc::new(RestoreQueueService::disabled());

        Self {
            artwork: Arc::new(ArtworkService::disabled()),
            credential,
            search,
            library,
            completion,
            playlists,
            queue,
            database,
            library_state: Arc::new(LibraryStateService::disabled()),
            streaming,
            scanner: ScanService::disabled(),
            library_registry: Arc::new(LibraryRegistry::disabled()),
            source_tags: SourceTags::Off,
        }
    }

    pub(crate) fn database(&self) -> Option<&database::Database> {
        self.database.as_deref()
    }
}

impl FromRef<AppState> for crate::features::music_library::api::native::NativeState {
    fn from_ref(state: &AppState) -> Self {
        Self {
            search: Arc::clone(&state.search),
            library: Arc::clone(&state.library),
            artwork: Arc::clone(&state.artwork),
        }
    }
}

impl FromRef<AppState> for crate::features::music_library::api::playlists::PlaylistsState {
    fn from_ref(state: &AppState) -> Self {
        Self {
            playlists: Arc::clone(&state.playlists),
            library: Arc::clone(&state.library),
        }
    }
}

impl FromRef<AppState> for crate::features::music_library::api::queue::QueueState {
    fn from_ref(state: &AppState) -> Self {
        Self {
            queue: Arc::clone(&state.queue),
            library: Arc::clone(&state.library),
        }
    }
}

impl FromRef<AppState> for crate::features::playback::api::native::NativeState {
    fn from_ref(state: &AppState) -> Self {
        Self(Arc::clone(&state.streaming))
    }
}

impl FromRef<AppState> for crate::features::identity::api::native::NativeState {
    fn from_ref(state: &AppState) -> Self {
        Self(Arc::clone(&state.credential))
    }
}

impl FromRef<AppState> for crate::facades::subsonic::state::SubsonicState {
    fn from_ref(state: &AppState) -> Self {
        Self {
            credential: Arc::clone(&state.credential),
            search: Arc::clone(&state.search),
            library: Arc::clone(&state.library),
            libraries: Arc::clone(&state.library_registry),
            completion: Arc::clone(&state.completion),
            playlists: Arc::clone(&state.playlists),
            queue: Arc::clone(&state.queue),
            artwork: Arc::clone(&state.artwork),
            library_state: Arc::clone(&state.library_state),
            scanner: Arc::clone(&state.scanner),
            streaming: Arc::clone(&state.streaming),
            source_tags: state.source_tags,
        }
    }
}

impl FromRef<AppState> for crate::facades::navidrome::state::NavidromeState {
    fn from_ref(state: &AppState) -> Self {
        Self {
            credential: Arc::clone(&state.credential),
            library: Arc::clone(&state.library),
            libraries: Arc::clone(&state.library_registry),
            completion: Arc::clone(&state.completion),
            playlists: Arc::clone(&state.playlists),
            source_tags: state.source_tags,
        }
    }
}

impl FromRef<AppState> for crate::facades::navidrome::state::NavidromeQueueState {
    fn from_ref(state: &AppState) -> Self {
        Self {
            navidrome:
                <crate::facades::navidrome::state::NavidromeState as FromRef<AppState>>::from_ref(
                    state,
                ),
            queue: Arc::clone(&state.queue),
        }
    }
}

pub fn router(state: AppState) -> Router {
    http::router(state)
}
