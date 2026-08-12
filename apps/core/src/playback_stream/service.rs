use std::sync::Arc;

use crate::{
    media::{ByteRange, FetchedRange, MediaFetchError, MediaFetcher, MediaInfo},
    playback::{PlayableMedia, PlaybackError, PlaybackService},
    request::RequestContext,
    tracks::TrackId,
};

#[derive(Debug, thiserror::Error)]
pub enum PlaybackStreamError {
    #[error("playback resolution failed")]
    Playback(#[from] PlaybackError),

    #[error("media fetch failed")]
    Media(#[from] MediaFetchError),
}

pub struct PreparedPlayback {
    pub media: PlayableMedia,
    pub info: MediaInfo,
}

pub struct PlaybackStreamService {
    playback: Arc<PlaybackService>,
    fetcher: Arc<dyn MediaFetcher>,
}

impl PlaybackStreamService {
    pub fn new(playback: Arc<PlaybackService>, fetcher: Arc<dyn MediaFetcher>) -> Self {
        Self { playback, fetcher }
    }

    pub async fn prepare(
        &self,
        track_id: &TrackId,
        context: &RequestContext,
    ) -> Result<PreparedPlayback, PlaybackStreamError> {
        let media = self.playback.resolve(track_id, context).await?;
        let info = self.fetcher.probe(&media).await?;
        Ok(PreparedPlayback { media, info })
    }

    pub async fn fetch_range(
        &self,
        prepared: &PreparedPlayback,
        range: &ByteRange,
    ) -> Result<FetchedRange, PlaybackStreamError> {
        self.fetcher
            .fetch_range(&prepared.media, range)
            .await
            .map_err(PlaybackStreamError::Media)
    }
}
