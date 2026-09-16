use std::{path::PathBuf, sync::Arc};

use axum::{
    body::Body,
    http::{
        HeaderMap, HeaderValue, StatusCode,
        header::{ACCEPT_RANGES, CONTENT_LENGTH, CONTENT_RANGE, CONTENT_TYPE},
    },
};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use super::{ActiveSpool, PlaybackByteStream};
use crate::{
    http::range::{RangeRequest, parse_range_header},
    media::{ByteRange, MediaBody, MediaFetchError, MediaFetcher, MediaInfo},
    playback::{DeliveryError, DeliveryResult, PlayableMedia},
};

pub const DEFAULT_MAX_SPOOL_BYTES: u64 = 256 * 1024 * 1024;
pub const DEFAULT_SPOOL_BUDGET_BYTES: u64 = 1024 * 1024 * 1024;
const DISK_UNIT: u64 = 64 * 1024;

pub(super) struct SpoolStrategy {
    fetcher: Arc<dyn MediaFetcher>,
    directory: PathBuf,
    disk: Arc<Semaphore>,
    max_bytes: u64,
}

impl SpoolStrategy {
    pub fn new(fetcher: Arc<dyn MediaFetcher>, directory: PathBuf) -> Self {
        Self {
            fetcher,
            directory,
            disk: Arc::new(Semaphore::new(0)),
            max_bytes: 0,
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

    pub async fn open(
        &self,
        media: &PlayableMedia,
        client_range: Option<&str>,
    ) -> Result<DeliveryResult, DeliveryError> {
        let info = self.fetcher.probe(media).await?;
        let response = SpoolResponse::new(info, client_range)?;
        let reservation = self.reserve(response.info.content_length)?;
        let body = self.open_media(media, response.info.content_length).await?;
        let spool = ActiveSpool::start(&self.directory, body, reservation).await?;
        let range = response.range();
        let reader = spool.reader(range.start(), range.len()).await?;
        let stream = PlaybackByteStream::start(reader, spool.id(), range);
        Ok(response.into_delivery(Body::from_stream(stream)))
    }

    fn reserve(&self, length: u64) -> Result<OwnedSemaphorePermit, DeliveryError> {
        if length == 0 || length > self.max_bytes {
            return Err(DeliveryError::Overloaded);
        }
        let units =
            u32::try_from(length.div_ceil(DISK_UNIT)).map_err(|_| DeliveryError::Overloaded)?;
        self.disk
            .clone()
            .try_acquire_many_owned(units)
            .map_err(|_| DeliveryError::Overloaded)
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

struct SpoolResponse {
    info: MediaInfo,
    request: RangeRequest,
}

impl SpoolResponse {
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
