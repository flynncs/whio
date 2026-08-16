use std::{
    pin::Pin,
    task::{Context, Poll},
    time::Instant,
};

use bytes::Bytes;
use tokio::sync::mpsc;
use tokio_stream::{Stream, wrappers::ReceiverStream};
use uuid::Uuid;

use crate::{
    media::ByteRange,
    playback_stream::{PlaybackStreamError, TransferStats, spool_read::SpoolReader},
};

const BUFFERED_CHUNK_COUNT: usize = 4;

pub struct PlaybackByteStream {
    inner: ReceiverStream<Result<Bytes, PlaybackStreamError>>,
}

impl PlaybackByteStream {
    pub(crate) fn start(reader: SpoolReader, spool_id: Uuid, range: ByteRange) -> Self {
        let (sender, receiver) = mpsc::channel(BUFFERED_CHUNK_COUNT);

        tokio::spawn(send(reader, sender, spool_id, range));

        Self {
            inner: ReceiverStream::new(receiver),
        }
    }
}

async fn send(
    mut reader: SpoolReader,
    sender: mpsc::Sender<Result<Bytes, PlaybackStreamError>>,
    spool_id: Uuid,
    range: ByteRange,
) {
    let started_at = Instant::now();
    let mut delivered_bytes = 0;

    let outcome = loop {
        match reader.next_chunk().await {
            Ok(Some(bytes)) => {
                let chunk_bytes = bytes.len() as u64;
                if sender.send(Ok(bytes)).await.is_err() {
                    break "client_disconnected";
                }
                delivered_bytes += chunk_bytes;
            }
            Ok(None) => break "completed",
            Err(error) => {
                tracing::error!(error = %error, "playback response stream failed");
                let _ = sender.send(Err(error)).await;
                break "failed";
            }
        }
    };

    let stats = TransferStats::since(delivered_bytes, started_at);
    let elapsed_ms = stats.elapsed_ms();
    let average_mbps = stats.average_mbps();

    tracing::info!(
        spool_id = %spool_id,
        range_start = range.start(),
        requested_bytes = range.len(),
        delivered_bytes,
        elapsed_ms,
        average_mbps = %format_args!("{average_mbps:.2}"),
        outcome,
        "playback response stream finished"
    );
}

impl Stream for PlaybackByteStream {
    type Item = Result<Bytes, PlaybackStreamError>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        Pin::new(&mut self.get_mut().inner).poll_next(cx)
    }
}
