use std::{path::PathBuf, sync::Arc, time::Duration};

use axum::{
    body::Body,
    http::{
        HeaderMap, HeaderValue, StatusCode,
        header::{ACCEPT_RANGES, CONTENT_LENGTH, CONTENT_RANGE, CONTENT_TYPE},
    },
};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use super::range::{RangeRequest, parse_range_header};
use super::spool_cache::{Cached, SpoolCache};
use super::{ActiveSpool, PlaybackByteStream};
use crate::{
    features::playback::media::{ByteRange, MediaBody, MediaFetchError, MediaFetcher, MediaInfo},
    features::playback::{DeliveryError, DeliveryResult, PlayableMedia, SourceKey},
    platform::clock::Clock,
};

pub const DEFAULT_MAX_SPOOL_BYTES: u64 = 256 * 1024 * 1024;
pub const DEFAULT_SPOOL_BUDGET_BYTES: u64 = 1024 * 1024 * 1024;
pub const DEFAULT_SPOOL_IDLE_SECONDS: u64 = 900;
const DISK_UNIT: u64 = 64 * 1024;

pub(super) struct SpoolStrategy {
    fetcher: Arc<dyn MediaFetcher>,
    directory: PathBuf,
    disk: Arc<Semaphore>,
    max_bytes: u64,
    cache: SpoolCache,
}

impl SpoolStrategy {
    pub fn new(fetcher: Arc<dyn MediaFetcher>, directory: PathBuf) -> Self {
        Self {
            fetcher,
            directory,
            disk: Arc::new(Semaphore::new(0)),
            max_bytes: 0,
            cache: SpoolCache::new(
                Clock::system(),
                Duration::from_secs(DEFAULT_SPOOL_IDLE_SECONDS),
            ),
        }
        .with_limits(DEFAULT_MAX_SPOOL_BYTES, DEFAULT_SPOOL_BUDGET_BYTES)
    }

    pub fn with_limits(mut self, per_spool: u64, total: u64) -> Self {
        self.max_bytes = per_spool;
        self.disk = Arc::new(Semaphore::new(
            (total / DISK_UNIT).min(u32::MAX as u64) as usize
        ));
        self
    }

    pub fn with_clock(mut self, clock: Clock) -> Self {
        self.cache = self.cache.with_clock(clock);
        self
    }

    pub fn with_idle(mut self, idle: Duration) -> Self {
        self.cache = self.cache.with_idle(idle);
        self
    }

    pub fn lookup(&self, key: &SourceKey) -> Option<Cached> {
        self.cache.evict_idle();
        self.cache.get(key)
    }

    pub async fn open(
        &self,
        key: &SourceKey,
        media: &PlayableMedia,
        client_range: Option<&str>,
    ) -> Result<DeliveryResult, DeliveryError> {
        if let Some(cached) = self.lookup(key) {
            match self.serve(key, &cached, client_range).await {
                Ok(delivery) => return Ok(delivery),
                Err(DeliveryError::Spool) if cached.failed() => {}
                Err(error) => return Err(error),
            }
        }
        let info = self.fetcher.probe(media).await?;
        SpoolResponse::validate(&info, client_range)?;
        let cached = self.create_once(key, media, &info).await?;
        self.serve(key, &cached, client_range).await
    }

    pub async fn serve(
        &self,
        key: &SourceKey,
        cached: &Cached,
        client_range: Option<&str>,
    ) -> Result<DeliveryResult, DeliveryError> {
        match respond(&cached.spool, &cached.info, client_range).await {
            Ok(delivery) => Ok(delivery),
            Err(DeliveryError::Spool) if cached.failed() => {
                self.cache.invalidate(key, cached);
                Err(DeliveryError::Spool)
            }
            Err(error) => Err(error),
        }
    }

    pub fn is_cached(&self, key: &SourceKey) -> bool {
        self.cache.is_cached(key)
    }

    pub async fn warm(&self, key: &SourceKey, media: &PlayableMedia) -> Result<(), DeliveryError> {
        if self.lookup(key).is_some() {
            return Ok(());
        }
        let info = self.fetcher.probe(media).await?;
        self.create_once(key, media, &info).await.map(|_| ())
    }

