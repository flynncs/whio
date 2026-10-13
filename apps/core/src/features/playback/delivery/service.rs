use std::{future::Future, sync::Arc, time::Duration};

use async_trait::async_trait;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use super::{PlaybackByteStream, local, session, spool::SpoolStrategy, spool_cache::SpoolKey};
use crate::{
    features::music_library::domain::TrackId,
    features::playback::media::{
        ByteRange, ConversionByteStream, ConversionInput, ConversionTarget, Converter,
        MediaFetcher, MediaInfo,
    },
    features::playback::{
        ClientFormats, DeliveryError, DeliveryResult, LocalMedia, PlayableMedia, PlaybackDelivery,
        Representation, ResolvedPlayback, SourceAudio, SourceKey, StreamRequest, TrackDurations,
        representation::{is_lossless_codec, is_lossless_content_type},
    },
    platform::clock::Clock,
};

pub const DEFAULT_MAX_SESSIONS: usize = 16;

/// bounded spool and local-file delivery. owns bytes, ranges, session limits
/// and cancellation.
pub struct StreamDelivery {
    spool: SpoolStrategy,
    sessions: Arc<Semaphore>,
    converter: Option<Arc<dyn Converter>>,
    formats: ClientFormats,
    durations: Option<Arc<dyn TrackDurations>>,
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
            converter: None,
            formats: ClientFormats::default(),
            durations: None,
        }
    }

    pub fn with_converter(mut self, converter: Arc<dyn Converter>) -> Self {
        self.converter = Some(converter);
        self
    }

    pub fn with_client_formats(mut self, formats: ClientFormats) -> Self {
        self.formats = formats;
        self
    }

    pub fn with_track_durations(mut self, durations: Arc<dyn TrackDurations>) -> Self {
        self.durations = Some(durations);
        self
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

    async fn track_duration(&self, track: &TrackId) -> Result<Option<u64>, DeliveryError> {
        let Some(durations) = self.durations.as_ref() else {
            return Ok(None);
        };
        durations
            .duration_ms(track)
            .await
            .map_err(DeliveryError::Duration)
    }

    async fn deliver_representation(
        &self,
        key: &SourceKey,
        resolved: &ResolvedPlayback,
        request: &StreamRequest,
        track: &TrackId,
        client_range: Option<&str>,
    ) -> Result<DeliveryResult, DeliveryError> {
        match resolved {
            ResolvedPlayback::Remote(media) => {
                let source = remote_source(media);
                match Representation::resolve(request, &self.formats, &source) {
                    Representation::Original => self.spool.open(key, media, client_range).await,
                    representation => {
                        let duration = media.metadata().duration_ms();
                        let open = || async {
                            let cached = self.spool.source(key, media).await?;
                            let reader = cached.spool.reader(0, cached.info.content_length).await?;
                            let stream = PlaybackByteStream::start(
                                reader,
                                cached.spool.id(),
                                ByteRange {
                                    start: 0,
                                    end: cached.info.content_length - 1,
                                },
                            );
                            let input: ConversionByteStream = Box::pin(stream);
                            Ok(ConversionInput::Stream(input))
                        };
                        self.convert(
                            ConversionRequest {
                                key,
                                representation,
                                source: &source,
                                resolved_duration_ms: duration,
                                track,
                            },
                            open,
                            client_range,
                        )
                        .await
                    }
                }
            }
            ResolvedPlayback::Local(media) => {
                let source = local_source(media);
                match Representation::resolve(request, &self.formats, &source) {
                    Representation::Original => {
                        local::serve(media, client_range.map(str::to_owned)).await
                    }
                    representation => {
                        let path = media.path().to_path_buf();
                        let open = move || async move { Ok(ConversionInput::Path(path)) };
                        self.convert(
                            ConversionRequest {
                                key,
                                representation,
                                source: &source,
                                resolved_duration_ms: None,
                                track,
                            },
                            open,
                            client_range,
                        )
                        .await
                    }
                }
            }
        }
    }

    async fn convert<F, Fut>(
        &self,
        request: ConversionRequest<'_>,
        open_input: F,
        client_range: Option<&str>,
    ) -> Result<DeliveryResult, DeliveryError>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<ConversionInput, DeliveryError>>,
    {
        let ConversionRequest {
            key,
            representation,
            source,
            resolved_duration_ms,
            track,
        } = request;
        let spool_key = SpoolKey::new(key, representation);
        if let Some(cached) = self.spool.lookup(&spool_key) {
            return self.spool.serve(&spool_key, &cached, client_range).await;
        }
        let Some(converter) = self
            .converter
            .as_ref()
            .filter(|converter| converter.available())
        else {
            return Err(DeliveryError::ConversionUnavailable);
        };
        let duration_ms = match resolved_duration_ms {
            Some(duration) => Some(duration),
            None => self.track_duration(track).await?,
        };
        let Some(duration_ms) = duration_ms else {
            return Err(DeliveryError::MissingDuration);
        };
        let estimate = representation.estimate_bytes(duration_ms, source.bitrate_kbps);
        let info = MediaInfo {
            content_length: estimate,
            supports_ranges: true,
            content_type: representation.content_type().map(str::to_owned),
        };
        SpoolStrategy::validate(&info, client_range)?;
        let cached = self
            .spool
            .converted(
                &spool_key,
                info,
                converter.as_ref(),
                ConversionTarget { representation },
                open_input,
            )
            .await?;
        self.spool.serve(&spool_key, &cached, client_range).await
    }
}

