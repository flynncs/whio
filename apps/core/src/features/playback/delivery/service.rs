use std::{sync::Arc, time::Duration};

use async_trait::async_trait;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use super::{local, session, spool::SpoolStrategy};
use crate::{
    features::playback::media::MediaFetcher,
    features::playback::{
        DeliveryError, DeliveryResult, PlaybackDelivery, ResolvedPlayback, SourceKey,
    },
    platform::clock::Clock,
};

pub const DEFAULT_MAX_SESSIONS: usize = 16;

/// bounded spool and local-file delivery. owns bytes, ranges, session limits
/// and cancellation.
pub struct StreamDelivery {
    spool: SpoolStrategy,
    sessions: Arc<Semaphore>,
}

impl StreamDelivery {
    pub fn new(
        fetcher: Arc<dyn MediaFetcher>,
        directory: std::path::PathBuf,
        max_sessions: usize,
    ) -> Self {
        Self {
            spool: SpoolStrategy::new(fetcher, directory),
            sessions: Arc::new(Semaphore::new(max_sessions.max(1))),
        }
    }

    pub fn with_spool_limits(mut self, per_spool: u64, total: u64) -> Self {
        self.spool = self.spool.with_limits(per_spool, total);
        self
    }

    pub fn with_spool_clock(mut self, clock: Clock) -> Self {
        self.spool = self.spool.with_clock(clock);
        self
    }

    pub fn with_spool_idle(mut self, idle: Duration) -> Self {
        self.spool = self.spool.with_idle(idle);
        self
    }

    fn acquire_session(&self) -> Result<OwnedSemaphorePermit, DeliveryError> {
        self.sessions
            .clone()
            .try_acquire_owned()
            .map_err(|_| DeliveryError::Overloaded)
    }
}

#[async_trait]
impl PlaybackDelivery for StreamDelivery {
    async fn deliver(
        &self,
        key: &SourceKey,
        resolved: &ResolvedPlayback,
        client_range: Option<String>,
    ) -> Result<DeliveryResult, DeliveryError> {
        let permit = self.acquire_session()?;
        let mut delivery = match resolved {
            ResolvedPlayback::Remote(media) => {
                self.spool.open(key, media, client_range.as_deref()).await?
            }
            ResolvedPlayback::Local(media) => local::serve(media, client_range).await?,
        };
        // the permit travels with the response body, so a session is only
        // released once the client has finished with it.
        delivery.body = session::hold(delivery.body, permit);
        Ok(delivery)
    }

    async fn cached(
        &self,
        key: &SourceKey,
        client_range: Option<String>,
    ) -> Option<Result<DeliveryResult, DeliveryError>> {
        let cached = self.spool.lookup(key)?;
        let permit = match self.acquire_session() {
            Ok(permit) => permit,
            Err(error) => return Some(Err(error)),
        };
        match self
            .spool
            .serve(key, &cached, client_range.as_deref())
            .await
        {
            Ok(mut delivery) => {
                delivery.body = session::hold(delivery.body, permit);
                Some(Ok(delivery))
            }
            Err(DeliveryError::Spool) if cached.failed() => None,
            Err(error) => Some(Err(error)),
        }
    }

    fn is_cached(&self, key: &SourceKey) -> bool {
        self.spool.is_cached(key)
    }

