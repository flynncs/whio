use std::pin::Pin;

use async_trait::async_trait;
use bytes::Bytes;
use tokio_stream::Stream;

use crate::features::playback::PlayableMedia;
use crate::platform::http::FetchError as MediaFetchError;

#[derive(Debug)]
pub struct MediaInfo {
    pub content_length: u64,
    pub supports_ranges: bool,
    pub content_type: Option<String>,
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
