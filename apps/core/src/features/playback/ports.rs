use async_trait::async_trait;
use uuid::Uuid;

use crate::{
    features::music_library::domain::ProviderKind,
    features::playback::providers::ProviderError,
    features::playback::{
        DeliveryError, DeliveryResult, PlaybackSource, ResolvedPlayback, SourceKey,
    },
    platform::request::RequestContext,
};

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
        client_range: Option<String>,
    ) -> Result<DeliveryResult, DeliveryError>;

    async fn cached(
        &self,
        key: &SourceKey,
        client_range: Option<String>,
    ) -> Option<Result<DeliveryResult, DeliveryError>>;

    fn is_cached(&self, key: &SourceKey) -> bool;

    async fn warm(&self, key: &SourceKey, resolved: &ResolvedPlayback)
    -> Result<(), DeliveryError>;
}
