use async_trait::async_trait;
use uuid::Uuid;

use crate::{
    features::music_library::domain::{ProviderKind, TrackId},
    features::playback::providers::ProviderError,
    features::playback::{
        DeliveryError, DeliveryResult, PlaybackSource, ResolvedPlayback, SourceKey, StreamRequest,
    },
    platform::request::RequestContext,
};

#[derive(Debug, thiserror::Error)]
pub enum TrackDurationError {
    #[error("music library read failed")]
    Library,
}

#[async_trait]
pub trait TrackDurations: Send + Sync {
    async fn duration_ms(&self, track: &TrackId) -> Result<Option<u64>, TrackDurationError>;
}

#[async_trait]
pub trait PlaybackProvider: Send + Sync {
    fn kind(&self) -> ProviderKind;

    async fn resolve(
        &self,
        source: &PlaybackSource,
        user: Option<&Uuid>,
        context: &RequestContext,
    ) -> Result<Option<ResolvedPlayback>, ProviderError>;
}

#[async_trait]
pub trait PlaybackDelivery: Send + Sync {
    async fn deliver(
        &self,
        key: &SourceKey,
        resolved: &ResolvedPlayback,
        request: &StreamRequest,
        track: &TrackId,
        client_range: Option<String>,
    ) -> Result<DeliveryResult, DeliveryError>;

    async fn cached(
        &self,
        key: &SourceKey,
        request: &StreamRequest,
        client_range: Option<String>,
    ) -> Option<Result<DeliveryResult, DeliveryError>>;

    fn is_cached(&self, key: &SourceKey) -> bool;

    async fn warm(&self, key: &SourceKey, resolved: &ResolvedPlayback)
    -> Result<(), DeliveryError>;
}
