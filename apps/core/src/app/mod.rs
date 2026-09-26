pub mod cli;
mod config;
mod startup;
pub use startup::run;

pub mod database;
pub(crate) mod http;
pub mod resolver;

use std::sync::Arc;

use axum::{Router, extract::FromRef};

use crate::{
    facades::SourceTags,
    features::identity::service::CredentialService,
    features::music_library::artwork::ArtworkService,
    features::music_library::catalogue::{CompletionService, LibraryService, SearchService},
    features::music_library::library::ScanService,
    features::music_library::library_state::LibraryStateService,
    features::music_library::playlists::PlaylistService,
    features::playback::StreamingService,
};

#[derive(Clone)]
pub struct AppState {
    artwork: Arc<ArtworkService>,
    search: Arc<SearchService>,
    library: Arc<LibraryService>,
    completion: Arc<CompletionService>,
    playlists: Arc<PlaylistService>,
    credential: Arc<CredentialService>,
    database: Option<Arc<database::Database>>,
    library_state: Arc<LibraryStateService>,
    streaming: Arc<StreamingService>,
    scanner: Arc<ScanService>,
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

        Self {
            artwork: Arc::new(ArtworkService::disabled()),
            credential,
            search,
            library,
            completion,
            playlists,
            database,
            library_state: Arc::new(LibraryStateService::disabled()),
            streaming,
            scanner: ScanService::disabled(),
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
            completion: Arc::clone(&state.completion),
            playlists: Arc::clone(&state.playlists),
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
            search: Arc::clone(&state.search),
            library: Arc::clone(&state.library),
            completion: Arc::clone(&state.completion),
            playlists: Arc::clone(&state.playlists),
            source_tags: state.source_tags,
        }
    }
}

pub fn router(state: AppState) -> Router {
    http::router(state)
}
