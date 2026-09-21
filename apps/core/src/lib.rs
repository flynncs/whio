use std::sync::Arc;

use axum::Router;

pub mod artwork;
pub mod catalogue;
pub mod compat;
pub mod database;
pub mod delivery;
pub mod identity;
pub mod library;
pub mod library_state;
pub mod media;
pub mod playback;
pub mod providers;
pub mod request;
mod source_path;
pub mod tracks;

use crate::{
    artwork::ArtworkService,
    catalogue::{LibraryService, SearchService},
    compat::SourceTags,
    identity::service::CredentialService,
    library::ScanService,
    library_state::LibraryStateService,
    playback::StreamingService,
};

#[derive(Clone)]
pub struct AppState {
    artwork: Arc<ArtworkService>,
    search: Arc<SearchService>,
    library: Arc<LibraryService>,
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
        Self {
            artwork: Arc::new(ArtworkService::disabled()),
            credential,
            search,
            library,
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

    pub(crate) fn search(&self) -> &SearchService {
        &self.search
    }

    pub(crate) fn library(&self) -> &LibraryService {
        &self.library
    }

    pub(crate) fn credential(&self) -> &CredentialService {
        &self.credential
    }

    pub(crate) fn streaming(&self) -> &StreamingService {
        &self.streaming
    }

    pub(crate) fn artwork(&self) -> &ArtworkService {
        &self.artwork
    }

    pub(crate) fn library_state(&self) -> &LibraryStateService {
        &self.library_state
    }

    pub(crate) fn scanner(&self) -> &ScanService {
        &self.scanner
    }

    pub(crate) fn source_tags(&self) -> SourceTags {
        self.source_tags
    }
}

pub fn router(state: AppState) -> Router {
    http::router(state)
}

mod http;
