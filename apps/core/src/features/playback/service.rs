use std::{collections::HashMap, sync::Arc};

use uuid::Uuid;

use crate::{
    features::music_library::{
        domain::{ProviderKind, TrackId},
        sources::{SourceQueries, SourceQueryError},
    },
    features::playback::providers::ProviderError,
    features::playback::{
        DeliveryError, DeliveryResult, LocalMedia, PlayableMedia, PlaybackDelivery, PlaybackSource,
        SourceKey, StreamRequest,
    },
    platform::request::RequestContext,
};

use super::{PlaybackProvider, next::NextTracks, prefetch::Prefetch};

#[derive(Debug, thiserror::Error)]
pub enum PlaybackError {
    #[error("track was not found")]
    TrackNotFound,

    #[error("no eligible source was found")]
    SourceNotFound,

    #[error("the source credentials were rejected")]
    CredentialsStale,

    #[error("playback is temporarily unavailable")]
    Unavailable,

    #[error("music library source query failed")]
    Library(#[source] SourceQueryError),

    #[error("playback provider access failed")]
    Internal(#[source] ProviderError),

    #[error("playback provider failed")]
    Provider(#[source] ProviderError),

    #[error("playback delivery failed")]
    Delivery(#[source] DeliveryError),
}

impl PlaybackError {
    fn from_provider(error: ProviderError) -> Self {
        match error {
            ProviderError::SourceNotFound => Self::SourceNotFound,
            ProviderError::CredentialsRejected => Self::CredentialsStale,
            ProviderError::Disabled
            | ProviderError::ProviderUnavailable
            | ProviderError::Request(_) => Self::Unavailable,
            ProviderError::Access => Self::Internal(error),
            error => Self::Provider(error),
        }
    }

    fn allows_fallback(&self) -> bool {
        match self {
            Self::SourceNotFound | Self::CredentialsStale | Self::Unavailable => true,
            Self::Delivery(error) => error.allows_fallback(),
            Self::Provider(_) => true,
            Self::Internal(_) | Self::Library(_) | Self::TrackNotFound => false,
        }
    }
}

#[derive(Debug)]
pub enum ResolvedPlayback {
    Remote(PlayableMedia),
    Local(LocalMedia),
}

const MAX_MEDIA_ATTEMPTS: usize = 3;

#[derive(Clone)]
pub(super) struct SourceResolver {
    providers: HashMap<ProviderKind, Arc<dyn PlaybackProvider>>,
    sources: Arc<SourceQueries>,
}

impl SourceResolver {
    pub(super) fn new(
        sources: Arc<SourceQueries>,
        providers: Vec<Arc<dyn PlaybackProvider>>,
    ) -> Self {
        Self {
            providers: providers
                .into_iter()
                .map(|provider| (provider.kind(), provider))
                .collect(),
            sources,
        }
    }

    pub(super) async fn candidates(
        &self,
        track_id: &TrackId,
    ) -> Result<Vec<PlaybackSource>, PlaybackError> {
        let sources = self
            .sources
            .candidates(track_id)
            .await
            .map_err(PlaybackError::Library)?;
        let mut candidates: Vec<_> = sources
            .into_iter()
            .filter_map(PlaybackSource::from_identity)
            .collect();
        candidates.sort_by(PlaybackSource::compare);
        Ok(candidates)
    }

    pub(super) async fn resolve_source(
        &self,
        source: &PlaybackSource,
        user: Option<&Uuid>,
        context: &RequestContext,
    ) -> Result<Option<ResolvedPlayback>, PlaybackError> {
        let Some(provider) = self.providers.get(&source.kind()) else {
            return Ok(None);
        };
        provider
            .resolve(source, user, context)
            .await
            .map_err(PlaybackError::Provider)
    }
}

pub struct StreamingService {
    resolver: SourceResolver,
    delivery: Arc<dyn PlaybackDelivery>,
    prefetch: Option<Arc<Prefetch>>,
}

impl StreamingService {
    pub fn new(
        sources: Arc<SourceQueries>,
        providers: Vec<Arc<dyn PlaybackProvider>>,
        delivery: Arc<dyn PlaybackDelivery>,
    ) -> Self {
        Self {
            resolver: SourceResolver::new(sources, providers),
            delivery,
            prefetch: None,
        }
    }

    pub fn with_prefetch(mut self, next: Arc<dyn NextTracks>, count: usize) -> Self {
        self.prefetch = Some(Arc::new(Prefetch::new(
            next,
            self.resolver.clone(),
            Arc::clone(&self.delivery),
            count,
        )));
        self
    }

    pub async fn candidates(
        &self,
        track_id: &TrackId,
    ) -> Result<Vec<PlaybackSource>, PlaybackError> {
        self.resolver.candidates(track_id).await
    }

    pub async fn resolve_source(
        &self,
        source: &PlaybackSource,
        user: Option<&Uuid>,
        context: &RequestContext,
    ) -> Result<Option<ResolvedPlayback>, PlaybackError> {
        self.resolver.resolve_source(source, user, context).await
    }

    /// select eligible sources in preference order and deliver the first one
    /// that answers, retrying and falling back as needed.
    pub async fn stream(
        &self,
        track_id: &TrackId,
        user: Option<&Uuid>,
        context: &RequestContext,
        request: StreamRequest,
        range: Option<String>,
    ) -> Result<DeliveryResult, PlaybackError> {
        let mut failure = PlaybackError::TrackNotFound;
        for source in self.candidates(track_id).await? {
            match self
                .try_source(track_id, &source, user, context, &request, range.clone())
                .await
            {
                Ok(Some(delivery)) => return Ok(delivery),
                Ok(None) => continue,
                Err(error) if error.allows_fallback() => failure = error,
                Err(error) => return Err(error),
            }
        }
        Err(failure)
    }

    async fn try_source(
        &self,
        track_id: &TrackId,
        source: &PlaybackSource,
        user: Option<&Uuid>,
        context: &RequestContext,
        request: &StreamRequest,
        range: Option<String>,
    ) -> Result<Option<DeliveryResult>, PlaybackError> {
        let key = SourceKey::new(source);
        if let Some(delivery) = self.delivery.cached(&key, request, range.clone()).await {
            let delivery = delivery.map_err(playback_delivery_error)?;
            self.prefetch_next(track_id, user, context);
            return Ok(Some(delivery));
        }
        for attempt in 1..=MAX_MEDIA_ATTEMPTS {
            let resolved = match self.resolve_source(source, user, context).await {
                Ok(resolved) => resolved,
                Err(PlaybackError::Provider(error)) => {
                    return Err(PlaybackError::from_provider(error));
                }
                Err(error) => return Err(error),
            };
            let Some(resolved) = resolved else {
                return Ok(None);
            };
            match self
                .delivery
                .deliver(&key, &resolved, request, track_id, range.clone())
                .await
            {
                Ok(delivery) => {
                    if matches!(resolved, ResolvedPlayback::Remote(_)) {
                        self.prefetch_next(track_id, user, context);
                    }
                    return Ok(Some(delivery));
                }
                Err(error) if attempt < MAX_MEDIA_ATTEMPTS && error.allows_retry() => continue,
                Err(error) => return Err(playback_delivery_error(error)),
            }
        }
        unreachable!("last attempt returns")
    }

    fn prefetch_next(&self, track_id: &TrackId, user: Option<&Uuid>, context: &RequestContext) {
        let Some(prefetch) = self.prefetch.as_ref() else {
            return;
        };
        prefetch.after(track_id.clone(), user.copied(), context.clone());
    }
}

fn playback_delivery_error(error: DeliveryError) -> PlaybackError {
    match error {
        DeliveryError::Overloaded => PlaybackError::Unavailable,
        error => PlaybackError::Delivery(error),
    }
}
#[cfg(test)]
#[path = "service_tests.rs"]
mod tests;
