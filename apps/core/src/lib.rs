use std::sync::Arc;

use axum::Router;

pub mod catalogue;
pub mod database;
pub mod identity;
pub mod media;
pub mod request;
pub mod resolvers;
pub mod tracks;
pub use resolvers as resolver;
pub mod playback;
pub mod playback_stream;

use crate::{
    catalogue::CatalogueService, identity::service::CredentialService, playback::PlaybackService,
    playback_stream::PlaybackStreamService,
};

#[derive(Clone)]
pub struct AppState {
    catalogue: Arc<CatalogueService>,
    credential: Arc<CredentialService>,
    database: Option<Arc<database::Database>>,
    playback: Arc<PlaybackService>,
    playback_stream: Arc<PlaybackStreamService>,
}

impl AppState {
    pub fn new(
        credential: Arc<CredentialService>,
        catalogue: Arc<CatalogueService>,
        playback: Arc<PlaybackService>,
        playback_stream: Arc<PlaybackStreamService>,
    ) -> Self {
        Self {
            credential,
            catalogue,
            database: None,
            playback,
            playback_stream,
        }
    }

    pub fn with_database(
        credential: Arc<CredentialService>,
        catalogue: Arc<CatalogueService>,
        playback: Arc<PlaybackService>,
        playback_stream: Arc<PlaybackStreamService>,
        database: Arc<database::Database>,
    ) -> Self {
        Self {
            credential,
            catalogue,
            database: Some(database),
            playback,
            playback_stream,
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

    pub(crate) fn playback_stream(&self) -> &PlaybackStreamService {
        &self.playback_stream
    }
}

pub fn router(state: AppState) -> Router {
    http::router(state)
}

mod http;