struct ConversionRequest<'a> {
    key: &'a SourceKey,
    representation: Representation,
    source: &'a SourceAudio,
    resolved_duration_ms: Option<u64>,
    track: &'a TrackId,
}

fn remote_source(media: &PlayableMedia) -> SourceAudio {
    let metadata = media.metadata();
    SourceAudio {
        codec: metadata.codec().map(str::to_owned),
        bitrate_kbps: metadata.bitrate_kbps().filter(|kbps| *kbps > 0.0),
        lossless: metadata.codec().is_some_and(is_lossless_codec),
    }
}

fn local_source(media: &LocalMedia) -> SourceAudio {
    let codec = media.codec().map(str::to_owned);
    SourceAudio {
        bitrate_kbps: media.bitrate_kbps().filter(|kbps| *kbps > 0.0),
        lossless: codec.as_deref().is_some_and(is_lossless_codec)
            || media.content_type().is_some_and(is_lossless_content_type),
        codec,
    }
}

#[async_trait]
impl PlaybackDelivery for StreamDelivery {
    async fn deliver(
        &self,
        key: &SourceKey,
        resolved: &ResolvedPlayback,
        request: &StreamRequest,
        track: &TrackId,
        client_range: Option<String>,
    ) -> Result<DeliveryResult, DeliveryError> {
        let permit = self.acquire_session()?;
        let mut delivery = self
            .deliver_representation(key, resolved, request, track, client_range.as_deref())
            .await?;
        // the permit travels with the response body, so a session is only
        // released once the client has finished with it.
        delivery.body = session::hold(delivery.body, permit);
        Ok(delivery)
    }

    async fn cached(
        &self,
        key: &SourceKey,
        request: &StreamRequest,
        client_range: Option<String>,
    ) -> Option<Result<DeliveryResult, DeliveryError>> {
        if !Representation::cannot_convert(request, &self.formats) {
            return None;
        }
        let key = SpoolKey::original(key);
        let cached = self.spool.lookup(&key)?;
        let permit = match self.acquire_session() {
            Ok(permit) => permit,
            Err(error) => return Some(Err(error)),
        };
        match self
            .spool
            .serve(&key, &cached, client_range.as_deref())
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
        features::music_library::domain::{ProviderId, SourceIdentity, SourceScope, TrackId},
        features::playback::media::{MediaBody, MediaFetchError, MediaInfo},
        features::playback::{
            AacMode, LocalMedia, MediaMetadata, PlayableMedia, PlaybackSource, PlaybackUrl,
            TrackDurationError, TrackDurations,
        },
        platform::clock::Clock,
    };

    fn track() -> TrackId {
        TrackId::new()
    }

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
            .deliver(
                &source,
                &resolved("https://media.example.test/audio"),
                &StreamRequest::default(),
                &track(),
                None,
            )
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
                &StreamRequest::default(),
                &track(),
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
            .deliver(
                &source,
                &resolved("https://media.example.test/audio"),
                &StreamRequest::default(),
                &track(),
                None,
            )
            .await
            .unwrap();
        let (_, _, bytes) = body_bytes(first).await;
        assert_eq!(bytes, b"0123456789");

        let second = delivery
            .deliver(
                &source,
                &resolved("https://media.example.test/audio"),
                &StreamRequest::default(),
                &track(),
                None,
            )
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
            .deliver(
                &source,
                &resolved("https://media.example.test/audio"),
                &StreamRequest::default(),
                &track(),
                None,
            )
            .await
            .unwrap();
        body_bytes(first).await;

        let seek = delivery
            .deliver(
                &source,
                &resolved("https://media.example.test/audio"),
                &StreamRequest::default(),
                &track(),
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
            .deliver(
                &source,
                &resolved("https://media.example.test/audio"),
                &StreamRequest::default(),
                &track(),
                None,
            )
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
                &StreamRequest::default(),
                &track(),
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
            .deliver(
                &source,
                &resolved("https://media.example.test/audio"),
                &StreamRequest::default(),
                &track(),
                None,
            )
            .await
            .unwrap();
        release.notify_one();
        assert!(
            axum::body::to_bytes(failed.body, 1024 * 1024)
                .await
                .is_err()
        );

        let retried = delivery
            .deliver(
                &source,
                &resolved("https://media.example.test/audio"),
                &StreamRequest::default(),
                &track(),
                None,
            )
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
                .deliver(
                    &source,
                    &resolved("https://media.example.test/audio"),
                    &StreamRequest::default(),
                    &track(),
                    None,
                )
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
            .deliver(
                &source,
                &resolved("https://media.example.test/audio"),
                &StreamRequest::default(),
                &track(),
                None,
            )
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

        let cached = delivery
            .spool
            .lookup(&SpoolKey::original(&source))
            .expect("a live warm spool");
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
            .deliver(
                &source,
                &resolved("https://media.example.test/audio"),
                &StreamRequest::default(),
                &track(),
                None,
            )
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
        assert!(
            delivery
                .cached(&other, &StreamRequest::default(), None)
                .await
                .is_some()
        );
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
            .deliver(
                &source,
                &ResolvedPlayback::Local(media),
                &StreamRequest::default(),
                &track(),
                None,
            )
            .await
            .unwrap();
        body_bytes(result).await;

        assert!(
            delivery
                .cached(&source, &StreamRequest::default(), None)
                .await
                .is_none()
        );

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
            .deliver(
                &key("vid-1"),
                &source,
                &StreamRequest::default(),
                &track(),
                None,
            )
            .await
            .unwrap();
        body_bytes(first).await;
        *now.lock() += chrono::Duration::seconds(1);
        let second = delivery
            .deliver(
                &key("vid-2"),
                &source,
                &StreamRequest::default(),
                &track(),
                None,
            )
            .await
            .unwrap();
        body_bytes(second).await;
        *now.lock() += chrono::Duration::seconds(1);

        let third = delivery
            .deliver(
                &key("vid-3"),
                &source,
                &StreamRequest::default(),
                &track(),
                None,
            )
            .await
            .unwrap();
        let (_, _, bytes) = body_bytes(third).await;
        assert_eq!(bytes, b"0123456789");
        assert!(
            delivery
                .cached(&key("vid-1"), &StreamRequest::default(), None)
                .await
                .is_none()
        );
        assert!(
            delivery
                .cached(&key("vid-2"), &StreamRequest::default(), None)
                .await
                .is_some()
        );
    }

