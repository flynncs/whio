use async_trait::async_trait;
use thiserror::Error;

use crate::playback::PlayableMedia;

use super::ByteRange;

#[derive(Debug, Error)]
pub(crate) enum MediaFetchError {
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

    #[error("media response length did not match the requested range")]
    LengthMismatch,
}

pub(crate) struct MediaInfo {
    pub(crate) content_length: u64,
    pub(crate) supports_ranges: bool,
}

pub(crate) struct FetchedRange {
    pub(crate) bytes: Vec<u8>,
    pub(crate) range: ByteRange,
}

#[async_trait]
pub(crate) trait MediaFetcher: Send + Sync {
    async fn probe(&self, media: &PlayableMedia) -> Result<MediaInfo, MediaFetchError>;

    async fn fetch_range(
        &self,
        media: &PlayableMedia,
        range: &ByteRange,
    ) -> Result<FetchedRange, MediaFetchError>;
}