    async fn create_once(
        &self,
        key: &SourceKey,
        media: &PlayableMedia,
        info: &MediaInfo,
    ) -> Result<Cached, DeliveryError> {
        self.cache
            .get_or_init(key, || async {
                let reservation = self.reserve(info.content_length)?;
                let body = self.open_media(media, info.content_length).await?;
                let spool = ActiveSpool::start(&self.directory, body, reservation).await?;
                Ok(self.cache.entry(spool, info.clone()))
            })
            .await
    }

    fn reserve(&self, length: u64) -> Result<OwnedSemaphorePermit, DeliveryError> {
        if length == 0 || length > self.max_bytes {
            return Err(DeliveryError::Overloaded);
        }
        let units =
            u32::try_from(length.div_ceil(DISK_UNIT)).map_err(|_| DeliveryError::Overloaded)?;
        loop {
            match self.disk.clone().try_acquire_many_owned(units) {
                Ok(permit) => return Ok(permit),
                Err(_) if self.cache.evict_one_idle() => continue,
                Err(_) => return Err(DeliveryError::Overloaded),
            }
        }
    }

    async fn open_media(
        &self,
        media: &PlayableMedia,
        length: u64,
    ) -> Result<MediaBody, DeliveryError> {
        let mut body = self.fetcher.open_continuous(media).await?;
        if body.content_length.is_some_and(|actual| actual != length) {
            return Err(MediaFetchError::InvalidContentRange.into());
        }
        // the writer enforces this ceiling, including for chunked responses.
        body.content_length = Some(length);
        Ok(body)
    }
}

async fn respond(
    spool: &Arc<ActiveSpool>,
    info: &MediaInfo,
    client_range: Option<&str>,
) -> Result<DeliveryResult, DeliveryError> {
    let response = SpoolResponse::new(info.clone(), client_range)?;
    let range = response.range();
    let reader = spool.reader(range.start(), range.len()).await?;
    if spool.failed() {
        return Err(DeliveryError::Spool);
    }
    let stream = PlaybackByteStream::start(reader, spool.id(), range);
    Ok(response.into_delivery(Body::from_stream(stream)))
}

struct SpoolResponse {
    info: MediaInfo,
    request: RangeRequest,
}

impl SpoolResponse {
    fn validate(info: &MediaInfo, header: Option<&str>) -> Result<(), DeliveryError> {
        Self::new(info.clone(), header).map(|_| ())
    }

    fn new(info: MediaInfo, header: Option<&str>) -> Result<Self, DeliveryError> {
        if info.content_length == 0 {
            return Err(MediaFetchError::InvalidContentRange.into());
        }
        let request = parse_range_header(header, info.content_length).map_err(|error| {
            DeliveryError::Range {
                content_length: info.content_length,
                error,
            }
        })?;
        Ok(Self { info, request })
    }

    fn range(&self) -> ByteRange {
        match &self.request {
            RangeRequest::Full => ByteRange {
                start: 0,
                end: self.info.content_length - 1,
            },
            RangeRequest::Partial(range) => range.clone(),
        }
    }