    #[tokio::test]
    async fn a_full_budget_with_only_busy_entries_is_overloaded() {
        let fetcher = Arc::new(HangingFetcher);
        let delivery = delivery(Arc::clone(&fetcher) as Arc<dyn MediaFetcher>, 16)
            .with_spool_limits(1024 * 1024, 2 * 64 * 1024);
        let source = resolved("https://media.example.test/audio");

        let first = delivery
            .deliver(
                &key("vid-1"),
                &source,
                &StreamRequest::default(),
                &track(),
                None,
            )
            .await
            .unwrap();
        let second = delivery
            .deliver(
                &key("vid-2"),
                &source,
                &StreamRequest::default(),
                &track(),
                None,
            )
            .await
            .unwrap();

        let error = delivery
            .deliver(
                &key("vid-3"),
                &source,
                &StreamRequest::default(),
                &track(),
                None,
            )
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

        let first = delivery
            .deliver(
                &source,
                &resolved,
                &StreamRequest::default(),
                &track(),
                None,
            )
            .await
            .unwrap();
        let error = delivery
            .deliver(
                &source,
                &resolved,
                &StreamRequest::default(),
                &track(),
                None,
            )
            .await
            .err()
            .expect("second delivery must fail");
        assert!(matches!(error, DeliveryError::Overloaded));

        drop(first);
        delivery
            .deliver(
                &source,
                &resolved,
                &StreamRequest::default(),
                &track(),
                None,
            )
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
            .deliver(
                &key("vid-busy"),
                &busy,
                &StreamRequest::default(),
                &track(),
                None,
            )
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

        let busy_result = delivery
            .cached(
                &source,
                &StreamRequest::default(),
                Some("bytes=0-0".to_owned()),
            )
            .await;
        assert!(matches!(busy_result, Some(Err(DeliveryError::Overloaded))));

        drop(held);
        let free_result = delivery
            .cached(
                &source,
                &StreamRequest::default(),
                Some("bytes=0-0".to_owned()),
            )
            .await;
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
                &StreamRequest::default(),
                &track(),
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
                &StreamRequest::default(),
                &track(),
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
            .deliver(
                &source,
                &resolved("https://media.example.test/audio"),
                &StreamRequest::default(),
                &track(),
                None,
            )
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
                &StreamRequest::default(),
                &track(),
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

    struct FakeConverter {
        starts: AtomicUsize,
        representations: Mutex<Vec<Representation>>,
        available: bool,
        fail: bool,
        gate: Option<Arc<Notify>>,
        failure_gate: Option<Arc<Notify>>,
        entered: mpsc::UnboundedSender<&'static str>,
    }

    impl FakeConverter {
        fn new(available: bool, fail: bool) -> (Self, mpsc::UnboundedReceiver<&'static str>) {
            Self::gated(available, fail, None)
        }

        fn failing_when(gate: Arc<Notify>) -> (Self, mpsc::UnboundedReceiver<&'static str>) {
            let (mut converter, receiver) = Self::new(true, true);
            converter.failure_gate = Some(gate);
            (converter, receiver)
        }

        fn gated(
            available: bool,
            fail: bool,
            gate: Option<Arc<Notify>>,
        ) -> (Self, mpsc::UnboundedReceiver<&'static str>) {
            let (entered, receiver) = mpsc::unbounded_channel();
            (
                Self {
                    starts: AtomicUsize::new(0),
                    representations: Mutex::new(Vec::new()),
                    available,
                    fail,
                    gate,
                    failure_gate: None,
                    entered,
                },
                receiver,
            )
        }

        fn starts(&self) -> usize {
            self.starts.load(Ordering::SeqCst)
        }

        fn representations(&self) -> Vec<Representation> {
            self.representations.lock().clone()
        }

        fn only_representation(&self) -> Representation {
            let representations = self.representations();
            assert_eq!(representations.len(), 1, "{representations:?}");
            representations[0]
        }
    }

    #[async_trait]
    impl Converter for FakeConverter {
        fn available(&self) -> bool {
            self.available
        }

        async fn start(
            &self,
            input: ConversionInput,
            target: ConversionTarget,
        ) -> Result<MediaBody, DeliveryError> {
            self.starts.fetch_add(1, Ordering::SeqCst);
            self.representations.lock().push(target.representation);
            let _ = self.entered.send("start");
            if let Some(gate) = &self.gate {
                gate.notified().await;
            }
            if self.fail {
                let (sender, receiver) = mpsc::channel(1);
                let gate = self.failure_gate.clone();
                tokio::spawn(async move {
                    if let Some(gate) = gate {
                        gate.notified().await;
                    }
                    let _ = sender.send(Err(MediaFetchError::Conversion)).await;
                });
                return Ok(MediaBody {
                    content_length: None,
                    chunks: Box::pin(tokio_stream::wrappers::ReceiverStream::new(receiver)),
                });
            }
            let stream: ConversionByteStream = match input {
                ConversionInput::Stream(stream) => stream,
                ConversionInput::Path(_) => Box::pin(tokio_stream::empty()),
            };
            let forwarded = stream.map(|chunk| chunk.map_err(|_| MediaFetchError::Conversion));
            Ok(MediaBody {
                content_length: None,
                chunks: Box::pin(forwarded),
            })
        }
    }

    #[derive(Default)]
    struct StubDurations {
        duration_ms: Option<u64>,
        fail: bool,
        calls: AtomicUsize,
    }

    impl StubDurations {
        fn returning(duration_ms: u64) -> Self {
            Self {
                duration_ms: Some(duration_ms),
                ..Self::default()
            }
        }

        fn failing() -> Self {
            Self {
                fail: true,
                ..Self::default()
            }
        }

        fn calls(&self) -> usize {
            self.calls.load(Ordering::SeqCst)
        }
    }

    #[async_trait]
    impl TrackDurations for StubDurations {
        async fn duration_ms(&self, _track: &TrackId) -> Result<Option<u64>, TrackDurationError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if self.fail {
                return Err(TrackDurationError::Library);
            }
            Ok(self.duration_ms)
        }
    }

