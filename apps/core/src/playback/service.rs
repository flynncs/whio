use std::{collections::HashMap, sync::Arc};

use uuid::Uuid;

use crate::{
    playback::{DeliveryError, DeliveryResult, PlayableMedia, PlaybackDelivery, PlaybackSource},
    providers::ProviderError,
    request::RequestContext,
    tracks::{ArtworkRef, ProviderKind, TrackId, TrackRepository, TrackRepositoryError},
};

use super::PlaybackProvider;

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

    #[error("track repository failed")]
    Repository(#[source] TrackRepositoryError),

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
            error => Self::Provider(error),
        }
    }

    fn allows_fallback(&self) -> bool {
        match self {
            Self::SourceNotFound | Self::CredentialsStale | Self::Unavailable => true,
            Self::Delivery(error) => error.allows_fallback(),
            Self::Provider(error) => !matches!(error, ProviderError::Access),
            Self::Repository(_) | Self::TrackNotFound => false,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeliveryStrategy {
    Direct,
    Spool,
}

pub enum ArtworkLocation {
    /// validated against the registered upstream origin that produced it.
    Registered(String),
    /// a public URL that must be validated against the artwork host suffixes.
    Public(String),
}

pub struct ResolvedPlayback {
    pub media: PlayableMedia,
    pub strategy: DeliveryStrategy,
}

const MAX_MEDIA_ATTEMPTS: usize = 3;

pub struct PlaybackService {
    providers: HashMap<ProviderKind, Arc<dyn PlaybackProvider>>,
    track_repository: Arc<dyn TrackRepository>,
    delivery: Arc<dyn PlaybackDelivery>,
}

impl PlaybackService {
    pub fn new(
        track_repository: Arc<dyn TrackRepository>,
        providers: Vec<Arc<dyn PlaybackProvider>>,
        delivery: Arc<dyn PlaybackDelivery>,
    ) -> Self {
        Self {
            providers: providers
                .into_iter()
                .map(|provider| (provider.kind(), provider))
                .collect(),
            track_repository,
            delivery,
        }
    }

    pub async fn candidates(
        &self,
        track_id: &TrackId,
    ) -> Result<Vec<PlaybackSource>, PlaybackError> {
        let sources = self
            .track_repository
            .find_sources(track_id)
            .await
            .map_err(PlaybackError::Repository)?;
        let mut candidates: Vec<_> = sources
            .into_iter()
            .filter_map(PlaybackSource::from_identity)
            .collect();
        candidates.sort_by(PlaybackSource::compare);
        Ok(candidates)
    }

    /// where a track's cover art lives. a public URL still needs host
    /// validation; a registered URL was already validated against the origin it
    /// was registered under.
    pub async fn artwork(
        &self,
        track_id: &TrackId,
        user: Option<&Uuid>,
        context: &RequestContext,
    ) -> Result<Option<ArtworkLocation>, PlaybackError> {
        let Some((source, artwork)) = self
            .track_repository
            .find_artwork(track_id)
            .await
            .map_err(PlaybackError::Repository)?
        else {
            return Ok(None);
        };

        match &artwork {
            ArtworkRef::Url(url) => Ok(Some(ArtworkLocation::Public(url.clone()))),
            ArtworkRef::Upstream(_) => {
                let Some(source) = PlaybackSource::from_identity(source) else {
                    return Ok(None);
                };
                let Some(provider) = self.providers.get(&source.kind()) else {
                    return Ok(None);
                };
                provider
                    .resolve_artwork(&source, &artwork, user, context)
                    .await
                    .map_err(PlaybackError::Provider)
                    .map(|url| url.map(ArtworkLocation::Registered))
            }
        }
    }

    pub async fn resolve_source(
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

    /// select eligible sources in preference order and deliver the first one
    /// that answers, retrying and falling back as needed.
    pub async fn stream(
        &self,
        track_id: &TrackId,
        user: Option<&Uuid>,
        context: &RequestContext,
        range: Option<String>,
    ) -> Result<DeliveryResult, PlaybackError> {
        let mut failure = PlaybackError::TrackNotFound;
        for source in self.candidates(track_id).await? {
            match self.try_source(&source, user, context, range.clone()).await {
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
        source: &PlaybackSource,
        user: Option<&Uuid>,
        context: &RequestContext,
        range: Option<String>,
    ) -> Result<Option<DeliveryResult>, PlaybackError> {
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
            match self.delivery.deliver(&resolved, range.clone()).await {
                Ok(delivery) => return Ok(Some(delivery)),
                Err(error) if attempt < MAX_MEDIA_ATTEMPTS && error.allows_retry() => continue,
                Err(DeliveryError::UpstreamUnauthorized) => {
                    return Err(self.credential_rejected(source, user).await);
                }
                Err(DeliveryError::UpstreamNotFound) => {
                    return Err(PlaybackError::SourceNotFound);
                }
                Err(DeliveryError::UpstreamUnavailable | DeliveryError::Overloaded) => {
                    return Err(PlaybackError::Unavailable);
                }
                Err(error) => return Err(PlaybackError::Delivery(error)),
            }
        }
        unreachable!("last attempt returns")
    }

    pub async fn credential_rejected(
        &self,
        source: &PlaybackSource,
        user: Option<&Uuid>,
    ) -> PlaybackError {
        let Some(provider) = self.providers.get(&source.kind()) else {
            return PlaybackError::Unavailable;
        };
        match provider.credential_rejected(source, user).await {
            ProviderError::CredentialsRejected => PlaybackError::CredentialsStale,
            error => PlaybackError::Provider(error),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{HashMap, VecDeque};

    use async_trait::async_trait;
    use axum::{
        body::Body,
        http::{HeaderMap, StatusCode},
    };
    use parking_lot::Mutex;

    use super::*;
    use crate::{
        identity::{
            secrets::CredentialKey,
            upstream::{MemoryUpstreamLinkStore, UpstreamLinkStore},
        },
        media::MediaFetchError,
        playback::{MediaMetadata, PlayableMedia, PlaybackUrl},
        providers::{
            NavidromeAdapter, UpstreamCandidate, UpstreamError, UpstreamTarget, UpstreamTransport,
        },
        request::RequestId,
        tracks::{
            InMemoryTrackRepository, ProviderId, SourceIdentity, SourceScope, Track, TrackMetadata,
        },
    };

    struct QueueProvider {
        responses: Mutex<VecDeque<Result<PlayableMedia, ProviderError>>>,
        seen: Mutex<Vec<PlaybackSource>>,
    }

    #[async_trait]
    impl PlaybackProvider for QueueProvider {
        fn kind(&self) -> ProviderKind {
            ProviderKind::YoutubeMusic
        }

        async fn resolve(
            &self,
            source: &PlaybackSource,
            _user: Option<&Uuid>,
            _context: &RequestContext,
        ) -> Result<Option<ResolvedPlayback>, ProviderError> {
            self.seen.lock().push(source.clone());
            self.responses
                .lock()
                .pop_front()
                .expect("unexpected resolve call")
                .map(|media| {
                    Some(ResolvedPlayback {
                        media,
                        strategy: DeliveryStrategy::Direct,
                    })
                })
        }
    }

    #[derive(Default)]
    struct FakeDelivery {
        outcomes: Mutex<VecDeque<Result<(), DeliveryError>>>,
        attempts: Mutex<usize>,
    }

    impl FakeDelivery {
        fn with_outcomes(outcomes: Vec<Result<(), DeliveryError>>) -> Self {
            Self {
                outcomes: Mutex::new(outcomes.into()),
                attempts: Mutex::new(0),
            }
        }
    }

    #[async_trait]
    impl PlaybackDelivery for FakeDelivery {
        async fn deliver(
            &self,
            _resolved: &ResolvedPlayback,
            _client_range: Option<String>,
        ) -> Result<DeliveryResult, DeliveryError> {
            *self.attempts.lock() += 1;
            match self
                .outcomes
                .lock()
                .pop_front()
                .expect("unexpected deliver call")
            {
                Ok(()) => Ok(DeliveryResult {
                    status: StatusCode::OK,
                    headers: HeaderMap::new(),
                    body: Body::empty(),
                }),
                Err(error) => Err(error),
            }
        }
    }

    struct FailingTrackRepository;

    #[async_trait]
    impl TrackRepository for FailingTrackRepository {
        async fn get_or_create(
            &self,
            _source: SourceIdentity,
            _metadata: TrackMetadata,
        ) -> Result<Track, TrackRepositoryError> {
            Err(TrackRepositoryError::Unavailable)
        }

        async fn find_sources(
            &self,
            _track_id: &TrackId,
        ) -> Result<Vec<SourceIdentity>, TrackRepositoryError> {
            Err(TrackRepositoryError::Unavailable)
        }

        async fn find_track(
            &self,
            _track_id: &TrackId,
        ) -> Result<Option<Track>, TrackRepositoryError> {
            Err(TrackRepositoryError::Unavailable)
        }
    }

    struct MultiSourceRepository {
        inner: Arc<InMemoryTrackRepository>,
        sources: Vec<SourceIdentity>,
    }

    #[async_trait]
    impl TrackRepository for MultiSourceRepository {
        async fn get_or_create(
            &self,
            source: SourceIdentity,
            metadata: TrackMetadata,
        ) -> Result<Track, TrackRepositoryError> {
            self.inner.get_or_create(source, metadata).await
        }

        async fn find_sources(
            &self,
            _track_id: &TrackId,
        ) -> Result<Vec<SourceIdentity>, TrackRepositoryError> {
            Ok(self.sources.clone())
        }

        async fn find_track(
            &self,
            track_id: &TrackId,
        ) -> Result<Option<Track>, TrackRepositoryError> {
            self.inner.find_track(track_id).await
        }
    }

    struct StubTransport;

    #[async_trait]
    impl UpstreamTransport for StubTransport {
        async fn search(
            &self,
            _target: &UpstreamTarget,
            _search: &crate::catalogue::CatalogueSearch,
            _context: &RequestContext,
        ) -> Result<Vec<UpstreamCandidate>, UpstreamError> {
            Ok(Vec::new())
        }

        async fn resolve(
            &self,
            _target: &UpstreamTarget,
            _external_id: &str,
            _context: &RequestContext,
        ) -> Result<String, UpstreamError> {
            Ok("https://music.example.test/audio".to_owned())
        }
    }

    fn global(external_id: &str) -> SourceIdentity {
        SourceIdentity::new(
            ProviderId::new("youtube_music".to_owned()).unwrap(),
            SourceScope::Global,
            external_id.to_owned(),
        )
        .unwrap()
    }

    fn track_metadata(title: &str) -> TrackMetadata {
        TrackMetadata::new(title.to_owned(), vec!["Artist".to_owned()], Some(1_000)).unwrap()
    }

    fn playable_media() -> PlayableMedia {
        PlayableMedia::new(
            PlaybackUrl::new("https://media.example.test/audio".to_owned()).unwrap(),
            HashMap::new(),
            None,
            MediaMetadata::new(None, None, Some("opus".to_owned()), Some(128.0), None).unwrap(),
        )
    }

    fn context() -> RequestContext {
        RequestContext::new(RequestId::new("request-123".to_owned()).unwrap())
    }

    async fn stored_track(
        repository: &Arc<InMemoryTrackRepository>,
        identity: SourceIdentity,
    ) -> TrackId {
        repository
            .get_or_create(identity, track_metadata("Title"))
            .await
            .unwrap()
            .id()
            .clone()
    }

    fn responses(values: Vec<Result<PlayableMedia, ProviderError>>) -> Arc<QueueProvider> {
        Arc::new(QueueProvider {
            responses: Mutex::new(values.into()),
            seen: Mutex::new(Vec::new()),
        })
    }

    fn service(
        repository: Arc<dyn TrackRepository>,
        values: Vec<Result<PlayableMedia, ProviderError>>,
    ) -> (PlaybackService, Arc<QueueProvider>) {
        let provider = responses(values);
        let service = PlaybackService::new(
            repository,
            vec![Arc::clone(&provider) as Arc<dyn PlaybackProvider>],
            Arc::new(FakeDelivery::default()),
        );
        (service, provider)
    }

    async fn resolve_global(
        service: &PlaybackService,
        track: &TrackId,
        context: &RequestContext,
    ) -> Result<PlayableMedia, PlaybackError> {
        let mut error = PlaybackError::TrackNotFound;
        for source in service.candidates(track).await? {
            match service.resolve_source(&source, None, context).await {
                Ok(Some(resolved)) => return Ok(resolved.media),
                Ok(None) => {}
                Err(failure) => error = failure,
            }
        }
        Err(error)
    }

    #[tokio::test]
    async fn resolve_paths() {
        let repository = Arc::new(InMemoryTrackRepository::default());
        let source = global("source-123");
        let track_id = stored_track(&repository, source.clone()).await;
        let expected = playable_media();
        let (playback, provider) = service(repository, vec![Ok(expected.clone())]);
        let result = resolve_global(&playback, &track_id, &context())
            .await
            .unwrap();
        assert_eq!(result, expected);
        assert_eq!(
            provider.seen.lock().as_slice(),
            &[PlaybackSource::from_identity(source).unwrap()]
        );

        let (playback, provider) = service(
            Arc::new(InMemoryTrackRepository::default()),
            vec![Ok(playable_media())],
        );
        let result = resolve_global(&playback, &TrackId::new(), &context()).await;
        assert!(matches!(result, Err(PlaybackError::TrackNotFound)));
        assert!(provider.seen.lock().is_empty());

        let (playback, _) = service(Arc::new(FailingTrackRepository), vec![Ok(playable_media())]);
        let result = resolve_global(&playback, &TrackId::new(), &context()).await;
        assert!(matches!(
            result,
            Err(PlaybackError::Repository(TrackRepositoryError::Unavailable))
        ));

        let repository = Arc::new(InMemoryTrackRepository::default());
        let track_id = stored_track(&repository, global("source-123")).await;
        let (playback, _) = service(repository, vec![Err(ProviderError::ProviderUnavailable)]);
        let result = resolve_global(&playback, &track_id, &context()).await;
        assert!(matches!(
            result,
            Err(PlaybackError::Provider(ProviderError::ProviderUnavailable))
        ));
    }

    #[tokio::test]
    async fn resolve_never_sends_a_named_source_to_the_fallback_resolver() {
        let repository = Arc::new(InMemoryTrackRepository::default());
        let named = SourceIdentity::new(
            ProviderId::new("navidrome".to_owned()).unwrap(),
            SourceScope::Named("server-1".to_owned()),
            "song-1".to_owned(),
        )
        .unwrap();
        let global = global("vid-1");
        repository
            .get_or_create(named, track_metadata("Upstream"))
            .await
            .unwrap();
        let track_id = stored_track(&repository, global.clone()).await;
        let (service, provider) = service(repository, vec![Ok(playable_media())]);

        resolve_global(&service, &track_id, &context())
            .await
            .unwrap();

        assert_eq!(
            provider.seen.lock().as_slice(),
            &[PlaybackSource::from_identity(global).unwrap()]
        );
    }

    #[tokio::test]
    async fn resolve_tries_the_next_global_source_after_a_failure() {
        let first = global("vid-1");
        let second = global("vid-2");
        let repository = Arc::new(MultiSourceRepository {
            inner: Arc::new(InMemoryTrackRepository::default()),
            sources: vec![first.clone(), second.clone()],
        });
        let (service, provider) = service(
            repository,
            vec![
                Err(ProviderError::ProviderUnavailable),
                Ok(playable_media()),
            ],
        );

        let result = resolve_global(&service, &TrackId::new(), &context())
            .await
            .unwrap();

        assert_eq!(result, playable_media());
        assert_eq!(
            *provider.seen.lock(),
            vec![
                PlaybackSource::from_identity(first).unwrap(),
                PlaybackSource::from_identity(second).unwrap(),
            ]
        );
    }

    #[tokio::test]
    async fn stream_falls_back_to_the_next_source() {
        let repository = Arc::new(MultiSourceRepository {
            inner: Arc::new(InMemoryTrackRepository::default()),
            sources: vec![global("vid-1"), global("vid-2")],
        });
        let provider = responses(vec![Ok(playable_media()), Ok(playable_media())]);
        let delivery = Arc::new(FakeDelivery::with_outcomes(vec![
            Err(DeliveryError::UpstreamUnavailable),
            Ok(()),
        ]));
        let service = PlaybackService::new(
            repository,
            vec![Arc::clone(&provider) as Arc<dyn PlaybackProvider>],
            Arc::clone(&delivery) as Arc<dyn PlaybackDelivery>,
        );

        let result = service
            .stream(&TrackId::new(), None, &context(), None)
            .await;

        assert!(result.is_ok());
        assert_eq!(*delivery.attempts.lock(), 2);
    }

    #[tokio::test]
    async fn stream_retries_a_retryable_delivery_error() {
        let repository = Arc::new(InMemoryTrackRepository::default());
        let track_id = stored_track(&repository, global("vid-1")).await;
        let provider = responses(vec![Ok(playable_media()), Ok(playable_media())]);
        let delivery = Arc::new(FakeDelivery::with_outcomes(vec![
            Err(DeliveryError::Media(MediaFetchError::UnexpectedStatus(
                StatusCode::SERVICE_UNAVAILABLE,
            ))),
            Ok(()),
        ]));
        let service = PlaybackService::new(
            repository,
            vec![Arc::clone(&provider) as Arc<dyn PlaybackProvider>],
            Arc::clone(&delivery) as Arc<dyn PlaybackDelivery>,
        );

        let result = service.stream(&track_id, None, &context(), None).await;

        assert!(result.is_ok());
        assert_eq!(*delivery.attempts.lock(), 2);
        assert_eq!(provider.seen.lock().len(), 2);
    }

    #[tokio::test]
    async fn stream_stops_on_a_non_retryable_delivery_error() {
        let repository = Arc::new(InMemoryTrackRepository::default());
        let track_id = stored_track(&repository, global("vid-1")).await;
        let delivery = Arc::new(FakeDelivery::with_outcomes(vec![Err(DeliveryError::Spool)]));
        let service = PlaybackService::new(
            repository,
            vec![responses(vec![Ok(playable_media())]) as Arc<dyn PlaybackProvider>],
            Arc::clone(&delivery) as Arc<dyn PlaybackDelivery>,
        );

        let result = service.stream(&track_id, None, &context(), None).await;

        assert!(matches!(
            result,
            Err(PlaybackError::Delivery(DeliveryError::Spool))
        ));
        assert_eq!(*delivery.attempts.lock(), 1);
    }

    #[tokio::test]
    async fn stream_reports_track_not_found_without_delivering() {
        let delivery = Arc::new(FakeDelivery::default());
        let service = PlaybackService::new(
            Arc::new(InMemoryTrackRepository::default()),
            vec![],
            Arc::clone(&delivery) as Arc<dyn PlaybackDelivery>,
        );

        let result = service
            .stream(&TrackId::new(), None, &context(), None)
            .await;

        assert!(matches!(result, Err(PlaybackError::TrackNotFound)));
        assert_eq!(*delivery.attempts.lock(), 0);
    }

    #[tokio::test]
    async fn stream_marks_a_rejected_link_stale() {
        let key =
            CredentialKey::from_base64("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA").unwrap();
        let user = Uuid::now_v7();
        let server = Uuid::now_v7();
        let store = Arc::new(MemoryUpstreamLinkStore::new());
        store.insert_link(
            &key,
            user,
            server,
            "https://music.example.test",
            "friend",
            "password",
        );
        let adapter = NavidromeAdapter::new(
            Arc::clone(&store) as Arc<dyn UpstreamLinkStore>,
            key,
            Arc::new(StubTransport),
        );
        let repository = Arc::new(InMemoryTrackRepository::default());
        let track_id = repository
            .get_or_create(
                SourceIdentity::new(
                    ProviderId::new("navidrome".to_owned()).unwrap(),
                    SourceScope::Named(server.to_string()),
                    "song-1".to_owned(),
                )
                .unwrap(),
                track_metadata("Song"),
            )
            .await
            .unwrap()
            .id()
            .clone();
        let delivery = Arc::new(FakeDelivery::with_outcomes(vec![Err(
            DeliveryError::UpstreamUnauthorized,
        )]));
        let service = PlaybackService::new(
            repository,
            vec![Arc::new(adapter) as Arc<dyn PlaybackProvider>],
            Arc::clone(&delivery) as Arc<dyn PlaybackDelivery>,
        );

        let result = service
            .stream(&track_id, Some(&user), &context(), None)
            .await;

        assert!(matches!(result, Err(PlaybackError::CredentialsStale)));
        assert!(store.is_stale(&user, &server));
    }
}
