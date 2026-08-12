use std::sync::Arc;

use async_trait::async_trait;
use whio_core::{
    media::{ByteRange, FetchedRange, MediaFetchError, MediaFetcher, MediaInfo},
    playback::{PlayableMedia, PlaybackService},
    playback_stream::PlaybackStreamService,
};

struct StubMediaFetcher;

#[async_trait]
impl MediaFetcher for StubMediaFetcher {
    async fn probe(&self, _media: &PlayableMedia) -> Result<MediaInfo, MediaFetchError> {
        Ok(MediaInfo {
            content_length: 1,
            supports_ranges: true,
        })
    }

    async fn fetch_range(
        &self,
        _media: &PlayableMedia,
        range: &ByteRange,
    ) -> Result<FetchedRange, MediaFetchError> {
        Ok(FetchedRange {
            bytes: vec![0],
            range: range.clone(),
        })
    }
}

pub fn playback_stream(playback: Arc<PlaybackService>) -> Arc<PlaybackStreamService> {
    Arc::new(PlaybackStreamService::new(
        playback,
        Arc::new(StubMediaFetcher),
    ))
}
