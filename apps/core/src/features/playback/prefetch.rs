use std::sync::Arc;

use tokio::sync::Semaphore;
use uuid::Uuid;

use crate::{
    features::music_library::domain::TrackId,
    features::playback::{PlaybackDelivery, ResolvedPlayback, SourceKey},
    platform::request::RequestContext,
};

use super::{next::NextTracks, service::SourceResolver};

const MAX_IN_FLIGHT: usize = 2;

pub(super) struct Prefetch {
    next: Arc<dyn NextTracks>,
    resolver: SourceResolver,
    delivery: Arc<dyn PlaybackDelivery>,
    count: usize,
    in_flight: Arc<Semaphore>,
}

impl Prefetch {
    pub(super) fn new(
        next: Arc<dyn NextTracks>,
        resolver: SourceResolver,
        delivery: Arc<dyn PlaybackDelivery>,
        count: usize,
    ) -> Self {
        Self {
            next,
            resolver,
            delivery,
            count,
            in_flight: Arc::new(Semaphore::new(MAX_IN_FLIGHT)),
        }
    }

    pub(super) fn after(
        self: &Arc<Self>,
        track_id: TrackId,
        user: Option<Uuid>,
        context: RequestContext,
    ) {
        if self.count == 0 {
            return;
        }
        let prefetch = Arc::clone(self);
        tokio::spawn(async move {
            prefetch.warm_next(track_id, user, context).await;
        });
    }

    async fn warm_next(
        self: &Arc<Self>,
        track_id: TrackId,
        user: Option<Uuid>,
        context: RequestContext,
    ) {
        for next in self.next.next(user, &track_id, self.count).await {
            let Ok(permit) = Arc::clone(&self.in_flight).try_acquire_owned() else {
                continue;
            };
            let prefetch = Arc::clone(self);
            let context = context.clone();
            tokio::spawn(async move {
                prefetch.warm_track(next, user, context).await;
                drop(permit);
            });
        }
    }

    #[tracing::instrument(skip_all, fields(track_id = %track_id, count = self.count, outcome))]
    async fn warm_track(&self, track_id: TrackId, user: Option<Uuid>, context: RequestContext) {
        let outcome = self.warm_source(&track_id, user, &context).await;
        tracing::Span::current().record("outcome", outcome);
    }