    async fn warm(
        &self,
        key: &SourceKey,
        resolved: &ResolvedPlayback,
    ) -> Result<(), DeliveryError> {
        match resolved {
            ResolvedPlayback::Remote(media) => self.spool.warm(key, media).await,
            ResolvedPlayback::Local(_) => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{
        collections::HashMap,
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
        time::Duration,
    };

    use async_trait::async_trait;
    use axum::http::StatusCode;
    use bytes::Bytes;
    use parking_lot::Mutex;
    use tokio::sync::{Notify, mpsc};
    use tokio_stream::StreamExt;

    use super::*;
    use crate::{
        features::music_library::domain::{ProviderId, SourceIdentity, SourceScope},
        features::playback::media::{MediaBody, MediaFetchError, MediaInfo},
        features::playback::{
            LocalMedia, MediaMetadata, PlayableMedia, PlaybackSource, PlaybackUrl,
        },
        platform::clock::Clock,
    };

    struct StubFetcher {
        bytes: Vec<u8>,
        probes: AtomicUsize,
    }

    impl StubFetcher {
        fn new(bytes: &[u8]) -> Self {
            Self {
                bytes: bytes.to_vec(),
                probes: AtomicUsize::new(0),
            }
        }

        fn probes(&self) -> usize {
            self.probes.load(Ordering::SeqCst)
        }
    }

    #[async_trait]
    impl MediaFetcher for StubFetcher {
        async fn probe(&self, _media: &PlayableMedia) -> Result<MediaInfo, MediaFetchError> {
            self.probes.fetch_add(1, Ordering::SeqCst);
            Ok(MediaInfo {
                content_length: self.bytes.len() as u64,
                supports_ranges: true,
                content_type: Some("audio/flac".to_owned()),
            })
        }

        async fn open_continuous(
            &self,
            _media: &PlayableMedia,
        ) -> Result<MediaBody, MediaFetchError> {
            Ok(MediaBody {
                content_length: Some(self.bytes.len() as u64),
                chunks: Box::pin(tokio_stream::iter(vec![Ok(Bytes::from(
                    self.bytes.clone(),
                ))])),
            })
        }
    }

    struct HeldFetcher {
        release: Arc<Notify>,
    }

    #[async_trait]
    impl MediaFetcher for HeldFetcher {
        async fn probe(&self, _media: &PlayableMedia) -> Result<MediaInfo, MediaFetchError> {
            Ok(MediaInfo {
                content_length: 10,
                supports_ranges: true,
                content_type: None,
            })
        }

        async fn open_continuous(
            &self,
            _media: &PlayableMedia,
        ) -> Result<MediaBody, MediaFetchError> {
            let (sender, receiver) = mpsc::channel(4);
            let release = Arc::clone(&self.release);
            tokio::spawn(async move {
                let _ = sender.send(Ok(Bytes::from_static(b"0123"))).await;
                release.notified().await;
                let _ = sender.send(Ok(Bytes::from_static(b"456789"))).await;
            });
            Ok(MediaBody {
                content_length: Some(10),
                chunks: Box::pin(tokio_stream::wrappers::ReceiverStream::new(receiver)),
            })
        }
    }

    struct HeldFailureFetcher {
        bytes: Vec<u8>,
        opens: AtomicUsize,
        release: Arc<Notify>,
    }

    impl HeldFailureFetcher {
        fn new(bytes: &[u8], release: Arc<Notify>) -> Self {
            Self {
                bytes: bytes.to_vec(),
                opens: AtomicUsize::new(0),
                release,
            }
        }
    }

    #[async_trait]
    impl MediaFetcher for HeldFailureFetcher {
        async fn probe(&self, _media: &PlayableMedia) -> Result<MediaInfo, MediaFetchError> {
            Ok(MediaInfo {
                content_length: self.bytes.len() as u64,
                supports_ranges: true,
                content_type: None,
            })
        }

        async fn open_continuous(
            &self,
            _media: &PlayableMedia,
        ) -> Result<MediaBody, MediaFetchError> {
            let opened = self.opens.fetch_add(1, Ordering::SeqCst) + 1;
            if opened > 1 {
                return Ok(MediaBody {
                    content_length: Some(self.bytes.len() as u64),
                    chunks: Box::pin(tokio_stream::iter(vec![Ok(Bytes::from(
                        self.bytes.clone(),
                    ))])),
                });
            }
            let (sender, receiver) = mpsc::channel(1);
            let release = Arc::clone(&self.release);
            tokio::spawn(async move {
                release.notified().await;
                let _ = sender
                    .send(Err(MediaFetchError::UnexpectedStatus(
                        StatusCode::BAD_GATEWAY,
                    )))
                    .await;
            });
            Ok(MediaBody {
                content_length: Some(self.bytes.len() as u64),
                chunks: Box::pin(tokio_stream::wrappers::ReceiverStream::new(receiver)),
            })
        }
    }

    struct GatedFetcher {
        bytes: Vec<u8>,
        hold_first_open: bool,
        fail_first_open: bool,
        release: Arc<Notify>,
        entered: mpsc::UnboundedSender<&'static str>,
        probes: AtomicUsize,
        opens: AtomicUsize,
    }

    impl GatedFetcher {
        fn new(
            bytes: &[u8],
            hold_first_open: bool,
            fail_first_open: bool,
            release: Arc<Notify>,
        ) -> (Self, mpsc::UnboundedReceiver<&'static str>) {
            let (entered, receiver) = mpsc::unbounded_channel();
            (
                Self {
                    bytes: bytes.to_vec(),
                    hold_first_open,
                    fail_first_open,
                    release,
                    entered,
                    probes: AtomicUsize::new(0),
                    opens: AtomicUsize::new(0),
                },
                receiver,
            )
        }

        fn probes(&self) -> usize {
            self.probes.load(Ordering::SeqCst)
        }

        fn opens(&self) -> usize {
            self.opens.load(Ordering::SeqCst)
        }
    }

    #[async_trait]
    impl MediaFetcher for GatedFetcher {
        async fn probe(&self, _media: &PlayableMedia) -> Result<MediaInfo, MediaFetchError> {
            self.probes.fetch_add(1, Ordering::SeqCst);
            let _ = self.entered.send("probe");
            Ok(MediaInfo {
                content_length: self.bytes.len() as u64,
                supports_ranges: true,
                content_type: None,
            })
        }

        async fn open_continuous(
            &self,
            _media: &PlayableMedia,
        ) -> Result<MediaBody, MediaFetchError> {
            let open = self.opens.fetch_add(1, Ordering::SeqCst) + 1;
            if self.hold_first_open && open == 1 {
                let _ = self.entered.send("open");
                self.release.notified().await;
            }
            if self.fail_first_open && open == 1 {
                return Err(MediaFetchError::UnexpectedStatus(StatusCode::BAD_GATEWAY));
            }
            Ok(MediaBody {
                content_length: Some(self.bytes.len() as u64),
                chunks: Box::pin(tokio_stream::iter(vec![Ok(Bytes::from(
                    self.bytes.clone(),
                ))])),
            })
        }
    }

    struct ColdFailureFetcher {
        bytes: Vec<u8>,
        probe_release: Arc<Notify>,
        failure_release: Arc<Notify>,
        entered: mpsc::UnboundedSender<&'static str>,
        probes: AtomicUsize,
        opens: AtomicUsize,
    }

    impl ColdFailureFetcher {
        fn new(
            bytes: &[u8],
            probe_release: Arc<Notify>,
            failure_release: Arc<Notify>,
        ) -> (Self, mpsc::UnboundedReceiver<&'static str>) {
            let (entered, receiver) = mpsc::unbounded_channel();
            (
                Self {
                    bytes: bytes.to_vec(),
                    probe_release,
                    failure_release,
                    entered,
                    probes: AtomicUsize::new(0),
                    opens: AtomicUsize::new(0),
                },
                receiver,
            )
        }

        fn opens(&self) -> usize {
            self.opens.load(Ordering::SeqCst)
        }
    }

    #[async_trait]
    impl MediaFetcher for ColdFailureFetcher {
        async fn probe(&self, _media: &PlayableMedia) -> Result<MediaInfo, MediaFetchError> {
            let probe = self.probes.fetch_add(1, Ordering::SeqCst) + 1;
            let _ = self.entered.send("probe");
            if probe == 1 {
                self.probe_release.notified().await;
            }
            Ok(MediaInfo {
                content_length: self.bytes.len() as u64,
                supports_ranges: true,
                content_type: None,
            })
        }

        async fn open_continuous(
            &self,
            _media: &PlayableMedia,
        ) -> Result<MediaBody, MediaFetchError> {
            let open = self.opens.fetch_add(1, Ordering::SeqCst) + 1;
            if open > 1 {
                return Ok(MediaBody {
                    content_length: Some(self.bytes.len() as u64),
                    chunks: Box::pin(tokio_stream::iter(vec![Ok(Bytes::from(
                        self.bytes.clone(),
                    ))])),
                });
            }
            let (sender, receiver) = mpsc::channel(1);
            let failure_release = Arc::clone(&self.failure_release);
            tokio::spawn(async move {
                failure_release.notified().await;
                let _ = sender
                    .send(Err(MediaFetchError::UnexpectedStatus(
                        StatusCode::BAD_GATEWAY,
                    )))
                    .await;
            });
            Ok(MediaBody {
                content_length: Some(self.bytes.len() as u64),
                chunks: Box::pin(tokio_stream::wrappers::ReceiverStream::new(receiver)),
            })
        }
    }

    struct HangingFetcher;

    #[async_trait]
    impl MediaFetcher for HangingFetcher {
        async fn probe(&self, _media: &PlayableMedia) -> Result<MediaInfo, MediaFetchError> {
            Ok(MediaInfo {
                content_length: 10,
                supports_ranges: true,
                content_type: None,
            })
        }

        async fn open_continuous(
            &self,
            _media: &PlayableMedia,
        ) -> Result<MediaBody, MediaFetchError> {
            Ok(MediaBody {
                content_length: Some(10),
                chunks: Box::pin(tokio_stream::pending::<Result<Bytes, MediaFetchError>>()),
            })
        }
    }

    fn media(url: &str) -> PlayableMedia {
        PlayableMedia::new(
            PlaybackUrl::new(url.to_owned()).unwrap(),
            HashMap::new(),
            None,
            MediaMetadata::new(None, None, Some("opus".to_owned()), Some(128.0), None).unwrap(),
        )
    }

    fn resolved(url: &str) -> ResolvedPlayback {
        ResolvedPlayback::Remote(media(url))
    }

    fn key(external_id: &str) -> SourceKey {
        SourceKey::new(
            &PlaybackSource::from_identity(
                SourceIdentity::new(
                    ProviderId::new("youtube_music".to_owned()).unwrap(),
                    SourceScope::Global,
                    external_id.to_owned(),
                )
                .unwrap(),
            )
            .unwrap(),
        )
    }

    fn local_key(path: &str) -> SourceKey {
        SourceKey::new(
            &PlaybackSource::from_identity(
                SourceIdentity::new(
                    ProviderId::new("local".to_owned()).unwrap(),
                    SourceScope::Named(path.to_owned()),
                    "recording-1".to_owned(),
                )
                .unwrap(),
            )
            .unwrap(),
        )
    }

    fn delivery(fetcher: Arc<dyn MediaFetcher>, max_sessions: usize) -> StreamDelivery {
        StreamDelivery::new(fetcher, std::env::temp_dir(), max_sessions)
    }

    async fn body_bytes(delivery: DeliveryResult) -> (StatusCode, axum::http::HeaderMap, Vec<u8>) {
        let status = delivery.status;
        let headers = delivery.headers;
        let bytes = axum::body::to_bytes(delivery.body, 1024 * 1024)
            .await
            .unwrap();
        (status, headers, bytes.to_vec())
    }

    #[tokio::test]
    async fn spool_delivery_serves_bytes_and_rejects_bad_ranges() {
        let delivery = delivery(Arc::new(StubFetcher::new(b"0123456789")), 16);
        let source = key("vid-1");

        let result = delivery
            .deliver(&source, &resolved("https://media.example.test/audio"), None)
            .await
            .unwrap();
        let (status, headers, bytes) = body_bytes(result).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(headers["content-length"], "10");
        assert_eq!(bytes, b"0123456789");

        let error = delivery
            .deliver(
                &source,
                &resolved("https://media.example.test/audio"),
                Some("bytes=99-100".to_owned()),
            )
            .await
            .err()
            .expect("bad range must fail");
        assert!(matches!(
            error,
            DeliveryError::Range {
                content_length: 10,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn a_second_request_for_a_source_reuses_its_spool() {
        let fetcher = Arc::new(StubFetcher::new(b"0123456789"));
        let delivery = delivery(Arc::clone(&fetcher) as Arc<dyn MediaFetcher>, 16);
        let source = key("vid-1");

        let first = delivery
            .deliver(&source, &resolved("https://media.example.test/audio"), None)
            .await
            .unwrap();
        let (_, _, bytes) = body_bytes(first).await;
        assert_eq!(bytes, b"0123456789");

        let second = delivery
            .deliver(&source, &resolved("https://media.example.test/audio"), None)
            .await
            .unwrap();
        let (_, _, bytes) = body_bytes(second).await;
        assert_eq!(bytes, b"0123456789");
        assert_eq!(fetcher.probes(), 1);
    }

    #[tokio::test]
    async fn a_seek_reads_the_cached_spool() {
        let fetcher = Arc::new(StubFetcher::new(b"0123456789"));
        let delivery = delivery(Arc::clone(&fetcher) as Arc<dyn MediaFetcher>, 16);
        let source = key("vid-1");

        let first = delivery
            .deliver(&source, &resolved("https://media.example.test/audio"), None)
            .await
            .unwrap();
        body_bytes(first).await;

        let seek = delivery
            .deliver(
                &source,
                &resolved("https://media.example.test/audio"),
                Some("bytes=2-4".to_owned()),
            )
            .await
            .unwrap();
        let (status, headers, bytes) = body_bytes(seek).await;
        assert_eq!(status, StatusCode::PARTIAL_CONTENT);
        assert_eq!(headers["content-range"], "bytes 2-4/10");
        assert_eq!(bytes, b"234");
        assert_eq!(fetcher.probes(), 1);
    }

    #[tokio::test]
    async fn a_cached_range_beyond_the_downloaded_prefix_waits_for_the_reader() {
        let release = Arc::new(Notify::new());
        let fetcher = Arc::new(HeldFetcher {
            release: Arc::clone(&release),
        });
        let delivery = delivery(Arc::clone(&fetcher) as Arc<dyn MediaFetcher>, 16);
        let source = key("vid-1");

        let first = delivery
            .deliver(&source, &resolved("https://media.example.test/audio"), None)
            .await
            .unwrap();
        let mut stream = first.body.into_data_stream();
        let chunk = stream.next().await.unwrap().unwrap();
        assert_eq!(&chunk[..], b"0123");
        drop(stream);

        let seek = delivery
            .deliver(
                &source,
                &resolved("https://media.example.test/audio"),
                Some("bytes=6-8".to_owned()),
            )
            .await
            .unwrap();
        release.notify_one();

        let (status, headers, bytes) = body_bytes(seek).await;
        assert_eq!(status, StatusCode::PARTIAL_CONTENT);
        assert_eq!(headers["content-range"], "bytes 6-8/10");
        assert_eq!(bytes, b"678");
    }

    #[tokio::test]
    async fn a_failed_spool_is_not_served() {
        let release = Arc::new(Notify::new());
        let fetcher = Arc::new(HeldFailureFetcher::new(b"0123456789", Arc::clone(&release)));
        let delivery = delivery(Arc::clone(&fetcher) as Arc<dyn MediaFetcher>, 16);
        let source = key("vid-1");

        let failed = delivery
            .deliver(&source, &resolved("https://media.example.test/audio"), None)
            .await
            .unwrap();
        release.notify_one();
        assert!(
            axum::body::to_bytes(failed.body, 1024 * 1024)
                .await
                .is_err()
        );

        let retried = delivery
            .deliver(&source, &resolved("https://media.example.test/audio"), None)
            .await
            .unwrap();
        let (_, _, bytes) = body_bytes(retried).await;
        assert_eq!(bytes, b"0123456789");
    }

    fn spawn_delivery(
        delivery: &Arc<StreamDelivery>,
        source: &SourceKey,
    ) -> tokio::task::JoinHandle<Result<DeliveryResult, DeliveryError>> {
        let delivery = Arc::clone(delivery);
        let source = source.clone();
        tokio::spawn(async move {
            delivery
                .deliver(&source, &resolved("https://media.example.test/audio"), None)
                .await
        })
    }

    #[tokio::test]
    async fn concurrent_deliveries_for_one_source_share_one_spool() {
        let release = Arc::new(Notify::new());
        let (fetcher, mut entered) =
            GatedFetcher::new(b"0123456789", true, false, Arc::clone(&release));
        let fetcher = Arc::new(fetcher);
        let delivery = Arc::new(
            delivery(Arc::clone(&fetcher) as Arc<dyn MediaFetcher>, 16)
                .with_spool_limits(1024 * 1024, 64 * 1024),
        );
        let source = key("vid-1");

        let creator = spawn_delivery(&delivery, &source);
        assert_eq!(entered.recv().await.unwrap(), "probe");
        assert_eq!(entered.recv().await.unwrap(), "open");

        let joiner = spawn_delivery(&delivery, &source);
        assert_eq!(entered.recv().await.unwrap(), "probe");
        release.notify_one();

        let (creator, joiner) = tokio::join!(creator, joiner);
        let creator = creator.unwrap().unwrap();
        let joiner = joiner.unwrap().unwrap();
        assert_eq!(fetcher.probes(), 2);
        assert_eq!(fetcher.opens(), 1);
        assert_eq!(body_bytes(creator).await.2, b"0123456789");
        assert_eq!(body_bytes(joiner).await.2, b"0123456789");
    }

    #[tokio::test]
    async fn a_concurrent_warm_and_delivery_share_one_spool() {
        let release = Arc::new(Notify::new());
        let (fetcher, mut entered) =
            GatedFetcher::new(b"0123456789", true, false, Arc::clone(&release));
        let fetcher = Arc::new(fetcher);
        let delivery = Arc::new(
            delivery(Arc::clone(&fetcher) as Arc<dyn MediaFetcher>, 16)
                .with_spool_limits(1024 * 1024, 64 * 1024),
        );
        let source = key("vid-1");

        let creator = {
            let delivery = Arc::clone(&delivery);
            let source = source.clone();
            tokio::spawn(async move {
                delivery
                    .warm(&source, &resolved("https://media.example.test/audio"))
                    .await
            })
        };
        assert_eq!(entered.recv().await.unwrap(), "probe");
        assert_eq!(entered.recv().await.unwrap(), "open");

        let delivered = spawn_delivery(&delivery, &source);
        assert_eq!(entered.recv().await.unwrap(), "probe");
        release.notify_one();

        let (creator, delivered) = tokio::join!(creator, delivered);
        creator.unwrap().unwrap();
        let delivered = delivered.unwrap().unwrap();
        assert_eq!(fetcher.probes(), 2);
        assert_eq!(fetcher.opens(), 1);
        assert_eq!(body_bytes(delivered).await.2, b"0123456789");
    }

    #[tokio::test]
    async fn a_cancelled_waiter_does_not_disturb_the_creator() {
        let release = Arc::new(Notify::new());
        let (fetcher, mut entered) =
            GatedFetcher::new(b"0123456789", true, false, Arc::clone(&release));
        let fetcher = Arc::new(fetcher);
        let delivery = Arc::new(
            delivery(Arc::clone(&fetcher) as Arc<dyn MediaFetcher>, 16)
                .with_spool_limits(1024 * 1024, 64 * 1024),
        );
        let source = key("vid-1");

        let creator = spawn_delivery(&delivery, &source);
        assert_eq!(entered.recv().await.unwrap(), "probe");
        assert_eq!(entered.recv().await.unwrap(), "open");

        let waiter = spawn_delivery(&delivery, &source);
        assert_eq!(entered.recv().await.unwrap(), "probe");
        waiter.abort();
        assert!(waiter.await.is_err());
        release.notify_one();

        let created = creator.await.unwrap().unwrap();
        assert_eq!(body_bytes(created).await.2, b"0123456789");

        let third = delivery
            .deliver(&source, &resolved("https://media.example.test/audio"), None)
            .await
            .unwrap();
        assert_eq!(body_bytes(third).await.2, b"0123456789");
        assert_eq!(fetcher.opens(), 1);
    }

    #[tokio::test]
    async fn a_cancelled_creator_leaves_the_waiter_to_create() {
        let release = Arc::new(Notify::new());
        let (fetcher, mut entered) =
            GatedFetcher::new(b"0123456789", true, false, Arc::clone(&release));
        let fetcher = Arc::new(fetcher);
        let delivery = Arc::new(
            delivery(Arc::clone(&fetcher) as Arc<dyn MediaFetcher>, 16)
                .with_spool_limits(1024 * 1024, 64 * 1024),
        );
        let source = key("vid-1");

        let creator = spawn_delivery(&delivery, &source);
        assert_eq!(entered.recv().await.unwrap(), "probe");
        assert_eq!(entered.recv().await.unwrap(), "open");

        let waiter = spawn_delivery(&delivery, &source);
        assert_eq!(entered.recv().await.unwrap(), "probe");
        creator.abort();
        assert!(creator.await.is_err());

        let created = waiter.await.unwrap().unwrap();
        assert_eq!(body_bytes(created).await.2, b"0123456789");
        assert!(delivery.is_cached(&source));
        assert_eq!(fetcher.opens(), 2);
    }

    #[tokio::test]
    async fn a_failed_creation_leaves_the_key_retryable() {
        let release = Arc::new(Notify::new());
        let (fetcher, mut entered) =
            GatedFetcher::new(b"0123456789", true, true, Arc::clone(&release));
        let fetcher = Arc::new(fetcher);
        let delivery = Arc::new(
            delivery(Arc::clone(&fetcher) as Arc<dyn MediaFetcher>, 16)
                .with_spool_limits(1024 * 1024, 64 * 1024),
        );
        let source = key("vid-1");

        let creator = spawn_delivery(&delivery, &source);
        assert_eq!(entered.recv().await.unwrap(), "probe");
        assert_eq!(entered.recv().await.unwrap(), "open");

        let waiter = spawn_delivery(&delivery, &source);
        assert_eq!(entered.recv().await.unwrap(), "probe");
        release.notify_one();

        let failed = creator.await.unwrap();
        assert!(matches!(failed, Err(DeliveryError::Media(_))));
        let created = waiter.await.unwrap().unwrap();
        assert_eq!(body_bytes(created).await.2, b"0123456789");
        assert!(delivery.is_cached(&source));
        assert_eq!(fetcher.probes(), 2);
        assert_eq!(fetcher.opens(), 2);
    }

    #[tokio::test]
    async fn a_failed_warm_spool_is_replaced_for_a_cold_client() {
        let probe_release = Arc::new(Notify::new());
        let failure_release = Arc::new(Notify::new());
        let (fetcher, mut entered) = ColdFailureFetcher::new(
            b"0123456789",
            Arc::clone(&probe_release),
            Arc::clone(&failure_release),
        );
        let fetcher = Arc::new(fetcher);
        let delivery = Arc::new(delivery(Arc::clone(&fetcher) as Arc<dyn MediaFetcher>, 16));
        let source = key("vid-1");

        let client = spawn_delivery(&delivery, &source);
        assert_eq!(entered.recv().await.unwrap(), "probe");

        let warm = {
            let delivery = Arc::clone(&delivery);
            let source = source.clone();
            tokio::spawn(async move {
                delivery
                    .warm(&source, &resolved("https://media.example.test/audio"))
                    .await
            })
        };
        assert_eq!(entered.recv().await.unwrap(), "probe");
        warm.await.unwrap().unwrap();
        assert_eq!(fetcher.opens(), 1);

        let cached = delivery.spool.lookup(&source).expect("a live warm spool");
        let mut reader = cached.spool.reader(0, 1).await.unwrap();
        failure_release.notify_one();
        assert!(reader.next_chunk().await.is_err());
        assert!(cached.spool.failed());
        drop(reader);
        drop(cached);

        probe_release.notify_one();
        let delivered = client.await.unwrap().unwrap();
        assert_eq!(body_bytes(delivered).await.2, b"0123456789");
        assert_eq!(fetcher.opens(), 2);
        assert!(delivery.is_cached(&source));
    }

    #[tokio::test]
    async fn warm_is_a_no_op_for_a_cached_key() {
        let fetcher = Arc::new(StubFetcher::new(b"0123456789"));
        let delivery = delivery(Arc::clone(&fetcher) as Arc<dyn MediaFetcher>, 16);
        let source = key("vid-1");

        let first = delivery
            .deliver(&source, &resolved("https://media.example.test/audio"), None)
            .await
            .unwrap();
        body_bytes(first).await;

        delivery
            .warm(&source, &resolved("https://media.example.test/audio"))
            .await
            .unwrap();
        assert_eq!(fetcher.probes(), 1);

        let other = key("vid-2");
        delivery
            .warm(&other, &resolved("https://media.example.test/audio"))
            .await
            .unwrap();
        assert_eq!(fetcher.probes(), 2);
        assert!(delivery.cached(&other, None).await.is_some());
    }

    #[tokio::test]
    async fn local_sources_are_not_cached() {
        let path =
            std::env::temp_dir().join(format!("whio-delivery-local-{}", uuid::Uuid::now_v7()));
        tokio::fs::write(&path, b"0123456789").await.unwrap();
        let media = LocalMedia::new(path.clone(), 10, Some("audio/flac".to_owned())).unwrap();
        let delivery = delivery(Arc::new(HangingFetcher), 16);
        let source = local_key(&path.to_string_lossy());

        let result = delivery
            .deliver(&source, &ResolvedPlayback::Local(media), None)
            .await
            .unwrap();
        body_bytes(result).await;

        assert!(delivery.cached(&source, None).await.is_none());

        tokio::fs::remove_file(path).await.unwrap();
    }

    #[tokio::test]
    async fn a_full_budget_evicts_the_least_recently_used_idle_entry() {
        let now = Arc::new(Mutex::new(chrono::Utc::now()));
        let handle = Arc::clone(&now);
        let fetcher = Arc::new(StubFetcher::new(b"0123456789"));
        let delivery = delivery(Arc::clone(&fetcher) as Arc<dyn MediaFetcher>, 16)
            .with_spool_limits(1024 * 1024, 2 * 64 * 1024)
            .with_spool_idle(Duration::from_secs(3600))
            .with_spool_clock(Clock::from_fn(move || *handle.lock()));
        let source = resolved("https://media.example.test/audio");

        let first = delivery
            .deliver(&key("vid-1"), &source, None)
            .await
            .unwrap();
        body_bytes(first).await;
        *now.lock() += chrono::Duration::seconds(1);
        let second = delivery
            .deliver(&key("vid-2"), &source, None)
            .await
            .unwrap();
        body_bytes(second).await;
        *now.lock() += chrono::Duration::seconds(1);

        let third = delivery
            .deliver(&key("vid-3"), &source, None)
            .await
            .unwrap();
        let (_, _, bytes) = body_bytes(third).await;
        assert_eq!(bytes, b"0123456789");
        assert!(delivery.cached(&key("vid-1"), None).await.is_none());
        assert!(delivery.cached(&key("vid-2"), None).await.is_some());
    }

    #[tokio::test]
    async fn a_full_budget_with_only_busy_entries_is_overloaded() {
        let fetcher = Arc::new(HangingFetcher);
        let delivery = delivery(Arc::clone(&fetcher) as Arc<dyn MediaFetcher>, 16)
            .with_spool_limits(1024 * 1024, 2 * 64 * 1024);
        let source = resolved("https://media.example.test/audio");

        let first = delivery
            .deliver(&key("vid-1"), &source, None)
            .await
            .unwrap();
        let second = delivery
            .deliver(&key("vid-2"), &source, None)
            .await
            .unwrap();

        let error = delivery
            .deliver(&key("vid-3"), &source, None)
            .await
            .err()
            .expect("a full budget of busy spools must overload");
        assert!(matches!(error, DeliveryError::Overloaded));

        drop(first);
        drop(second);
    }

    #[tokio::test]
    async fn session_limit_rejects_the_second_concurrent_delivery() {
        let delivery = delivery(Arc::new(HangingFetcher), 1);
        let source = key("vid-1");
        let resolved = resolved("https://media.example.test/audio");

        let first = delivery.deliver(&source, &resolved, None).await.unwrap();
        let error = delivery
            .deliver(&source, &resolved, None)
            .await
            .err()
            .expect("second delivery must fail");
        assert!(matches!(error, DeliveryError::Overloaded));

        drop(first);
        delivery
            .deliver(&source, &resolved, None)
            .await
            .expect("the released session must deliver");
    }

    #[tokio::test]
    async fn a_cached_hit_with_every_session_busy_is_overloaded_before_reader_setup() {
        let dir =
            std::env::temp_dir().join(format!("whio-delivery-session-{}", uuid::Uuid::now_v7()));
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let release = Arc::new(Notify::new());
        let delivery = StreamDelivery::new(
            Arc::new(HeldFetcher {
                release: Arc::clone(&release),
            }),
            dir.clone(),
            1,
        );
        let busy = resolved("https://media.example.test/audio");
        let held = delivery
            .deliver(&key("vid-busy"), &busy, None)
            .await
            .unwrap();

        let source = key("vid-1");
        delivery.warm(&source, &busy).await.unwrap();

        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path
                .extension()
                .is_some_and(|extension| extension == "part")
            {
                std::fs::remove_file(&path).unwrap();
            }
        }

        let busy_result = delivery.cached(&source, Some("bytes=0-0".to_owned())).await;
        assert!(matches!(busy_result, Some(Err(DeliveryError::Overloaded))));

        drop(held);
        let free_result = delivery.cached(&source, Some("bytes=0-0".to_owned())).await;
        assert!(matches!(free_result, Some(Err(DeliveryError::Spool))));

        tokio::fs::remove_dir_all(&dir).await.unwrap();
    }

    #[tokio::test]
    async fn a_cold_invalid_range_is_rejected_before_creating_a_spool() {
        let release = Arc::new(Notify::new());
        let (fetcher, _entered) = GatedFetcher::new(b"0123456789", false, false, release);
        let fetcher = Arc::new(fetcher);
        let delivery = delivery(Arc::clone(&fetcher) as Arc<dyn MediaFetcher>, 16);
        let source = key("vid-1");

        let error = delivery
            .deliver(
                &source,
                &resolved("https://media.example.test/audio"),
                Some("bytes=99-100".to_owned()),
            )
            .await
            .err()
            .expect("an invalid range must fail");
        assert!(matches!(
            error,
            DeliveryError::Range {
                content_length: 10,
                ..
            }
        ));
        assert_eq!(fetcher.probes(), 1);
        assert_eq!(fetcher.opens(), 0);
        assert!(!delivery.is_cached(&source));
    }

    #[tokio::test]
    async fn a_cold_invalid_range_does_not_evict_a_cached_spool() {
        let release = Arc::new(Notify::new());
        let (fetcher, _entered) = GatedFetcher::new(b"0123456789", false, false, release);
        let fetcher = Arc::new(fetcher);
        let delivery = delivery(Arc::clone(&fetcher) as Arc<dyn MediaFetcher>, 16)
            .with_spool_limits(1024 * 1024, 64 * 1024);
        let busy = key("vid-busy");
        delivery
            .warm(&busy, &resolved("https://media.example.test/audio"))
            .await
            .unwrap();
        assert!(delivery.is_cached(&busy));
        assert_eq!(fetcher.opens(), 1);

        let error = delivery
            .deliver(
                &key("vid-1"),
                &resolved("https://media.example.test/audio"),
                Some("bytes=99-100".to_owned()),
            )
            .await
            .err()
            .expect("an invalid range must fail");
        assert!(matches!(error, DeliveryError::Range { .. }));
        assert!(delivery.is_cached(&busy));
        assert_eq!(fetcher.opens(), 1);
    }

    #[tokio::test]
    async fn a_zero_length_probe_is_an_invalid_content_range() {
        let release = Arc::new(Notify::new());
        let (fetcher, _entered) = GatedFetcher::new(b"", false, false, release);
        let fetcher = Arc::new(fetcher);
        let delivery = delivery(Arc::clone(&fetcher) as Arc<dyn MediaFetcher>, 16);
        let source = key("vid-1");

        let error = delivery
            .deliver(&source, &resolved("https://media.example.test/audio"), None)
            .await
            .err()
            .expect("a zero length probe must fail");
        assert!(matches!(
            error,
            DeliveryError::Media(MediaFetchError::InvalidContentRange)
        ));
        assert_eq!(fetcher.opens(), 0);
        assert!(!delivery.is_cached(&source));
    }

    #[tokio::test]
    async fn a_local_target_is_served_from_disk_without_a_fetcher() {
        let path =
            std::env::temp_dir().join(format!("whio-delivery-local-{}", uuid::Uuid::now_v7()));
        tokio::fs::write(&path, b"0123456789").await.unwrap();
        let media = LocalMedia::new(path.clone(), 10, Some("audio/flac".to_owned())).unwrap();
        let delivery = delivery(Arc::new(HangingFetcher), 16);

        let result = delivery
            .deliver(
                &local_key(&path.to_string_lossy()),
                &ResolvedPlayback::Local(media),
                Some("bytes=2-4".to_owned()),
            )
            .await
            .unwrap();

        let (status, headers, bytes) = body_bytes(result).await;
        assert_eq!(status, StatusCode::PARTIAL_CONTENT);
        assert_eq!(headers["content-range"], "bytes 2-4/10");
        assert_eq!(headers["content-type"], "audio/flac");
        assert_eq!(bytes, b"234");

        tokio::fs::remove_file(path).await.unwrap();
    }
}
