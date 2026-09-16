use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::Semaphore;

use super::{direct, session, spool::SpoolStrategy};
use crate::{
    media::MediaFetcher,
    playback::{
        DeliveryError, DeliveryResult, DeliveryStrategy, PlaybackDelivery, ResolvedPlayback,
    },
};

pub const DEFAULT_MAX_SESSIONS: usize = 16;

/// direct proxy and bounded spool delivery. owns bytes, ranges, session limits
/// and cancellation, and knows nothing about sources, providers or credentials.
pub struct StreamDelivery {
    spool: SpoolStrategy,
    sessions: Arc<Semaphore>,
    http: reqwest::Client,
}

impl StreamDelivery {
    pub fn new(
        fetcher: Arc<dyn MediaFetcher>,
        directory: std::path::PathBuf,
        max_sessions: usize,
        http: reqwest::Client,
    ) -> Self {
        Self {
            spool: SpoolStrategy::new(fetcher, directory),
            sessions: Arc::new(Semaphore::new(max_sessions.max(1))),
            http,
        }
    }

    pub fn with_spool_limits(mut self, per_spool: u64, total: u64) -> Self {
        self.spool = self.spool.with_limits(per_spool, total);
        self
    }
}

#[async_trait]
impl PlaybackDelivery for StreamDelivery {
    async fn deliver(
        &self,
        resolved: &ResolvedPlayback,
        client_range: Option<String>,
    ) -> Result<DeliveryResult, DeliveryError> {
        let permit = self
            .sessions
            .clone()
            .try_acquire_owned()
            .map_err(|_| DeliveryError::Overloaded)?;
        let mut delivery = match resolved.strategy {
            DeliveryStrategy::Direct => {
                direct::relay(&self.http, &resolved.media, client_range).await?
            }
            DeliveryStrategy::Spool => {
                self.spool
                    .open(&resolved.media, client_range.as_deref())
                    .await?
            }
        };
        // the permit travels with the response body, so a session is only
        // released once the client has finished with it.
        delivery.body = session::hold(delivery.body, permit);
        Ok(delivery)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use async_trait::async_trait;
    use axum::http::StatusCode;
    use bytes::Bytes;

    use super::*;
    use crate::{
        media::{MediaBody, MediaFetchError, MediaInfo},
        playback::{MediaMetadata, PlayableMedia, PlaybackUrl},
    };

    struct StubFetcher {
        bytes: Vec<u8>,
    }

    #[async_trait]
    impl MediaFetcher for StubFetcher {
        async fn probe(&self, _media: &PlayableMedia) -> Result<MediaInfo, MediaFetchError> {
            Ok(MediaInfo {
                content_length: self.bytes.len() as u64,
                supports_ranges: true,
                content_type: Some("audio/flac".to_owned()),
            })
        }

        async fn open_continuous(
            &self,
            _media: &PlayableMedia,
        ) -> Result<MediaBody, MediaFetchError> {
            Ok(MediaBody {
                content_length: Some(self.bytes.len() as u64),
                chunks: Box::pin(tokio_stream::iter(vec![Ok(Bytes::from(
                    self.bytes.clone(),
                ))])),
            })
        }
    }

    struct HangingFetcher;

    #[async_trait]
    impl MediaFetcher for HangingFetcher {
        async fn probe(&self, _media: &PlayableMedia) -> Result<MediaInfo, MediaFetchError> {
            Ok(MediaInfo {
                content_length: 10,
                supports_ranges: true,
                content_type: None,
            })
        }

        async fn open_continuous(
            &self,
            _media: &PlayableMedia,
        ) -> Result<MediaBody, MediaFetchError> {
            Ok(MediaBody {
                content_length: Some(10),
                chunks: Box::pin(tokio_stream::pending::<Result<Bytes, MediaFetchError>>()),
            })
        }
    }

    fn media(url: &str) -> PlayableMedia {
        PlayableMedia::new(
            PlaybackUrl::new(url.to_owned()).unwrap(),
            HashMap::new(),
            None,
            MediaMetadata::new(None, None, Some("opus".to_owned()), Some(128.0), None).unwrap(),
        )
    }

    fn resolved(url: &str, strategy: DeliveryStrategy) -> ResolvedPlayback {
        ResolvedPlayback {
            media: media(url),
            strategy,
        }
    }

    fn delivery(fetcher: Arc<dyn MediaFetcher>, max_sessions: usize) -> StreamDelivery {
        StreamDelivery::new(
            fetcher,
            std::env::temp_dir(),
            max_sessions,
            reqwest::Client::new(),
        )
    }

    async fn media_server() -> String {
        use axum::http::{
            HeaderMap,
            header::{CONTENT_LENGTH, CONTENT_RANGE, CONTENT_TYPE, RANGE},
        };
        use axum::{Router, response::IntoResponse, routing::get};

        async fn serve(headers: HeaderMap) -> axum::response::Response {
            if headers.get(RANGE).and_then(|value| value.to_str().ok()) == Some("bytes=2-4") {
                return (
                    StatusCode::PARTIAL_CONTENT,
                    [
                        (CONTENT_TYPE, "audio/flac"),
                        (CONTENT_LENGTH, "3"),
                        (CONTENT_RANGE, "bytes 2-4/10"),
                    ],
                    "234",
                )
                    .into_response();
            }
            (
                StatusCode::OK,
                [(CONTENT_TYPE, "audio/flac"), (CONTENT_LENGTH, "10")],
                "0123456789",
            )
                .into_response()
        }

        let app = Router::new().route("/audio", get(serve));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await });
        format!("http://{address}")
    }

    async fn body_bytes(delivery: DeliveryResult) -> (StatusCode, axum::http::HeaderMap, Vec<u8>) {
        let status = delivery.status;
        let headers = delivery.headers;
        let bytes = axum::body::to_bytes(delivery.body, 1024 * 1024)
            .await
            .unwrap();
        (status, headers, bytes.to_vec())
    }

    #[tokio::test]
    async fn direct_delivery_proxies_full_and_range_slices() {
        let base = media_server().await;
        let delivery = delivery(Arc::new(StubFetcher { bytes: Vec::new() }), 16);

        let result = delivery
            .deliver(
                &resolved(&format!("{base}/audio"), DeliveryStrategy::Direct),
                None,
            )
            .await
            .unwrap();
        let (status, headers, bytes) = body_bytes(result).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(headers["content-type"], "audio/flac");
        assert_eq!(bytes, b"0123456789");

        let result = delivery
            .deliver(
                &resolved(&format!("{base}/audio"), DeliveryStrategy::Direct),
                Some("bytes=2-4".to_owned()),
            )
            .await
            .unwrap();
        let (status, headers, bytes) = body_bytes(result).await;
        assert_eq!(status, StatusCode::PARTIAL_CONTENT);
        assert_eq!(headers["content-range"], "bytes 2-4/10");
        assert_eq!(bytes, b"234");
    }

    #[tokio::test]
    async fn spool_delivery_serves_bytes_and_rejects_bad_ranges() {
        let delivery = delivery(
            Arc::new(StubFetcher {
                bytes: b"0123456789".to_vec(),
            }),
            16,
        );

        let result = delivery
            .deliver(
                &resolved("https://media.example.test/audio", DeliveryStrategy::Spool),
                None,
            )
            .await
            .unwrap();
        let (status, headers, bytes) = body_bytes(result).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(headers["content-length"], "10");
        assert_eq!(bytes, b"0123456789");

        let result = delivery
            .deliver(
                &resolved("https://media.example.test/audio", DeliveryStrategy::Spool),
                Some("bytes=2-4".to_owned()),
            )
            .await
            .unwrap();
        let (status, headers, bytes) = body_bytes(result).await;
        assert_eq!(status, StatusCode::PARTIAL_CONTENT);
        assert_eq!(headers["content-range"], "bytes 2-4/10");
        assert_eq!(bytes, b"234");

        let error = delivery
            .deliver(
                &resolved("https://media.example.test/audio", DeliveryStrategy::Spool),
                Some("bytes=99-100".to_owned()),
            )
            .await
            .err()
            .expect("bad range must fail");
        assert!(matches!(
            error,
            DeliveryError::Range {
                content_length: 10,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn session_limit_rejects_the_second_concurrent_delivery() {
        let delivery = delivery(Arc::new(HangingFetcher), 1);
        let source = resolved("https://media.example.test/audio", DeliveryStrategy::Spool);

        let first = delivery.deliver(&source, None).await.unwrap();
        let error = delivery
            .deliver(&source, None)
            .await
            .err()
            .expect("second delivery must fail");
        assert!(matches!(error, DeliveryError::Overloaded));

        drop(first);
        let mut attempts = 0;
        loop {
            match delivery.deliver(&source, None).await {
                Ok(_) => break,
                Err(DeliveryError::Overloaded) if attempts < 200 => {
                    attempts += 1;
                    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                }
                Err(error) => panic!("released session must deliver: {error:?}"),
            }
        }
    }
}
