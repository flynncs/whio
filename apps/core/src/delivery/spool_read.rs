use std::io::SeekFrom;

use bytes::Bytes;

use tokio::io::{AsyncReadExt, AsyncSeekExt};

use crate::{delivery::spool_status::SpoolStatus, playback::DeliveryError};

pub struct SpoolReader {
    file: tokio::fs::File,
    updates: tokio::sync::watch::Receiver<SpoolStatus>,
    _spool: std::sync::Arc<super::ActiveSpool>,
    offset: u64,
    remaining: u64,
}

impl SpoolReader {
    pub async fn new(
        mut file: tokio::fs::File,
        updates: tokio::sync::watch::Receiver<SpoolStatus>,
        spool: std::sync::Arc<super::ActiveSpool>,
        offset: u64,
        remaining: u64,
    ) -> Result<Self, DeliveryError> {
        file.seek(SeekFrom::Start(offset)).await.map_err(|error| {
            tracing::error!(error = %error, "playback spool seek failed");
            DeliveryError::Spool
        })?;

        Ok(Self {
            file,
            updates,
            _spool: spool,
            offset,
            remaining,
        })
    }

    pub async fn next_chunk(&mut self) -> Result<Option<Bytes>, DeliveryError> {
        loop {
            let status = self.updates.borrow_and_update().clone();

            let (available, complete, failed) = match status {
                SpoolStatus::Downloading { available } => (available, false, false),
                SpoolStatus::Complete { length } => (length, true, false),
                SpoolStatus::Failed { available } => (available, false, true),
            };

            if self.remaining == 0 {
                return Ok(None);
            }

            let bytes_ready = available.saturating_sub(self.offset);
            let readable = bytes_ready.min(self.remaining);

            if readable > 0 {
                let read_size = readable.min(64 * 1024) as usize;
                let mut buffer = vec![0u8; read_size];
                let count = self.file.read(&mut buffer).await.map_err(|error| {
                    tracing::error!(error = %error,  "playback spool read failed");
                    DeliveryError::Spool
                })?;

                if count == 0 {
                    if complete || failed {
                        tracing::error!(
                            offset = self.offset,
                            remaining = self.remaining,
                            available,
                            complete,
                            failed,
                            "playback spool reached EOF before the requested bytes were available"
                        );

                        return Err(DeliveryError::Spool);
                    }

                    self.wait_for_update().await?;
                    continue;
                }

                buffer.truncate(count);

                let count = count as u64;
                self.offset += count;
                self.remaining -= count;

                return Ok(Some(Bytes::from(buffer)));
            }

            if complete {
                tracing::error!(
                    offset = self.offset,
                    remaining = self.remaining,
                    available,
                    "playback spool completed before the requested range was available"
                );
                return Err(DeliveryError::Spool);
            }

            if failed {
                tracing::error!(
                    offset = self.offset,
                    remaining = self.remaining,
                    available,
                    "playback spool failed before the requested range was available"
                );
                return Err(DeliveryError::Spool);
            }

            self.wait_for_update().await?;
        }
    }

    async fn wait_for_update(&mut self) -> Result<(), DeliveryError> {
        if self.updates.changed().await.is_ok() {
            return Ok(());
        }

        match self.updates.borrow().clone() {
            SpoolStatus::Complete { .. } => Ok(()),
            SpoolStatus::Failed { available } => {
                tracing::error!(available, "playback spool failed while reader was waiting");
                Err(DeliveryError::Spool)
            }
            SpoolStatus::Downloading { available } => {
                tracing::error!(
                    available,
                    "playback spool stopped updating while reader was waiting"
                );
                Err(DeliveryError::Spool)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::{SystemTime, UNIX_EPOCH};

    use tokio::{fs, io::AsyncWriteExt, sync::watch};

    use super::*;

    async fn keepalive_spool() -> std::sync::Arc<crate::delivery::ActiveSpool> {
        use crate::media::MediaBody;

        let sessions = std::sync::Arc::new(tokio::sync::Semaphore::new(1));
        let permit = sessions.try_acquire_owned().unwrap();
        let body = MediaBody {
            content_length: Some(0),
            chunks: Box::pin(tokio_stream::iter(Vec::<
                Result<bytes::Bytes, crate::media::MediaFetchError>,
            >::new())),
        };
        crate::delivery::ActiveSpool::start(&std::env::temp_dir(), body, permit)
            .await
            .unwrap()
    }

    fn test_path() -> std::path::PathBuf {
        let suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();

        std::env::temp_dir().join(format!("whio-reader-{suffix}.part"))
    }

    #[tokio::test]
    async fn seeks_to_offset_and_stops_at_requested_length() {
        let path = test_path();
        let mut writer = fs::File::create(&path).await.unwrap();
        writer.write_all(b"0123456789").await.unwrap();
        writer.flush().await.unwrap();
        drop(writer);

        let reader_file = fs::File::open(&path).await.unwrap();
        let (_, updates) = watch::channel(SpoolStatus::Complete { length: 10 });
        let spool = keepalive_spool().await;
        let mut reader = SpoolReader::new(reader_file, updates, spool, 2, 4)
            .await
            .unwrap();

        assert_eq!(
            reader.next_chunk().await.unwrap().unwrap(),
            Bytes::from("2345")
        );
        assert_eq!(reader.next_chunk().await.unwrap(), None);

        fs::remove_file(path).await.unwrap();
    }
}
