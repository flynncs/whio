// fixtures are a toolbox; each suite uses a subset
#![allow(dead_code, unused_imports)]

use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
};

use async_trait::async_trait;
use axum::Router;
use whio_core::{
    AppState,
    catalogue::{CatalogueCandidate, CatalogueResolver, CatalogueSearch, CatalogueService},
    identity::service::CredentialService,
    playback::{PlayableMedia, PlaybackResolver, PlaybackService},
    playback_stream::PlaybackStreamService,
    request::RequestContext,
    resolver::ResolverError,
    tracks::{InMemoryTrackRepository, SourceIdentity},
};

pub mod credentials;
pub use credentials::{
    credential_service, credential_service_with_known_api_key,
    credential_service_with_known_app_password,
};

mod media;
pub use media::playback_stream;

// the single place test suites assemble app state; new services are wired
// here once instead of in every suite
pub fn router(
    credential: Arc<CredentialService>,
    catalogue: Arc<CatalogueService>,
    playback: Arc<PlaybackService>,
    playback_stream: Arc<PlaybackStreamService>,
) -> Router {
    whio_core::router(AppState::new(
        credential,
        catalogue,
        playback,
        playback_stream,
    ))
}

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
    let playback_stream = playback_stream(Arc::clone(&playback));

    router(credential_service(), catalogue, playback, playback_stream)
}
