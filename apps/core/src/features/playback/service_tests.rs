use std::collections::{HashMap, VecDeque};

use async_trait::async_trait;
use axum::{
    body::Body,
    http::{HeaderMap, StatusCode},
};
use parking_lot::Mutex;

use super::*;
use crate::{
    features::music_library::domain::{ProviderId, SourceIdentity, SourceScope},
    features::playback::media::MediaFetchError,
    features::playback::{MediaMetadata, PlayableMedia, PlaybackUrl},
    platform::request::RequestId,
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
            .map(|media| Some(ResolvedPlayback::Remote(media)))
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

/// Playback only asks for candidates; the fake rejects any extra lookup.
struct ScriptedSources {
    answers: Mutex<VecDeque<Result<Vec<SourceIdentity>, TrackRepositoryError>>>,
}

impl ScriptedSources {
    fn new(answer: Result<Vec<SourceIdentity>, TrackRepositoryError>) -> Self {
        Self {
            answers: Mutex::new(vec![answer].into()),
        }
    }
}

#[async_trait]
impl SourceLookup for ScriptedSources {
    async fn find_sources(
        &self,
        _track_id: &TrackId,
    ) -> Result<Vec<SourceIdentity>, TrackRepositoryError> {
        self.answers
            .lock()
            .pop_front()
            .expect("unexpected source query")
    }
}

use crate::features::music_library::test_support::{SourceLookup, TrackRepositoryError};

fn global(external_id: &str) -> SourceIdentity {
    SourceIdentity::new(
        ProviderId::new("youtube_music".to_owned()).unwrap(),
        SourceScope::Global,
        external_id.to_owned(),
    )
    .unwrap()
}

fn sources(identities: Vec<SourceIdentity>) -> Arc<ScriptedSources> {
    Arc::new(ScriptedSources::new(Ok(identities)))
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

fn failing_sources() -> Arc<ScriptedSources> {
    Arc::new(ScriptedSources::new(Err(TrackRepositoryError::Unavailable)))
}

fn responses(values: Vec<Result<PlayableMedia, ProviderError>>) -> Arc<QueueProvider> {
    Arc::new(QueueProvider {
        responses: Mutex::new(values.into()),
        seen: Mutex::new(Vec::new()),
    })
}

fn queries<T: SourceLookup + 'static>(reader: Arc<T>) -> Arc<SourceQueries> {
    Arc::new(SourceQueries::new(reader))
}

fn service(
    sources: Arc<dyn SourceLookup>,
    values: Vec<Result<PlayableMedia, ProviderError>>,
) -> (StreamingService, Arc<QueueProvider>) {
    let provider = responses(values);
    let service = StreamingService::new(
        Arc::new(SourceQueries::new(sources)),
        vec![Arc::clone(&provider) as Arc<dyn PlaybackProvider>],
        Arc::new(FakeDelivery::default()),
    );
    (service, provider)
}

async fn resolve_global(
    service: &StreamingService,
    track: &TrackId,
    context: &RequestContext,
) -> Result<PlayableMedia, PlaybackError> {
    let mut error = PlaybackError::TrackNotFound;
    for source in service.candidates(track).await? {
        match service.resolve_source(&source, None, context).await {
            Ok(Some(ResolvedPlayback::Remote(media))) => return Ok(media),
            Ok(Some(ResolvedPlayback::Local(_))) => {
                unreachable!("the queue provider resolves remote media")
            }
            Ok(None) => {}
            Err(failure) => error = failure,
        }
    }
    Err(error)
}

#[tokio::test]
async fn resolve_paths() {
    let source = global("source-123");
    let track_id = TrackId::new();
    let repository = sources(vec![source.clone()]);
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

    let (playback, provider) = service(sources(vec![]), vec![Ok(playable_media())]);
    let result = resolve_global(&playback, &TrackId::new(), &context()).await;
    assert!(matches!(result, Err(PlaybackError::TrackNotFound)));
    assert!(provider.seen.lock().is_empty());

    let (playback, _) = service(failing_sources(), vec![Ok(playable_media())]);
    let result = resolve_global(&playback, &TrackId::new(), &context()).await;
    assert!(matches!(
        result,
        Err(PlaybackError::Library(SourceQueryError::Unavailable))
    ));

    let track_id = TrackId::new();
    let repository = sources(vec![global("source-123")]);
    let (playback, _) = service(repository, vec![Err(ProviderError::ProviderUnavailable)]);
    let result = resolve_global(&playback, &track_id, &context()).await;
    assert!(matches!(
        result,
        Err(PlaybackError::Provider(ProviderError::ProviderUnavailable))
    ));
}

#[tokio::test]
async fn resolve_tries_the_next_global_source_after_a_failure() {
    let first = global("vid-1");
    let second = global("vid-2");
    let repository = sources(vec![first.clone(), second.clone()]);
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
    let repository = sources(vec![global("vid-1"), global("vid-2")]);
    let provider = responses(vec![Ok(playable_media()), Ok(playable_media())]);
    let delivery = Arc::new(FakeDelivery::with_outcomes(vec![
        Err(DeliveryError::Media(MediaFetchError::UnexpectedStatus(
            StatusCode::SERVICE_UNAVAILABLE,
        ))),
        Ok(()),
    ]));
    let service = StreamingService::new(
        queries(repository),
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
    let repository = sources(vec![global("vid-1")]);
    let track_id = TrackId::new();
    let provider = responses(vec![Ok(playable_media()), Ok(playable_media())]);
    let delivery = Arc::new(FakeDelivery::with_outcomes(vec![
        Err(DeliveryError::Media(MediaFetchError::UnexpectedStatus(
            StatusCode::SERVICE_UNAVAILABLE,
        ))),
        Ok(()),
    ]));
    let service = StreamingService::new(
        queries(repository),
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
    let repository = sources(vec![global("vid-1")]);
    let track_id = TrackId::new();
    let delivery = Arc::new(FakeDelivery::with_outcomes(vec![Err(DeliveryError::Spool)]));
    let service = StreamingService::new(
        queries(repository),
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
async fn provider_access_failure_is_internal_and_does_not_try_another_source() {
    let provider = responses(vec![Err(ProviderError::Access)]);
    let service = StreamingService::new(
        queries(sources(vec![global("vid-1"), global("vid-2")])),
        vec![Arc::clone(&provider) as Arc<dyn PlaybackProvider>],
        Arc::new(FakeDelivery::default()),
    );

    let result = service
        .stream(&TrackId::new(), None, &context(), None)
        .await;

    assert!(matches!(
        result,
        Err(PlaybackError::Internal(ProviderError::Access))
    ));
    assert_eq!(provider.seen.lock().len(), 1);
}

#[tokio::test]
async fn stream_reports_track_not_found_without_delivering() {
    let delivery = Arc::new(FakeDelivery::default());
    let service = StreamingService::new(
        queries(sources(vec![])),
        vec![],
        Arc::clone(&delivery) as Arc<dyn PlaybackDelivery>,
    );

    let result = service
        .stream(&TrackId::new(), None, &context(), None)
        .await;

    assert!(matches!(result, Err(PlaybackError::TrackNotFound)));
    assert_eq!(*delivery.attempts.lock(), 0);
}