    struct NeverFetcher {
        probes: AtomicUsize,
    }

    #[async_trait]
    impl MediaFetcher for NeverFetcher {
        async fn probe(&self, _media: &PlayableMedia) -> Result<MediaInfo, MediaFetchError> {
            self.probes.fetch_add(1, Ordering::SeqCst);
            Err(MediaFetchError::RangesUnsupported)
        }

        async fn open_continuous(
            &self,
            _media: &PlayableMedia,
        ) -> Result<MediaBody, MediaFetchError> {
            panic!("an unavailable conversion must not open the source")
        }
    }

    fn remote_media(
        duration_ms: Option<u64>,
        codec: &str,
        bitrate: Option<f64>,
    ) -> ResolvedPlayback {
        ResolvedPlayback::Remote(PlayableMedia::new(
            PlaybackUrl::new("https://media.example.test/audio".to_owned()).unwrap(),
            HashMap::new(),
            None,
            MediaMetadata::new(None, None, Some(codec.to_owned()), bitrate, duration_ms).unwrap(),
        ))
    }

    async fn local_media(
        codec: Option<&str>,
        bitrate: Option<f64>,
        content_type: &str,
    ) -> ResolvedPlayback {
        let path = std::env::temp_dir().join(format!("whio-l41-local-{}", uuid::Uuid::now_v7()));
        tokio::fs::write(&path, b"local").await.unwrap();
        ResolvedPlayback::Local(
            LocalMedia::new(path, 5, Some(content_type.to_owned()))
                .unwrap()
                .with_audio(codec.map(str::to_owned), bitrate),
        )
    }

    fn aac_request() -> StreamRequest {
        StreamRequest {
            format: Some("aac".to_owned()),
            ..StreamRequest::default()
        }
    }

    fn converted_estimate(duration_ms: u64) -> u64 {
        Representation::Aac(AacMode::Encode { bitrate_kbps: 256 }).estimate_bytes(duration_ms, None)
    }

    #[tokio::test]
    async fn a_converted_response_delivers_bytes_before_the_source_finishes() {
        let release = Arc::new(Notify::new());
        let (converter, _entered) = FakeConverter::new(true, false);
        let converter = Arc::new(converter);
        let delivery = StreamDelivery::new(
            Arc::new(HeldFetcher {
                release: Arc::clone(&release),
            }),
            std::env::temp_dir(),
            16,
        )
        .with_converter(Arc::clone(&converter) as Arc<dyn Converter>);
        let source = key("vid-1");

        let result = delivery
            .deliver(
                &source,
                &remote_media(Some(10_000), "opus", Some(128.0)),
                &aac_request(),
                &track(),
                None,
            )
            .await
            .unwrap();
        let mut stream = result.body.into_data_stream();
        let chunk = stream.next().await.unwrap().unwrap();
        assert_eq!(&chunk[..], b"0123");

        drop(stream);
        release.notify_one();
        assert_eq!(converter.starts(), 1);
    }

