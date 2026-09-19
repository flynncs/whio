use std::sync::Arc;

use axum::Router;

pub mod catalogue;
pub mod compat;
pub mod database;
pub mod delivery;
pub mod identity;
pub mod library;
pub mod media;
pub mod playback;
pub mod providers;
pub mod request;
pub mod tracks;

use crate::{
    catalogue::CatalogueService, compat::SourceTags, identity::service::CredentialService,
    library::ScanService, playback::PlaybackService,
};

#[derive(Clone)]
pub struct AppState {
    catalogue: Arc<CatalogueService>,
    credential: Arc<CredentialService>,
    database: Option<Arc<database::Database>>,
    playback: Arc<PlaybackService>,
    scanner: Arc<ScanService>,
    artwork_client: reqwest::Client,
    artwork_host_suffixes: Vec<String>,
    source_tags: SourceTags,
}

impl AppState {
    pub fn new(
        credential: Arc<CredentialService>,
        catalogue: Arc<CatalogueService>,
        playback: Arc<PlaybackService>,
    ) -> Self {
        Self::build(credential, catalogue, playback, None)
    }

    pub fn with_database(
        credential: Arc<CredentialService>,
        catalogue: Arc<CatalogueService>,
        playback: Arc<PlaybackService>,
        database: Arc<database::Database>,
    ) -> Self {
        Self::build(credential, catalogue, playback, Some(database))
    }

    /// host suffixes accepted for public provider artwork. replaces the default
    /// set for both validation and redirects.
    pub fn with_artwork_host_suffixes(mut self, suffixes: Vec<String>) -> Self {
        self.artwork_client = media::client(media::artwork_redirect_policy(suffixes.clone()));
        self.artwork_host_suffixes = suffixes;
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
        catalogue: Arc<CatalogueService>,
        playback: Arc<PlaybackService>,
        database: Option<Arc<database::Database>>,
    ) -> Self {
        let artwork_host_suffixes = media::default_artwork_host_suffixes();
        Self {
            credential,
            catalogue,
            database,
            playback,
            scanner: ScanService::disabled(),
            artwork_client: media::client(media::artwork_redirect_policy(
                artwork_host_suffixes.clone(),
            )),
            artwork_host_suffixes,
            source_tags: SourceTags::Off,
        }
    }

    pub(crate) fn database(&self) -> Option<&database::Database> {
        self.database.as_deref()
    }

    pub(crate) fn catalogue(&self) -> &CatalogueService {
        &self.catalogue
    }

    pub(crate) fn credential(&self) -> &CredentialService {
        &self.credential
    }

    pub(crate) fn playback(&self) -> &PlaybackService {
        &self.playback
    }

    pub(crate) fn scanner(&self) -> &ScanService {
        &self.scanner
    }

    pub(crate) fn artwork_client(&self) -> &reqwest::Client {
        &self.artwork_client
    }

    pub(crate) fn artwork_host_suffixes(&self) -> &[String] {
        &self.artwork_host_suffixes
    }

    pub(crate) fn source_tags(&self) -> SourceTags {
        self.source_tags
    }
}

pub fn router(state: AppState) -> Router {
    http::router(state)
}

mod http;