    async fn warm_source(
        &self,
        track_id: &TrackId,
        user: Option<Uuid>,
        context: &RequestContext,
    ) -> &'static str {
        let candidates = match self.resolver.candidates(track_id).await {
            Ok(candidates) => candidates,
            Err(error) => {
                tracing::debug!(error = %error, "playback prefetch could not list sources");
                return "candidates_failed";
            }
        };
        let Some(source) = candidates.first() else {
            return "no_source";
        };
        let key = SourceKey::new(source);
        if self.delivery.is_cached(&key) {
            return "cached";
        }
        let resolved = match self
            .resolver
            .resolve_source(source, user.as_ref(), context)
            .await
        {
            Ok(Some(resolved)) => resolved,
            Ok(None) => return "unresolved",
            Err(error) => {
                tracing::debug!(error = %error, "playback prefetch could not resolve the source");
                return "resolve_failed";
            }
        };
        if !matches!(resolved, ResolvedPlayback::Remote(_)) {
            return "local";
        }
        match self.delivery.warm(&key, &resolved).await {
            Ok(()) => "warmed",
            Err(error) => {
                tracing::debug!(error = %error, "playback prefetch could not warm the source");
                "warm_failed"
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::HashMap, path::PathBuf, sync::Arc, time::Duration};

    use async_trait::async_trait;
    use axum::{
        body::Body,
        http::{HeaderMap, StatusCode},
    };
    use parking_lot::Mutex;
    use tokio::sync::{Barrier, mpsc};
    use uuid::Uuid;

    use super::*;
    use crate::{
        features::music_library::{
            domain::{ProviderKind, SourceIdentity},
            sources::SourceQueries,
            test_support::SourceLookup,
        },
        features::playback::{
            CatalogueNextTracks, DeliveryError, DeliveryResult, LocalMedia, MediaMetadata,
            PlayableMedia, PlaybackProvider, PlaybackSource, PlaybackUrl, StreamingService,
            next::fixtures::{FakeQueue, FakeSources, library, local, queue, track, youtube},
            providers::ProviderError,
        },
        platform::request::{RequestContext, RequestId},
    };

    fn key_for(identity: SourceIdentity) -> SourceKey {
        SourceKey::new(&PlaybackSource::from_identity(identity).unwrap())
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

    struct StubProvider {
        failing: Vec<String>,
        gate: Mutex<Option<Arc<Barrier>>>,
        resolved: Mutex<Vec<String>>,
    }

    impl StubProvider {
        fn new(failing: Vec<String>) -> Self {
            Self {
                failing,
                gate: Mutex::new(None),
                resolved: Mutex::new(Vec::new()),
            }
        }

        fn gated(failing: Vec<String>, gate: Arc<Barrier>) -> Self {
            Self {
                failing,
                gate: Mutex::new(Some(gate)),
                resolved: Mutex::new(Vec::new()),
            }
        }
    }

    #[async_trait]
    impl PlaybackProvider for StubProvider {
        fn kind(&self) -> ProviderKind {
            ProviderKind::YoutubeMusic
        }

        async fn resolve(
            &self,
            source: &PlaybackSource,
            _user: Option<&Uuid>,
            _context: &RequestContext,
        ) -> Result<Option<ResolvedPlayback>, ProviderError> {
            let external_id = source.identity().external_id().to_owned();
            self.resolved.lock().push(external_id.clone());
            if self.failing.contains(&external_id) {
                let gate = self.gate.lock().clone();
                if let Some(gate) = gate {
                    gate.wait().await;
                }
                return Err(ProviderError::ProviderUnavailable);
            }
            Ok(Some(ResolvedPlayback::Remote(playable_media())))
        }
    }

    #[derive(Default)]
    struct LocalProvider {
        resolved: Mutex<Vec<String>>,
    }

    #[async_trait]
    impl PlaybackProvider for LocalProvider {
        fn kind(&self) -> ProviderKind {
            ProviderKind::Local
        }

        async fn resolve(
            &self,
            source: &PlaybackSource,
            _user: Option<&Uuid>,
            _context: &RequestContext,
        ) -> Result<Option<ResolvedPlayback>, ProviderError> {
            self.resolved
                .lock()
                .push(source.identity().external_id().to_owned());
            Ok(Some(ResolvedPlayback::Local(
                LocalMedia::new(PathBuf::from("/music/track.flac"), 10, None).unwrap(),
            )))
        }
    }

    #[derive(Default)]
    struct RecordingDelivery {
        present: Mutex<Vec<SourceKey>>,
        warmed: Mutex<Vec<SourceKey>>,
        delivered: Mutex<usize>,
        entered: Mutex<Option<mpsc::UnboundedSender<SourceKey>>>,
        finished: Mutex<Option<mpsc::UnboundedSender<SourceKey>>>,
        barrier: Mutex<Option<Arc<Barrier>>>,
    }

    impl RecordingDelivery {
        fn signal(
            &self,
        ) -> (
            mpsc::UnboundedReceiver<SourceKey>,
            mpsc::UnboundedReceiver<SourceKey>,
        ) {
            let (entered, entered_rx) = mpsc::unbounded_channel();
            let (finished, finished_rx) = mpsc::unbounded_channel();
            *self.entered.lock() = Some(entered);
            *self.finished.lock() = Some(finished);
            (entered_rx, finished_rx)
        }

        fn hold(&self, barrier: Arc<Barrier>) {
            *self.barrier.lock() = Some(barrier);
        }

        fn prime(&self, key: SourceKey) {
            self.present.lock().push(key);
        }

        fn warmed(&self) -> Vec<SourceKey> {
            self.warmed.lock().clone()
        }
    }

    #[async_trait]
    impl PlaybackDelivery for RecordingDelivery {
        async fn deliver(
            &self,
            _key: &SourceKey,
            _resolved: &ResolvedPlayback,
            _client_range: Option<String>,
        ) -> Result<DeliveryResult, DeliveryError> {
            *self.delivered.lock() += 1;
            Ok(DeliveryResult {
                status: StatusCode::OK,
                headers: HeaderMap::new(),
                body: Body::empty(),
            })
        }

        async fn cached(
            &self,
            key: &SourceKey,
            _client_range: Option<String>,
        ) -> Option<Result<DeliveryResult, DeliveryError>> {
            self.is_cached(key).then(|| {
                Ok(DeliveryResult {
                    status: StatusCode::OK,
                    headers: HeaderMap::new(),
                    body: Body::empty(),
                })
            })
        }

        fn is_cached(&self, key: &SourceKey) -> bool {
            self.present.lock().contains(key)
        }

        async fn warm(
            &self,
            key: &SourceKey,
            _resolved: &ResolvedPlayback,
        ) -> Result<(), DeliveryError> {
            self.present.lock().push(key.clone());
            self.warmed.lock().push(key.clone());
            let sender = self.entered.lock().clone();
            if let Some(sender) = sender {
                let _ = sender.send(key.clone());
            }
            let barrier = self.barrier.lock().clone();
            if let Some(barrier) = barrier {
                barrier.wait().await;
            }
            let sender = self.finished.lock().clone();
            if let Some(sender) = sender {
                let _ = sender.send(key.clone());
            }
            Ok(())
        }
    }

    struct StubNextTracks {
        tracks: Vec<TrackId>,
        calls: Mutex<usize>,
        gate: Mutex<Option<Arc<Barrier>>>,
    }

    impl StubNextTracks {
        fn new(tracks: Vec<TrackId>) -> Self {
            Self {
                tracks,
                calls: Mutex::new(0),
                gate: Mutex::new(None),
            }
        }

        fn gated(tracks: Vec<TrackId>, gate: Arc<Barrier>) -> Self {
            Self {
                tracks,
                calls: Mutex::new(0),
                gate: Mutex::new(Some(gate)),
            }
        }
    }

    #[async_trait]
    impl NextTracks for StubNextTracks {
        async fn next(
            &self,
            _user: Option<Uuid>,
            _track_id: &TrackId,
            _count: usize,
        ) -> Vec<TrackId> {
            *self.calls.lock() += 1;
            let gate = self.gate.lock().clone();
            if let Some(gate) = gate {
                gate.wait().await;
            }
            self.tracks.clone()
        }
    }

    fn resolver(
        sources: Arc<FakeSources>,
        providers: Vec<Arc<dyn PlaybackProvider>>,
    ) -> SourceResolver {
        SourceResolver::new(
            Arc::new(SourceQueries::new(sources as Arc<dyn SourceLookup>)),
            providers,
        )
    }

    #[tokio::test]
    async fn prefetch_warms_the_next_queue_entry() {
        let user = Uuid::from_u128(1);
        let first = TrackId::new();
        let second = TrackId::new();
        let sources = Arc::new(FakeSources::new(vec![(
            second.clone(),
            vec![youtube("vid-2")],
        )]));
        let delivery = Arc::new(RecordingDelivery::default());
        let (mut entered, _) = delivery.signal();
        let next = Arc::new(CatalogueNextTracks::new(
            queue(FakeQueue::with_entries(
                user,
                vec![first.clone(), second.clone()],
                Some(0),
            )),
            library(Vec::new()),
        ));
        let prefetch = Arc::new(Prefetch::new(
            next,
            resolver(sources, vec![Arc::new(StubProvider::new(Vec::new()))]),
            Arc::clone(&delivery) as Arc<dyn PlaybackDelivery>,
            1,
        ));

        prefetch.after(first, Some(user), context());

        assert_eq!(entered.recv().await.unwrap(), key_for(youtube("vid-2")));
        assert_eq!(delivery.warmed(), vec![key_for(youtube("vid-2"))]);
    }

    #[tokio::test]
    async fn prefetch_falls_back_to_album_order() {
        let user = Uuid::from_u128(1);
        let first = track("One");
        let second = track("Two");
        let sources = Arc::new(FakeSources::new(vec![(
            second.id().clone(),
            vec![youtube("vid-2")],
        )]));
        let delivery = Arc::new(RecordingDelivery::default());
        let (mut entered, _) = delivery.signal();
        let next = Arc::new(CatalogueNextTracks::new(
            queue(FakeQueue::empty()),
            library(vec![first.clone(), second.clone()]),
        ));
        let prefetch = Arc::new(Prefetch::new(
            next,
            resolver(sources, vec![Arc::new(StubProvider::new(Vec::new()))]),
            Arc::clone(&delivery) as Arc<dyn PlaybackDelivery>,
            1,
        ));

        prefetch.after(first.id().clone(), Some(user), context());

        assert_eq!(entered.recv().await.unwrap(), key_for(youtube("vid-2")));
        assert_eq!(delivery.warmed(), vec![key_for(youtube("vid-2"))]);
    }

    #[tokio::test]
    async fn prefetch_skips_a_local_next_track() {
        let user = Uuid::from_u128(1);
        let second = TrackId::new();
        let sources = Arc::new(FakeSources::new(vec![(second.clone(), vec![local()])]));
        let provider = Arc::new(LocalProvider::default());
        let delivery = Arc::new(RecordingDelivery::default());
        let prefetch = Prefetch::new(
            Arc::new(StubNextTracks::new(vec![second.clone()])),
            resolver(
                sources,
                vec![Arc::clone(&provider) as Arc<dyn PlaybackProvider>],
            ),
            Arc::clone(&delivery) as Arc<dyn PlaybackDelivery>,
            1,
        );

        let outcome = prefetch.warm_source(&second, Some(user), &context()).await;

        assert_eq!(outcome, "local");
        assert_eq!(
            provider.resolved.lock().as_slice(),
            &["recording-1".to_owned()]
        );
        assert!(delivery.warmed().is_empty());
    }

    #[tokio::test]
    async fn a_prefetch_failure_does_not_affect_the_stream() {
        let user = Uuid::from_u128(1);
        let first = TrackId::new();
        let second = TrackId::new();
        let sources = Arc::new(FakeSources::new(vec![
            (first.clone(), vec![youtube("vid-1")]),
            (second.clone(), vec![youtube("vid-2")]),
        ]));
        let gate = Arc::new(Barrier::new(2));
        let provider = Arc::new(StubProvider::gated(
            vec!["vid-2".to_owned()],
            Arc::clone(&gate),
        ));
        let delivery = Arc::new(RecordingDelivery::default());
        let service = StreamingService::new(
            Arc::new(SourceQueries::new(
                Arc::clone(&sources) as Arc<dyn SourceLookup>
            )),
            vec![Arc::clone(&provider) as Arc<dyn PlaybackProvider>],
            Arc::clone(&delivery) as Arc<dyn PlaybackDelivery>,
        )
        .with_prefetch(Arc::new(StubNextTracks::new(vec![second.clone()])), 1);

        let result = service.stream(&first, Some(&user), &context(), None).await;

        assert!(result.is_ok());
        assert_eq!(*delivery.delivered.lock(), 1);

        tokio::time::timeout(Duration::from_secs(5), gate.wait())
            .await
            .expect("the held prefetch resolve must be released");
        assert_eq!(
            provider.resolved.lock().as_slice(),
            &["vid-1".to_owned(), "vid-2".to_owned()]
        );

        let failing = Arc::new(StubProvider::new(vec!["vid-2".to_owned()]));
        let prefetch = Prefetch::new(
            Arc::new(StubNextTracks::new(vec![second.clone()])),
            resolver(
                Arc::clone(&sources),
                vec![Arc::clone(&failing) as Arc<dyn PlaybackProvider>],
            ),
            Arc::clone(&delivery) as Arc<dyn PlaybackDelivery>,
            1,
        );

        let outcome = prefetch.warm_source(&second, Some(user), &context()).await;

        assert_eq!(outcome, "resolve_failed");
        assert!(delivery.warmed().is_empty());
    }

    #[tokio::test]
    async fn prefetch_is_off_at_zero() {
        let first = TrackId::new();
        let next = Arc::new(StubNextTracks::new(vec![TrackId::new()]));
        let delivery = Arc::new(RecordingDelivery::default());
        let prefetch = Arc::new(Prefetch::new(
            Arc::clone(&next) as Arc<dyn NextTracks>,
            resolver(Arc::new(FakeSources::default()), Vec::new()),
            Arc::clone(&delivery) as Arc<dyn PlaybackDelivery>,
            0,
        ));

        prefetch.after(first, None, context());

        assert_eq!(*next.calls.lock(), 0);
        assert!(delivery.warmed().is_empty());
    }

    #[tokio::test]
    async fn prefetch_respects_two_in_flight() {
        let first = TrackId::new();
        let second = TrackId::new();
        let third = TrackId::new();
        let fourth = TrackId::new();
        let sources = Arc::new(FakeSources::new(vec![
            (second.clone(), vec![youtube("vid-2")]),
            (third.clone(), vec![youtube("vid-3")]),
            (fourth.clone(), vec![youtube("vid-4")]),
        ]));
        let delivery = Arc::new(RecordingDelivery::default());
        let (mut entered, mut finished) = delivery.signal();
        delivery.hold(Arc::new(Barrier::new(3)));
        let prefetch = Arc::new(Prefetch::new(
            Arc::new(StubNextTracks::new(vec![
                second.clone(),
                third.clone(),
                fourth.clone(),
            ])),
            resolver(sources, vec![Arc::new(StubProvider::new(Vec::new()))]),
            Arc::clone(&delivery) as Arc<dyn PlaybackDelivery>,
            3,
        ));

        prefetch.after(first, None, context());

        let first_warm = entered.recv().await.unwrap();
        let second_warm = entered.recv().await.unwrap();
        let barrier = delivery.barrier.lock().clone().unwrap();
        barrier.wait().await;
        finished.recv().await.unwrap();
        finished.recv().await.unwrap();

        assert_eq!(delivery.warmed().len(), 2);
        assert!(delivery.warmed().contains(&first_warm));
        assert!(delivery.warmed().contains(&second_warm));
        assert!(!delivery.warmed().contains(&key_for(youtube("vid-4"))));
    }

    #[tokio::test]
    async fn a_cached_hit_also_prefetches_the_next_track() {
        let user = Uuid::from_u128(1);
        let first = TrackId::new();
        let second = TrackId::new();
        let sources = Arc::new(FakeSources::new(vec![
            (first.clone(), vec![youtube("vid-1")]),
            (second.clone(), vec![youtube("vid-2")]),
        ]));
        let delivery = Arc::new(RecordingDelivery::default());
        delivery.prime(key_for(youtube("vid-1")));
        let (mut entered, _) = delivery.signal();
        let service = StreamingService::new(
            Arc::new(SourceQueries::new(
                Arc::clone(&sources) as Arc<dyn SourceLookup>
            )),
            vec![Arc::new(StubProvider::new(Vec::new())) as Arc<dyn PlaybackProvider>],
            Arc::clone(&delivery) as Arc<dyn PlaybackDelivery>,
        )
        .with_prefetch(Arc::new(StubNextTracks::new(vec![second.clone()])), 1);

        let result = service.stream(&first, Some(&user), &context(), None).await;

        assert!(result.is_ok());
        assert_eq!(*delivery.delivered.lock(), 0);
        assert_eq!(entered.recv().await.unwrap(), key_for(youtube("vid-2")));
        assert_eq!(delivery.warmed(), vec![key_for(youtube("vid-2"))]);
    }

    #[tokio::test]
    async fn a_held_next_tracks_read_does_not_delay_the_stream() {
        let first = TrackId::new();
        let second = TrackId::new();
        let gate = Arc::new(Barrier::new(2));
        let sources = Arc::new(FakeSources::new(vec![
            (first.clone(), vec![youtube("vid-1")]),
            (second.clone(), vec![youtube("vid-2")]),
        ]));
        let delivery = Arc::new(RecordingDelivery::default());
        let service = Arc::new(
            StreamingService::new(
                Arc::new(SourceQueries::new(
                    Arc::clone(&sources) as Arc<dyn SourceLookup>
                )),
                vec![Arc::new(StubProvider::new(Vec::new())) as Arc<dyn PlaybackProvider>],
                Arc::clone(&delivery) as Arc<dyn PlaybackDelivery>,
            )
            .with_prefetch(
                Arc::new(StubNextTracks::gated(
                    vec![second.clone()],
                    Arc::clone(&gate),
                )),
                1,
            ),
        );

        let (sender, mut receiver) = mpsc::unbounded_channel();
        tokio::spawn(async move {
            let _ = sender.send(service.stream(&first, None, &context(), None).await.is_ok());
        });

        let streamed = tokio::time::timeout(Duration::from_secs(5), receiver.recv())
            .await
            .expect("the stream must not wait for the prefetch read")
            .unwrap();
        assert!(streamed);

        tokio::time::timeout(Duration::from_secs(5), gate.wait())
            .await
            .expect("the held prefetch read must be released");
    }
}
