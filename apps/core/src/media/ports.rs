use std::pin::Pin;

use async_trait::async_trait;
use bytes::Bytes;
use thiserror::Error;
use tokio_stream::Stream;

use crate::playback::PlayableMedia;

#[derive(Debug, Error)]
pub enum MediaFetchError {
    #[error("media request failed")]
    Request(#[source] reqwest::Error),

    #[error("media response returned unexpected status: {0}")]
    UnexpectedStatus(reqwest::StatusCode),

    #[error("media response did not include a content length")]
    MissingContentLength,

    #[error("media response did not include a valid content range")]
    InvalidContentRange,

    #[error("media source does not support byte ranges")]
    RangesUnsupported,
}

pub struct MediaInfo {
    pub content_length: u64,
    pub supports_ranges: bool,
}

pub type MediaByteStream = Pin<Box<dyn Stream<Item = Result<Bytes, MediaFetchError>> + Send>>;

pub struct MediaBody {
    pub content_length: Option<u64>,
    pub chunks: MediaByteStream,
}

#[async_trait]
pub trait MediaFetcher: Send + Sync {
    async fn probe(&self, media: &PlayableMedia) -> Result<MediaInfo, MediaFetchError>;

    async fn open_continuous(&self, media: &PlayableMedia) -> Result<MediaBody, MediaFetchError>;
}
