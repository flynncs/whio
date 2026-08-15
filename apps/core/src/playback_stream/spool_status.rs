use tokio::{fs::File, io::AsyncWriteExt};
use tokio_stream::StreamExt;

use crate::media::MediaBody;

#[derive(Clone, Debug)]
pub enum SpoolStatus {
    Downloading { available: u64 },
    Complete { length: u64 },
    Failed { available: u64 },
}

pub async fn download_to_file(
    mut body: MediaBody,
    mut file: File,
    updates: tokio::sync::watch::Sender<SpoolStatus>,
) {
    let expected_length = body.content_length;
    let mut available: u64 = 0;

    while let Some(result) = body.chunks.next().await {
        match result {
            Ok(bytes) => {
                if let Err(error) = file.write_all(&bytes).await {
                    tracing::error!(
                        error = %error,
                        available,
                        "playback spool write failed"
                    );

                    updates.send_replace(SpoolStatus::Failed { available });

                    return;
                }

                available += bytes.len() as u64;

                updates.send_replace(SpoolStatus::Downloading { available });
            }
            Err(error) => {
                tracing::error!(
                    error = %error,
                    available,
                    "playback spool upstream failed"
                );

                updates.send_replace(SpoolStatus::Failed { available });

                return;
            }
        }
    }

    if let Err(error) = file.flush().await {
        tracing::error!(
            error = %error,
            available,
            "playback spool flush failed"
        );

        updates.send_replace(SpoolStatus::Failed { available });

        return;
    }

    if let Some(expected_length) = expected_length
        && available != expected_length
    {
        tracing::error!(
            expected = expected_length,
            actual = available,
            "playback spool ended before the advertised content length"
        );

        updates.send_replace(SpoolStatus::Failed { available });

        return;
    }

    updates.send_replace(SpoolStatus::Complete { length: available });
}

#[cfg(test)]
mod tests {
    use std::time::{SystemTime, UNIX_EPOCH};

    use bytes::Bytes;
    use tokio::{fs, sync::watch};
    use tokio_stream::iter;

    use super::*;
    use crate::media::MediaFetchError;

    fn test_path() -> std::path::PathBuf {
        let suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();

        std::env::temp_dir().join(format!("whio-spool-{suffix}.part"))
    }

    #[tokio::test]
    async fn writes_all_body_chunks_and_publishes_completion() {
        let path = test_path();
        let file = fs::File::create(&path).await.unwrap();
        let (updates, receiver) = watch::channel(SpoolStatus::Downloading { available: 0 });
        let body = MediaBody {
            content_length: Some(11),
            chunks: Box::pin(iter([
                Ok::<Bytes, MediaFetchError>(Bytes::from_static(b"hello ")),
                Ok::<Bytes, MediaFetchError>(Bytes::from_static(b"world")),
            ])),
        };

        download_to_file(body, file, updates).await;

        assert_eq!(fs::read(&path).await.unwrap(), b"hello world");
        assert!(matches!(
            &*receiver.borrow(),
            SpoolStatus::Complete { length: 11 }
        ));

        fs::remove_file(path).await.unwrap();
    }
}
