use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
};

use async_trait::async_trait;
use axum::Router;
use whio_core::{
    AppState,
    catalogue::{CatalogueCandidate, CatalogueResolver, CatalogueSearch, CatalogueService},
    playback::{PlayableMedia, PlaybackResolver, PlaybackService},
    request::RequestContext,
    resolver::ResolverError,
    tracks::{InMemoryTrackRepository, SourceIdentity},
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

#[async_trait]
impl PlaybackResolver for StubResolver {
    async fn resolve(
        &self,
        _source: &SourceIdentity,
        _context: &RequestContext,
    ) -> Result<PlayableMedia, ResolverError> {
        Err(ResolverError::Internal)
    }
}

pub fn app(responses: Vec<Result<Vec<CatalogueCandidate>, ResolverError>>) -> Router {
    let resolver = Arc::new(StubResolver {
        responses: Mutex::new(responses.into()),
    });
    let track_repository = Arc::new(InMemoryTrackRepository::default());
    let catalogue = Arc::new(CatalogueService::new(
        resolver.clone(),
        track_repository.clone(),
    ));
    let playback = Arc::new(PlaybackService::new(resolver, track_repository));

    whio_core::router(AppState::new(catalogue, playback))
}
