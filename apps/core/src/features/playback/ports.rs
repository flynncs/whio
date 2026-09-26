use async_trait::async_trait;
use uuid::Uuid;

use crate::{
    features::music_library::domain::ProviderKind,
    features::playback::providers::ProviderError,
    features::playback::{DeliveryError, DeliveryResult, PlaybackSource, ResolvedPlayback},
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
        resolved: &ResolvedPlayback,
        client_range: Option<String>,
    ) -> Result<DeliveryResult, DeliveryError>;
}
