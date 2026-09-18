use async_trait::async_trait;
use uuid::Uuid;

use crate::{
    playback::{DeliveryError, DeliveryResult, PlaybackSource, ResolvedPlayback},
    providers::ProviderError,
    request::RequestContext,
    tracks::{ArtworkRef, ProviderKind},
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

    async fn credential_rejected(
        &self,
        _source: &PlaybackSource,
        _user: Option<&Uuid>,
    ) -> ProviderError {
        ProviderError::ProviderUnavailable
    }

    async fn resolve_artwork(
        &self,
        _source: &PlaybackSource,
        _artwork: &ArtworkRef,
        _user: Option<&Uuid>,
        _context: &RequestContext,
    ) -> Result<Option<String>, ProviderError> {
        Ok(None)
    }
}

#[async_trait]
pub trait PlaybackDelivery: Send + Sync {
    async fn deliver(
        &self,
        resolved: &ResolvedPlayback,
        client_range: Option<String>,
    ) -> Result<DeliveryResult, DeliveryError>;
}
