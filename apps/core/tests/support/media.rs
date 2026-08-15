use std::sync::Arc;

use async_trait::async_trait;
use bytes::Bytes;
use whio_core::{
    media::{MediaBody, MediaFetchError, MediaFetcher, MediaInfo},
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

    async fn open_continuous(&self, _media: &PlayableMedia) -> Result<MediaBody, MediaFetchError> {
        Ok(MediaBody {
            content_length: Some(1),
            chunks: Box::pin(tokio_stream::once(Ok(Bytes::from_static(&[0])))),
        })
    }
}

pub fn playback_stream(playback: Arc<PlaybackService>) -> Arc<PlaybackStreamService> {
    Arc::new(PlaybackStreamService::new(
        playback,
        Arc::new(StubMediaFetcher),
    ))
}
