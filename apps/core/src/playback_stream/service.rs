use std::{
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};

use bytes::Bytes;
use tokio::sync::mpsc;
use tokio_stream::{Stream, wrappers::ReceiverStream};

use crate::{
    media::{ByteRange, MediaFetchError, MediaFetcher, MediaInfo},
    playback::{PlayableMedia, PlaybackError, PlaybackService},
    playback_stream::ActiveSpool,
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
    pub media: PlayableMedia,
    pub info: MediaInfo,
}

pub struct PlaybackStreamService {
    playback: Arc<PlaybackService>,
    fetcher: Arc<dyn MediaFetcher>,
}

const BUFFERED_CHUNK_COUNT: usize = 4;

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

    pub async fn stream_range(
        &self,
        prepared: PreparedPlayback,
        range: ByteRange,
    ) -> Result<PlaybackByteStream, PlaybackStreamError> {
        let body = self.fetcher.open_continuous(&prepared.media).await?;

        let spool = ActiveSpool::start(&std::env::temp_dir(), body).await?;

        let reader = spool.reader(range.start(), range.len()).await?;

        let (sender, receiver) =
            mpsc::channel::<Result<Bytes, PlaybackStreamError>>(BUFFERED_CHUNK_COUNT);

        tokio::spawn(async move {
            let mut reader = reader;

            loop {
                match reader.next_chunk().await {
                    Ok(Some(bytes)) => {
                        if sender.send(Ok(bytes)).await.is_err() {
                            break;
                        }
                    }
                    Ok(None) => break,
                    Err(error) => {
                        tracing::error!(error = %error, "playback response stream failed");
                        let _ = sender.send(Err(error)).await;
                        break;
                    }
                }
            }
        });

        Ok(PlaybackByteStream::new(receiver))
    }
}

pub struct PlaybackByteStream {
    inner: ReceiverStream<Result<Bytes, PlaybackStreamError>>,
}

impl PlaybackByteStream {
    fn new(receiver: mpsc::Receiver<Result<Bytes, PlaybackStreamError>>) -> Self {
        Self {
            inner: ReceiverStream::new(receiver),
        }
    }
}

impl Stream for PlaybackByteStream {
    type Item = Result<Bytes, PlaybackStreamError>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        Pin::new(&mut self.get_mut().inner).poll_next(cx)
    }
}
