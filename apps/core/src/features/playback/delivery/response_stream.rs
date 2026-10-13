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
    features::playback::DeliveryError,
    features::playback::delivery::{TransferStats, spool_read::SpoolReader},
    features::playback::media::ByteRange,
};

const BUFFERED_CHUNK_COUNT: usize = 4;

pub struct PlaybackByteStream {
    inner: ReceiverStream<Result<Bytes, DeliveryError>>,
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
    sender: mpsc::Sender<Result<Bytes, DeliveryError>>,
    spool_id: Uuid,
    range: ByteRange,
) {
    let started_at = Instant::now();
    let mut delivered_bytes = 0;

    let outcome = loop {
        let chunk = tokio::select! {
            chunk = reader.next_chunk() => chunk,
            _ = sender.closed() => break "client_disconnected",
        };
        match chunk {
            Ok(Some(bytes)) => {
                let chunk_bytes = bytes.len() as u64;
                if sender.send(Ok(bytes)).await.is_err() {
                    break "client_disconnected";
                }
                delivered_bytes += chunk_bytes;
            }
            Ok(None) => break "completed",
            Err(error) => {
                if matches!(error, DeliveryError::Conversion) {
                    tracing::debug!(error = %error, "playback conversion stream failed");
                } else {
                    tracing::error!(error = %error, "playback response stream failed");
                }
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
    type Item = Result<Bytes, DeliveryError>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        Pin::new(&mut self.get_mut().inner).poll_next(cx)
    }
}