    #[tokio::test]
    async fn a_converted_range_past_the_produced_prefix_is_pending_until_released() {
        let release = Arc::new(Notify::new());
        let (converter, _entered) = FakeConverter::new(true, false);
        let delivery = StreamDelivery::new(
            Arc::new(HeldFetcher {
                release: Arc::clone(&release),
            }),
            std::env::temp_dir(),
            16,
        )
        .with_converter(Arc::new(converter) as Arc<dyn Converter>);
        let source = key("vid-1");
        let media = remote_media(Some(10_000), "opus", Some(128.0));
        let request = aac_request();

        let full = delivery
            .deliver(&source, &media, &request, &track(), None)
            .await
            .unwrap();
        let mut stream = full.body.into_data_stream();
        assert_eq!(&stream.next().await.unwrap().unwrap()[..], b"0123");
        drop(stream);

        let seek = delivery
            .deliver(
                &source,
                &media,
                &request,
                &track(),
                Some("bytes=4-9".to_owned()),
            )
            .await
            .unwrap();
        let status = seek.status;
        let headers = seek.headers;
        let mut stream = seek.body.into_data_stream();
        let mut next = Box::pin(stream.next());
        std::future::poll_fn(|cx| {
            assert!(
                next.as_mut().poll(cx).is_pending(),
                "the beyond-prefix range must wait for the source"
            );
            std::task::Poll::Ready(())
        })
        .await;
        drop(next);

        release.notify_one();
        let mut collected = Vec::new();
        while let Some(chunk) = stream.next().await {
            collected.extend_from_slice(&chunk.unwrap());
        }
        assert_eq!(status, StatusCode::PARTIAL_CONTENT);
        assert_eq!(
            headers["content-range"],
            format!("bytes 4-9/{}", converted_estimate(10_000))
        );
        assert_eq!(collected, b"456789");
    }

    #[tokio::test]
    async fn a_range_inside_the_padding_returns_zeros() {
        let release = Arc::new(Notify::new());
        let (converter, _entered) = FakeConverter::new(true, false);
        let delivery = StreamDelivery::new(
            Arc::new(HeldFetcher {
                release: Arc::clone(&release),
            }),
            std::env::temp_dir(),
            16,
        )
        .with_converter(Arc::new(converter) as Arc<dyn Converter>);
        let source = key("vid-1");
        let media = remote_media(Some(10_000), "opus", Some(128.0));
        let request = aac_request();
        let estimate = converted_estimate(10_000);

        let full = delivery
            .deliver(&source, &media, &request, &track(), None)
            .await
            .unwrap();
        release.notify_one();
        body_bytes(full).await;

        let padding = delivery
            .deliver(
                &source,
                &media,
                &request,
                &track(),
                Some("bytes=100-199".to_owned()),
            )
            .await
            .unwrap();
        let (status, headers, bytes) = body_bytes(padding).await;
        assert_eq!(status, StatusCode::PARTIAL_CONTENT);
        assert_eq!(
            headers["content-range"],
            format!("bytes 100-199/{estimate}")
        );
        assert_eq!(bytes.len(), 100);
        assert!(bytes.iter().all(|byte| *byte == 0));
    }

    #[tokio::test]
    async fn two_cold_requests_share_one_conversion_run() {
        let gate = Arc::new(Notify::new());
        let (converter, mut entered) = FakeConverter::gated(true, false, Some(Arc::clone(&gate)));
        let converter = Arc::new(converter);
        let delivery = Arc::new(
            StreamDelivery::new(
                Arc::new(StubFetcher::new(b"0123456789")),
                std::env::temp_dir(),
                16,
            )
            .with_converter(Arc::clone(&converter) as Arc<dyn Converter>),
        );
        let request = aac_request();

        let creator = {
            let delivery = Arc::clone(&delivery);
            let request = request.clone();
            tokio::spawn(async move {
                delivery
                    .deliver(
                        &key("vid-1"),
                        &remote_media(Some(10_000), "opus", Some(128.0)),
                        &request,
                        &track(),
                        None,
                    )
                    .await
            })
        };
        assert_eq!(entered.recv().await.unwrap(), "start");
        assert_eq!(converter.starts(), 1);

        let joiner_source = key("vid-1");
        let joiner_media = remote_media(Some(10_000), "opus", Some(128.0));
        let joiner_track = track();
        let mut joiner = Box::pin(delivery.deliver(
            &joiner_source,
            &joiner_media,
            &request,
            &joiner_track,
            None,
        ));
        std::future::poll_fn(|cx| {
            assert!(
                joiner.as_mut().poll(cx).is_pending(),
                "the joiner must await the creator's initializer"
            );
            std::task::Poll::Ready(())
        })
        .await;
        assert_eq!(converter.starts(), 1);

        gate.notify_one();
        let (creator, joiner) = tokio::join!(creator, joiner);
        body_bytes(creator.unwrap().unwrap()).await;
        body_bytes(joiner.unwrap()).await;
        assert_eq!(converter.starts(), 1);
    }

