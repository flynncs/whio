use std::pin::Pin;

use async_trait::async_trait;
use bytes::Bytes;
use tokio_stream::Stream;

use crate::features::playback::PlayableMedia;
use crate::features::playback::{DeliveryError, Representation};
use crate::platform::http::FetchError as MediaFetchError;

#[derive(Debug, Clone)]
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

pub type ConversionByteStream = Pin<Box<dyn Stream<Item = Result<Bytes, DeliveryError>> + Send>>;

pub enum ConversionInput {
    Stream(ConversionByteStream),
    Path(std::path::PathBuf),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConversionTarget {
    pub representation: Representation,
}

#[async_trait]
pub trait MediaFetcher: Send + Sync {
    async fn probe(&self, media: &PlayableMedia) -> Result<MediaInfo, MediaFetchError>;

    async fn open_continuous(&self, media: &PlayableMedia) -> Result<MediaBody, MediaFetchError>;
}

#[async_trait]
pub trait Converter: Send + Sync {
    fn available(&self) -> bool;

    async fn start(
        &self,
        input: ConversionInput,
        target: ConversionTarget,
    ) -> Result<MediaBody, DeliveryError>;
}
