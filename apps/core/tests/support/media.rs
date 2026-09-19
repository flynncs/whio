// fixtures are a toolbox; each suite uses a subset
#![allow(dead_code)]

use std::sync::Arc;

use async_trait::async_trait;
use bytes::Bytes;
use whio_core::{
    delivery::StreamDelivery,
    media::{MediaBody, MediaFetchError, MediaFetcher, MediaInfo},
    playback::PlayableMedia,
};

struct StubMediaFetcher;

#[async_trait]
impl MediaFetcher for StubMediaFetcher {
    async fn probe(&self, _media: &PlayableMedia) -> Result<MediaInfo, MediaFetchError> {
        Ok(MediaInfo {
            content_length: 1,
            supports_ranges: true,
            content_type: Some("audio/mpeg".to_owned()),
        })
    }

    async fn open_continuous(&self, _media: &PlayableMedia) -> Result<MediaBody, MediaFetchError> {
        Ok(MediaBody {
            content_length: Some(1),
            chunks: Box::pin(tokio_stream::once(Ok(Bytes::from_static(&[0])))),
        })
    }
}

pub fn delivery() -> Arc<StreamDelivery> {
    Arc::new(StreamDelivery::new(
        Arc::new(StubMediaFetcher),
        std::env::temp_dir(),
        whio_core::delivery::DEFAULT_MAX_SESSIONS,
    ))
}