    #[tokio::test]
    async fn a_cancelled_creator_leaves_the_waiter_to_convert() {
        let gate = Arc::new(Notify::new());
        let (converter, mut entered) = FakeConverter::gated(true, false, Some(Arc::clone(&gate)));
        let converter = Arc::new(converter);
        let delivery = Arc::new(
            StreamDelivery::new(
                Arc::new(StubFetcher::new(b"0123456789")),
                std::env::temp_dir(),
                16,
            )
            .with_converter(Arc::clone(&converter) as Arc<dyn Converter>),
        );
        let request = aac_request();

        let creator = {
            let delivery = Arc::clone(&delivery);
            let request = request.clone();
            tokio::spawn(async move {
                delivery
                    .deliver(
                        &key("vid-1"),
                        &remote_media(Some(10_000), "opus", Some(128.0)),
                        &request,
                        &track(),
                        None,
                    )
                    .await
            })
        };
        assert_eq!(entered.recv().await.unwrap(), "start");

        let waiter = {
            let delivery = Arc::clone(&delivery);
            tokio::spawn(async move {
                delivery
                    .deliver(
                        &key("vid-1"),
                        &remote_media(Some(10_000), "opus", Some(128.0)),
                        &request,
                        &track(),
                        None,
                    )
                    .await
            })
        };
        creator.abort();
        assert!(creator.await.is_err());
        assert_eq!(entered.recv().await.unwrap(), "start");

        gate.notify_one();
        let created = waiter.await.unwrap().unwrap();
        body_bytes(created).await;
        assert_eq!(converter.starts(), 2);
    }

    #[tokio::test]
    async fn a_failed_conversion_is_not_served() {
        let failure_gate = Arc::new(Notify::new());
        let (converter, _entered) = FakeConverter::failing_when(Arc::clone(&failure_gate));
        let delivery = delivery(Arc::new(StubFetcher::new(b"0123456789")), 16)
            .with_converter(Arc::new(converter) as Arc<dyn Converter>);
        let source = key("vid-1");

        let result = delivery
            .deliver(
                &source,
                &remote_media(Some(10_000), "opus", Some(128.0)),
                &aac_request(),
                &track(),
                None,
            )
            .await
            .expect("the converted spool is admitted before the failure is released");

        failure_gate.notify_one();
        assert!(axum::body::to_bytes(result.body, 1024).await.is_err());
    }

    #[tokio::test]
    async fn an_unavailable_converter_fails_before_any_source_work() {
        let fetcher = Arc::new(NeverFetcher {
            probes: AtomicUsize::new(0),
        });
        let (converter, _entered) = FakeConverter::new(false, false);
        let converter = Arc::new(converter);
        let delivery = StreamDelivery::new(
            Arc::clone(&fetcher) as Arc<dyn MediaFetcher>,
            std::env::temp_dir(),
            16,
        )
        .with_converter(Arc::clone(&converter) as Arc<dyn Converter>);

        let error = delivery
            .deliver(
                &key("vid-1"),
                &remote_media(Some(10_000), "opus", Some(128.0)),
                &aac_request(),
                &track(),
                None,
            )
            .await
            .err()
            .expect("an unavailable conversion must fail");
        assert!(matches!(error, DeliveryError::ConversionUnavailable));
        assert_eq!(fetcher.probes.load(Ordering::SeqCst), 0);
        assert_eq!(converter.starts(), 0);
    }

    #[tokio::test]
    async fn an_unavailable_converter_leaves_original_delivery_unchanged() {
        let (converter, _entered) = FakeConverter::new(false, false);
        let delivery = delivery(Arc::new(StubFetcher::new(b"0123456789")), 16)
            .with_converter(Arc::new(converter) as Arc<dyn Converter>);

        let result = delivery
            .deliver(
                &key("vid-1"),
                &remote_media(Some(10_000), "opus", Some(128.0)),
                &StreamRequest::default(),
                &track(),
                None,
            )
            .await
            .unwrap();
        assert_eq!(body_bytes(result).await.2, b"0123456789");
    }

    #[tokio::test]
    async fn a_conversion_without_a_duration_is_rejected_without_work() {
        let (converter, _entered) = FakeConverter::new(true, false);
        let converter = Arc::new(converter);
        let delivery = converting_delivery(Arc::clone(&converter));

        let error = delivery
            .deliver(
                &key("vid-1"),
                &remote_media(None, "opus", Some(128.0)),
                &aac_request(),
                &track(),
                None,
            )
            .await
            .err()
            .expect("a conversion without a duration must fail");
        assert!(matches!(error, DeliveryError::MissingDuration));
        assert_eq!(converter.starts(), 0);
    }

    #[tokio::test]
    async fn an_invalid_converted_range_is_rejected_before_conversion() {
        let (converter, _entered) = FakeConverter::new(true, false);
        let converter = Arc::new(converter);
        let delivery = converting_delivery(Arc::clone(&converter));

        let error = delivery
            .deliver(
                &key("vid-1"),
                &remote_media(Some(10_000), "opus", Some(128.0)),
                &aac_request(),
                &track(),
                Some("bytes=99999999-".to_owned()),
            )
            .await
            .err()
            .expect("an unsatisfiable converted range must fail");
        assert!(matches!(error, DeliveryError::Range { .. }));
        assert_eq!(converter.starts(), 0);
    }

    #[tokio::test]
    async fn a_remote_source_without_duration_uses_the_track_duration() {
        let durations = Arc::new(StubDurations::returning(10_000));
        let (converter, _entered) = FakeConverter::new(true, false);
        let converter = Arc::new(converter);
        let delivery = delivery(Arc::new(StubFetcher::new(b"0123456789")), 16)
            .with_converter(Arc::clone(&converter) as Arc<dyn Converter>)
            .with_track_durations(Arc::clone(&durations) as Arc<dyn TrackDurations>);

        let result = delivery
            .deliver(
                &key("vid-1"),
                &remote_media(None, "opus", Some(128.0)),
                &aac_request(),
                &track(),
                None,
            )
            .await
            .unwrap();
        body_bytes(result).await;
        assert_eq!(converter.starts(), 1);
        assert_eq!(durations.calls(), 1);
    }

