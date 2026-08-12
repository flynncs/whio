use std::sync::Arc;

use axum::Router;

pub mod catalogue;
pub(crate) mod media;
pub mod request;
pub mod resolvers;
pub mod tracks;
pub use resolvers as resolver;
pub mod playback;

use crate::{catalogue::CatalogueService, playback::PlaybackService};

#[derive(Clone)]
pub struct AppState {
    catalogue: Arc<CatalogueService>,
    playback: Arc<PlaybackService>,
}

impl AppState {
    pub fn new(catalogue: Arc<CatalogueService>, playback: Arc<PlaybackService>) -> Self {
        Self {
            catalogue,
            playback,
        }
    }

    pub(crate) fn catalogue(&self) -> &CatalogueService {
        &self.catalogue
    }

    pub(crate) fn playback(&self) -> &PlaybackService {
        &self.playback
    }
}

pub fn router(state: AppState) -> Router {
    http::router(state)
}

mod http;
