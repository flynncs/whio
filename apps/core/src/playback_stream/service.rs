use std::sync::Arc;

use crate::{
    media::{ByteRange, MediaFetchError, MediaFetcher, MediaInfo},
    playback::{PlaybackError, PlaybackService},
    playback_stream::{ActiveSpool, PlaybackByteStream},
    request::RequestContext,
    tracks::TrackId,
};

#[derive(Debug, thiserror::Error)]
pub enum PlaybackStreamError {
    #[error("playback resolution failed")]
    Playback(#[from] PlaybackError),

    #[error("media fetch failed")]
    Media(#[from] MediaFetchError),

    #[error("playback spool failed")]
    Spool,
}

pub struct PreparedPlayback {
    pub info: MediaInfo,
    spool: Arc<ActiveSpool>,
}

pub struct PlaybackStreamService {
    playback: Arc<PlaybackService>,
    fetcher: Arc<dyn MediaFetcher>,
}

const MAX_MEDIA_ATTEMPTS: usize = 3;

impl PlaybackStreamService {
    pub fn new(playback: Arc<PlaybackService>, fetcher: Arc<dyn MediaFetcher>) -> Self {
        Self { playback, fetcher }
    }

    pub async fn prepare(
        &self,
        track_id: &TrackId,
        context: &RequestContext,
    ) -> Result<PreparedPlayback, PlaybackStreamError> {
        for attempt in 1..=MAX_MEDIA_ATTEMPTS {
            let media = self.playback.resolve(track_id, context).await?;

            let info = match self.fetcher.probe(&media).await {
                Ok(info) => info,
                Err(error) if attempt < MAX_MEDIA_ATTEMPTS && is_retryable(&error) => {
                    tracing::warn!(attempt, error = %error, "media probe failed; resolving a fresh URL");
                    continue;
                }
                Err(error) => return Err(error.into()),
            };

            let body = match self.fetcher.open_continuous(&media).await {
                Ok(body) => body,
                Err(error) if attempt < MAX_MEDIA_ATTEMPTS && is_retryable(&error) => {
                    tracing::warn!(attempt, error = %error, "media open failed; resolving a fresh URL");
                    continue;
                }
                Err(error) => return Err(error.into()),
            };

            let spool = ActiveSpool::start(&std::env::temp_dir(), body).await?;

            return Ok(PreparedPlayback { info, spool });
        }

        unreachable!("the media attempt loop always returns on its final attempt")
    }

    pub async fn stream_range(
        &self,
        prepared: PreparedPlayback,
        range: ByteRange,
    ) -> Result<PlaybackByteStream, PlaybackStreamError> {
        let spool_id = prepared.spool.id();
        let reader = prepared.spool.reader(range.start(), range.len()).await?;

        Ok(PlaybackByteStream::start(reader, spool_id, range))
    }
}

fn is_retryable(error: &MediaFetchError) -> bool {
    match error {
        MediaFetchError::Request(error) => error.is_connect() || error.is_timeout(),
        MediaFetchError::UnexpectedStatus(status) => {
            *status == reqwest::StatusCode::FORBIDDEN
                || *status == reqwest::StatusCode::TOO_MANY_REQUESTS
                || status.is_server_error()
        }
        MediaFetchError::MissingContentLength
        | MediaFetchError::InvalidContentRange
        | MediaFetchError::RangesUnsupported => false,
    }
}