    #[tokio::test]
    async fn a_missing_track_duration_fails_the_conversion() {
        let durations = Arc::new(StubDurations::default());
        let (converter, _entered) = FakeConverter::new(true, false);
        let converter = Arc::new(converter);
        let delivery = delivery(Arc::new(StubFetcher::new(b"0123456789")), 16)
            .with_converter(Arc::clone(&converter) as Arc<dyn Converter>)
            .with_track_durations(Arc::clone(&durations) as Arc<dyn TrackDurations>);

        let error = delivery
            .deliver(
                &key("vid-1"),
                &remote_media(None, "opus", Some(128.0)),
                &aac_request(),
                &track(),
                None,
            )
            .await
            .err()
            .expect("a missing duration must fail");
        assert!(matches!(error, DeliveryError::MissingDuration));
        assert_eq!(converter.starts(), 0);
    }

    #[tokio::test]
    async fn a_failed_track_duration_lookup_is_a_named_error() {
        let durations = Arc::new(StubDurations::failing());
        let (converter, _entered) = FakeConverter::new(true, false);
        let converter = Arc::new(converter);
        let delivery = delivery(Arc::new(StubFetcher::new(b"0123456789")), 16)
            .with_converter(Arc::clone(&converter) as Arc<dyn Converter>)
            .with_track_durations(Arc::clone(&durations) as Arc<dyn TrackDurations>);

        let error = delivery
            .deliver(
                &key("vid-1"),
                &remote_media(None, "opus", Some(128.0)),
                &aac_request(),
                &track(),
                None,
            )
            .await
            .err()
            .expect("a library failure must fail the request");
        assert!(matches!(error, DeliveryError::Duration(_)));
        assert_eq!(converter.starts(), 0);
    }

    #[tokio::test]
    async fn an_original_request_never_reads_the_track_duration() {
        let durations = Arc::new(StubDurations::returning(10_000));
        let delivery = delivery(Arc::new(StubFetcher::new(b"0123456789")), 16)
            .with_track_durations(Arc::clone(&durations) as Arc<dyn TrackDurations>);

        let result = delivery
            .deliver(
                &key("vid-1"),
                &remote_media(None, "opus", Some(128.0)),
                &StreamRequest::default(),
                &track(),
                None,
            )
            .await
            .unwrap();
        assert_eq!(body_bytes(result).await.2, b"0123456789");
        assert_eq!(durations.calls(), 0);
    }

    #[tokio::test]
    async fn a_local_aac_source_is_copied() {
        let durations = Arc::new(StubDurations::returning(10_000));
        let (converter, _entered) = FakeConverter::new(true, false);
        let converter = Arc::new(converter);
        let delivery = delivery(Arc::new(StubFetcher::new(b"0123456789")), 16)
            .with_converter(Arc::clone(&converter) as Arc<dyn Converter>)
            .with_track_durations(Arc::clone(&durations) as Arc<dyn TrackDurations>);

        let result = delivery
            .deliver(
                &key("vid-1"),
                &local_media(Some("mp4a.40.2"), Some(192.0), "audio/mp4").await,
                &aac_request(),
                &track(),
                None,
            )
            .await
            .unwrap();
        body_bytes(result).await;
        assert_eq!(
            converter.only_representation(),
            Representation::Aac(AacMode::Copy)
        );
    }

    #[tokio::test]
    async fn a_local_source_below_the_max_bitrate_stays_original() {
        let durations = Arc::new(StubDurations::returning(10_000));
        let (converter, _entered) = FakeConverter::new(true, false);
        let converter = Arc::new(converter);
        let delivery = delivery(Arc::new(StubFetcher::new(b"0123456789")), 16)
            .with_converter(Arc::clone(&converter) as Arc<dyn Converter>)
            .with_track_durations(Arc::clone(&durations) as Arc<dyn TrackDurations>);

        let request = StreamRequest {
            max_bitrate_kbps: Some(128),
            ..StreamRequest::default()
        };
        let result = delivery
            .deliver(
                &key("vid-1"),
                &local_media(Some("mp4a.40.2"), Some(96.0), "audio/mp4").await,
                &request,
                &track(),
                None,
            )
            .await
            .unwrap();
        assert_eq!(body_bytes(result).await.2, b"local");
        assert_eq!(converter.starts(), 0);
    }

    #[tokio::test]
    async fn a_local_source_equal_to_the_max_bitrate_stays_original() {
        let durations = Arc::new(StubDurations::returning(10_000));
        let (converter, _entered) = FakeConverter::new(true, false);
        let converter = Arc::new(converter);
        let delivery = delivery(Arc::new(StubFetcher::new(b"0123456789")), 16)
            .with_converter(Arc::clone(&converter) as Arc<dyn Converter>)
            .with_track_durations(Arc::clone(&durations) as Arc<dyn TrackDurations>);

        let request = StreamRequest {
            max_bitrate_kbps: Some(128),
            ..StreamRequest::default()
        };
        let result = delivery
            .deliver(
                &key("vid-1"),
                &local_media(Some("mp4a.40.2"), Some(128.0), "audio/mp4").await,
                &request,
                &track(),
                None,
            )
            .await
            .unwrap();
        body_bytes(result).await;
        assert_eq!(converter.starts(), 0);
    }