    fn into_delivery(self, body: Body) -> DeliveryResult {
        let range = self.range();
        let partial = matches!(self.request, RangeRequest::Partial(_));
        let mut headers = HeaderMap::new();
        headers.insert(ACCEPT_RANGES, HeaderValue::from_static("bytes"));
        headers.insert(
            CONTENT_LENGTH,
            HeaderValue::from_str(&range.len().to_string()).expect("byte length header"),
        );
        if let Some(content_type) = self.info.content_type
            && let Ok(value) = HeaderValue::from_str(&content_type)
        {
            headers.insert(CONTENT_TYPE, value);
        }
        if partial {
            let value = format!(
                "bytes {}-{}/{}",
                range.start(),
                range.end(),
                self.info.content_length
            );
            headers.insert(
                CONTENT_RANGE,
                HeaderValue::from_str(&value).expect("content range header"),
            );
        }
        DeliveryResult {
            status: if partial {
                StatusCode::PARTIAL_CONTENT
            } else {
                StatusCode::OK
            },
            headers,
            body,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{
        path::PathBuf,
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
    };

    use async_trait::async_trait;
    use axum::http::StatusCode;
    use bytes::Bytes;
    use tokio::sync::{Notify, mpsc};

    use super::*;
    use crate::features::playback::{
        MediaMetadata, PlayableMedia, PlaybackUrl,
        media::{MediaBody, MediaFetchError},
    };

    struct HeldFailureFetcher {
        bytes: Vec<u8>,
        release: Arc<Notify>,
        opens: AtomicUsize,
    }

    impl HeldFailureFetcher {
        fn new(bytes: &[u8], release: Arc<Notify>) -> Self {
            Self {
                bytes: bytes.to_vec(),
                release,
                opens: AtomicUsize::new(0),
            }
        }

        fn opens(&self) -> usize {
            self.opens.load(Ordering::SeqCst)
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

    fn media() -> PlayableMedia {
        PlayableMedia::new(
            PlaybackUrl::new("https://media.example.test/audio".to_owned()).unwrap(),
            std::collections::HashMap::new(),
            None,
            MediaMetadata::new(None, None, Some("opus".to_owned()), Some(128.0), None).unwrap(),
        )
    }

    fn key(external_id: &str) -> SourceKey {
        SourceKey::new(
            &crate::features::playback::PlaybackSource::from_identity(
                crate::features::music_library::domain::SourceIdentity::new(
                    crate::features::music_library::domain::ProviderId::new(
                        "youtube_music".to_owned(),
                    )
                    .unwrap(),
                    crate::features::music_library::domain::SourceScope::Global,
                    external_id.to_owned(),
                )
                .unwrap(),
            )
            .unwrap(),
        )
    }

    async fn test_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("whio-spool-test-{}", uuid::Uuid::now_v7()));
        tokio::fs::create_dir_all(&dir).await.unwrap();
        dir
    }

    async fn failed_cached(
        strategy: &SpoolStrategy,
        key: &SourceKey,
        release: &Arc<Notify>,
    ) -> Cached {
        let cached = strategy.lookup(key).expect("a live cached spool");
        release.notify_one();
        let mut reader = cached.spool.reader(0, 1).await.unwrap();
        assert!(reader.next_chunk().await.is_err());
        assert!(cached.failed());
        cached
    }

    async fn body_bytes(delivery: DeliveryResult) -> Vec<u8> {
        axum::body::to_bytes(delivery.body, 1024 * 1024)
            .await
            .unwrap()
            .to_vec()
    }

    #[tokio::test]
    async fn a_failed_spool_is_rejected_at_admission_and_replaced() {
        let dir = test_dir().await;
        let release = Arc::new(Notify::new());
        let fetcher = Arc::new(HeldFailureFetcher::new(b"0123456789", Arc::clone(&release)));
        let strategy =
            SpoolStrategy::new(Arc::clone(&fetcher) as Arc<dyn MediaFetcher>, dir.clone());
        let source = key("vid-1");
        strategy.warm(&source, &media()).await.unwrap();

        let cached = failed_cached(&strategy, &source, &release).await;
        let error = strategy
            .serve(&source, &cached, None)
            .await
            .err()
            .expect("a failed spool must not be admitted");
        assert!(matches!(error, DeliveryError::Spool));

        let replaced = strategy.open(&source, &media(), None).await.unwrap();
        assert_eq!(body_bytes(replaced).await, b"0123456789");
        assert_eq!(fetcher.opens(), 2);

        tokio::fs::remove_dir_all(&dir).await.unwrap();
    }

    #[tokio::test]
    async fn a_failed_spool_keeps_the_range_error() {
        let dir = test_dir().await;
        let release = Arc::new(Notify::new());
        let fetcher = Arc::new(HeldFailureFetcher::new(b"0123456789", Arc::clone(&release)));
        let strategy =
            SpoolStrategy::new(Arc::clone(&fetcher) as Arc<dyn MediaFetcher>, dir.clone());
        let source = key("vid-1");
        strategy.warm(&source, &media()).await.unwrap();

        let cached = failed_cached(&strategy, &source, &release).await;
        let error = strategy
            .serve(&source, &cached, Some("bytes=99-100"))
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

        tokio::fs::remove_dir_all(&dir).await.unwrap();
    }
}
