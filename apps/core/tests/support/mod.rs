// fixtures are a toolbox; each suite uses a subset
#![allow(dead_code, unused_imports)]

use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
};

use async_trait::async_trait;
use axum::Router;
use uuid::Uuid;
use whio_core::{
    AppState,
    catalogue::{CatalogueCandidate, CatalogueProvider, CatalogueSearch, CatalogueService},
    compat::SourceTags,
    identity::service::CredentialService,
    playback::{PlaybackProvider, PlaybackService, PlaybackSource, ResolvedPlayback},
    providers::ProviderError,
    request::RequestContext,
    tracks::{InMemoryTrackRepository, ProviderKind},
};

pub mod credentials;
pub use credentials::{
    NATIVE_AUTH_QUERY, credential_service, credential_service_with_known_api_key,
    credential_service_with_known_app_password, credential_service_with_storage_failure,
};

mod media;
pub use media::delivery;

// the single place test suites assemble app state; new services are wired
// here once instead of in every suite
pub fn router(
    credential: Arc<CredentialService>,
    catalogue: Arc<CatalogueService>,
    playback: Arc<PlaybackService>,
) -> Router {
    router_with_source_tags(credential, catalogue, playback, SourceTags::Off)
}

pub fn router_with_source_tags(
    credential: Arc<CredentialService>,
    catalogue: Arc<CatalogueService>,
    playback: Arc<PlaybackService>,
    tags: SourceTags,
) -> Router {
    whio_core::router(AppState::new(credential, catalogue, playback).with_source_tags(tags))
}

struct StubProvider {
    responses: Mutex<VecDeque<Result<Vec<CatalogueCandidate>, ProviderError>>>,
}

#[async_trait]
impl CatalogueProvider for StubProvider {
    async fn search(
        &self,
        _search: &CatalogueSearch,
        _user: Option<&Uuid>,
        _context: &RequestContext,
    ) -> Result<Vec<CatalogueCandidate>, ProviderError> {
        self.responses.lock().unwrap().pop_front().unwrap()
    }
}

#[async_trait]
impl PlaybackProvider for StubProvider {
    fn kind(&self) -> ProviderKind {
        ProviderKind::YoutubeMusic
    }

    async fn resolve(
        &self,
        _source: &PlaybackSource,
        _user: Option<&Uuid>,
        _context: &RequestContext,
    ) -> Result<Option<ResolvedPlayback>, ProviderError> {
        Ok(None)
    }
}

pub fn services(
    responses: Vec<Result<Vec<CatalogueCandidate>, ProviderError>>,
) -> (Arc<CatalogueService>, Arc<PlaybackService>) {
    let provider = Arc::new(StubProvider {
        responses: Mutex::new(responses.into()),
    });
    let track_repository = Arc::new(InMemoryTrackRepository::default());
    let catalogue = Arc::new(CatalogueService::with_providers(
        track_repository.clone(),
        vec![provider.clone() as Arc<dyn CatalogueProvider>],
    ));
    let playback = Arc::new(PlaybackService::new(
        track_repository,
        vec![provider as Arc<dyn PlaybackProvider>],
        delivery(),
    ));
    (catalogue, playback)
}

pub fn app(responses: Vec<Result<Vec<CatalogueCandidate>, ProviderError>>) -> Router {
    let (catalogue, playback) = services(responses);

    router(credential_service(), catalogue, playback)
}
