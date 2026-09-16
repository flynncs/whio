use std::sync::Arc;

use axum::Router;

pub mod catalogue;
pub mod compat;
pub mod database;
pub mod delivery;
pub mod identity;
pub mod media;
pub mod playback;
pub mod providers;
pub mod request;
pub mod tracks;

use crate::{
    catalogue::CatalogueService, identity::service::CredentialService, playback::PlaybackService,
};

#[derive(Clone)]
pub struct AppState {
    catalogue: Arc<CatalogueService>,
    credential: Arc<CredentialService>,
    database: Option<Arc<database::Database>>,
    playback: Arc<PlaybackService>,
}

impl AppState {
    pub fn new(
        credential: Arc<CredentialService>,
        catalogue: Arc<CatalogueService>,
        playback: Arc<PlaybackService>,
    ) -> Self {
        Self {
            credential,
            catalogue,
            database: None,
            playback,
        }
    }

    pub fn with_database(
        credential: Arc<CredentialService>,
        catalogue: Arc<CatalogueService>,
        playback: Arc<PlaybackService>,
        database: Arc<database::Database>,
    ) -> Self {
        Self {
            credential,
            catalogue,
            database: Some(database),
            playback,
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
}

pub fn router(state: AppState) -> Router {
    http::router(state)
}

mod http;