    #[tokio::test]
    async fn a_local_source_above_the_max_bitrate_converts() {
        let durations = Arc::new(StubDurations::returning(10_000));
        let (converter, _entered) = FakeConverter::new(true, false);
        let converter = Arc::new(converter);
        let delivery = delivery(Arc::new(StubFetcher::new(b"0123456789")), 16)
            .with_converter(Arc::clone(&converter) as Arc<dyn Converter>)
            .with_track_durations(Arc::clone(&durations) as Arc<dyn TrackDurations>);

        let request = StreamRequest {
            max_bitrate_kbps: Some(128),
            ..StreamRequest::default()
        };
        let result = delivery
            .deliver(
                &key("vid-1"),
                &local_media(Some("mp4a.40.2"), Some(192.0), "audio/mp4").await,
                &request,
                &track(),
                None,
            )
            .await
            .unwrap();
        body_bytes(result).await;
        assert_eq!(
            converter.only_representation(),
            Representation::Aac(AacMode::Encode { bitrate_kbps: 128 })
        );
    }

    #[tokio::test]
    async fn a_local_lossless_source_with_a_max_bitrate_encodes() {
        let durations = Arc::new(StubDurations::returning(10_000));
        let (converter, _entered) = FakeConverter::new(true, false);
        let converter = Arc::new(converter);
        let delivery = delivery(Arc::new(StubFetcher::new(b"0123456789")), 16)
            .with_converter(Arc::clone(&converter) as Arc<dyn Converter>)
            .with_track_durations(Arc::clone(&durations) as Arc<dyn TrackDurations>);

        let request = StreamRequest {
            max_bitrate_kbps: Some(128),
            ..StreamRequest::default()
        };
        let result = delivery
            .deliver(
                &key("vid-1"),
                &local_media(Some("flac"), Some(900.0), "audio/flac").await,
                &request,
                &track(),
                None,
            )
            .await
            .unwrap();
        body_bytes(result).await;
        assert_eq!(
            converter.only_representation(),
            Representation::Aac(AacMode::Encode { bitrate_kbps: 128 })
        );
    }

    #[tokio::test]
    async fn a_local_source_without_duration_uses_the_track_duration() {
        let durations = Arc::new(StubDurations::returning(10_000));
        let (converter, _entered) = FakeConverter::new(true, false);
        let converter = Arc::new(converter);
        let delivery = delivery(Arc::new(StubFetcher::new(b"0123456789")), 16)
            .with_converter(Arc::clone(&converter) as Arc<dyn Converter>)
            .with_track_durations(Arc::clone(&durations) as Arc<dyn TrackDurations>);

        let result = delivery
            .deliver(
                &key("vid-1"),
                &local_media(Some("mp4a.40.2"), Some(192.0), "audio/mp4").await,
                &aac_request(),
                &track(),
                None,
            )
            .await
            .unwrap();
        body_bytes(result).await;
        assert_eq!(converter.starts(), 1);
        assert_eq!(durations.calls(), 1);
    }

    #[tokio::test]
    async fn a_converted_entry_is_reused_after_only_the_source_entry_is_evicted() {
        let now = Arc::new(Mutex::new(chrono::Utc::now()));
        let handle = Arc::clone(&now);
        let fetcher = Arc::new(StubFetcher::new(b"0123456789"));
        let (converter, _entered) = FakeConverter::new(true, false);
        let converter = Arc::new(converter);
        let delivery = StreamDelivery::new(
            Arc::clone(&fetcher) as Arc<dyn MediaFetcher>,
            std::env::temp_dir(),
            16,
        )
        .with_converter(Arc::clone(&converter) as Arc<dyn Converter>)
        .with_spool_idle(Duration::from_secs(60))
        .with_spool_clock(Clock::from_fn(move || *handle.lock()));
        let source = key("vid-1");
        let media = remote_media(Some(10_000), "opus", Some(128.0));
        let request = aac_request();

        let full = delivery
            .deliver(&source, &media, &request, &track(), None)
            .await
            .unwrap();
        body_bytes(full).await;
        assert_eq!(fetcher.probes(), 1);
        assert_eq!(converter.starts(), 1);

        *now.lock() += chrono::Duration::seconds(30);
        let seek = delivery
            .deliver(
                &source,
                &media,
                &request,
                &track(),
                Some("bytes=0-3".to_owned()),
            )
            .await
            .unwrap();
        body_bytes(seek).await;
        assert_eq!(fetcher.probes(), 1);
        assert_eq!(converter.starts(), 1);

        *now.lock() += chrono::Duration::seconds(31);
        let reused = delivery
            .deliver(
                &source,
                &media,
                &request,
                &track(),
                Some("bytes=0-3".to_owned()),
            )
            .await
            .unwrap();
        body_bytes(reused).await;
        assert_eq!(fetcher.probes(), 1);
        assert_eq!(converter.starts(), 1);
    }

    fn converting_delivery(converter: Arc<FakeConverter>) -> StreamDelivery {
        delivery(Arc::new(StubFetcher::new(b"0123456789")), 16)
            .with_converter(converter as Arc<dyn Converter>)
    }
}
