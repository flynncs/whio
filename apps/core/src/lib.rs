use std::sync::Arc;

use axum::Router;

pub mod catalogue;
pub mod request;
pub mod resolvers;
pub use resolvers as resolver;

use crate::catalogue::CatalogueService;

#[derive(Clone)]
pub struct AppState {
    catalogue: Arc<CatalogueService>,
}

impl AppState {
    pub fn new(catalogue: Arc<CatalogueService>) -> Self {
        Self { catalogue }
    }

    pub(crate) fn catalogue(&self) -> &CatalogueService {
        &self.catalogue
    }
}

pub fn router(state: AppState) -> Router {
    http::router(state)
}

mod http;
