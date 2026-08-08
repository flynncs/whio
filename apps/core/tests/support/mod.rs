use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
};

use async_trait::async_trait;
use axum::Router;
use whio_core::{
    AppState,
    catalogue::{CatalogueCandidate, CatalogueResolver, CatalogueSearch, CatalogueService},
    request::RequestContext,
    resolver::ResolverError,
    tracks::InMemoryTrackRepository,
};

struct StubResolver {
    responses: Mutex<VecDeque<Result<Vec<CatalogueCandidate>, ResolverError>>>,
}

#[async_trait]
impl CatalogueResolver for StubResolver {
    async fn search(
        &self,
        _search: &CatalogueSearch,
        _context: &RequestContext,
    ) -> Result<Vec<CatalogueCandidate>, ResolverError> {
        self.responses.lock().unwrap().pop_front().unwrap()
    }
}

pub fn app(responses: Vec<Result<Vec<CatalogueCandidate>, ResolverError>>) -> Router {
    let resolver = Arc::new(StubResolver {
        responses: Mutex::new(responses.into()),
    });
    let catalogue = Arc::new(CatalogueService::new(
        resolver,
        Arc::new(InMemoryTrackRepository::default()),
    ));

    whio_core::router(AppState::new(catalogue))
}
