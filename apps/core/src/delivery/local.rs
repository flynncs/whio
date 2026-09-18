use std::io::SeekFrom;

use axum::{
    body::Body,
    http::{
        HeaderMap, HeaderValue, StatusCode,
        header::{ACCEPT_RANGES, CONTENT_LENGTH, CONTENT_RANGE, CONTENT_TYPE},
    },
};
use tokio::io::{AsyncReadExt, AsyncSeekExt};
use tokio_util::io::ReaderStream;

use crate::{
    http::range::{RangeError, RangeRequest, parse_range_header},
    media::ByteRange,
    playback::{DeliveryError, DeliveryResult, LocalMedia},
};

pub(super) async fn serve(
    media: &LocalMedia,
    client_range: Option<String>,
) -> Result<DeliveryResult, DeliveryError> {
    let length = media.content_length();
    if length == 0 {
        return Err(DeliveryError::Range {
            content_length: 0,
            error: RangeError::Unsatisfiable,
        });
    }

    let request = parse_range_header(client_range.as_deref(), length).map_err(|error| {
        DeliveryError::Range {
            content_length: length,
            error,
        }
    })?;
    let range = match &request {
        RangeRequest::Full => ByteRange {
            start: 0,
            end: length - 1,
        },
        RangeRequest::Partial(range) => range.clone(),
    };

    let mut file = tokio::fs::File::open(media.path())
        .await
        .map_err(DeliveryError::Local)?;
    file.seek(SeekFrom::Start(range.start()))
        .await
        .map_err(DeliveryError::Local)?;

    let mut headers = HeaderMap::new();
    headers.insert(ACCEPT_RANGES, HeaderValue::from_static("bytes"));
    headers.insert(
        CONTENT_LENGTH,
        HeaderValue::from_str(&range.len().to_string()).expect("byte length header"),
    );
    if let Some(content_type) = media.content_type() {
        headers.insert(
            CONTENT_TYPE,
            HeaderValue::from_str(content_type).map_err(|_| {
                DeliveryError::Local(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "local media content type is not a header value",
                ))
            })?,
        );
    }

    let status = if matches!(request, RangeRequest::Partial(_)) {
        headers.insert(
            CONTENT_RANGE,
            HeaderValue::from_str(&format!(
                "bytes {}-{}/{}",
                range.start(),
                range.end(),
                length
            ))
            .expect("content range header"),
        );
        StatusCode::PARTIAL_CONTENT
    } else {
        StatusCode::OK
    };

    Ok(DeliveryResult {
        status,
        headers,
        body: Body::from_stream(ReaderStream::new(file.take(range.len()))),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn local_media(bytes: &[u8]) -> (std::path::PathBuf, LocalMedia) {
        let path = std::env::temp_dir().join(format!("whio-local-{}", uuid::Uuid::now_v7()));
        tokio::fs::write(&path, bytes).await.unwrap();
        let media = LocalMedia::new(
            path.clone(),
            bytes.len() as u64,
            Some("audio/flac".to_owned()),
        )
        .unwrap();
        (path, media)
    }

    async fn body_bytes(delivery: DeliveryResult) -> (StatusCode, HeaderMap, Vec<u8>) {
        let status = delivery.status;
        let headers = delivery.headers;
        let bytes = axum::body::to_bytes(delivery.body, 1024 * 1024)
            .await
            .unwrap();
        (status, headers, bytes.to_vec())
    }

    #[tokio::test]
    async fn serves_the_whole_file_and_a_requested_range() {
        let (path, media) = local_media(b"0123456789").await;

        let result = serve(&media, None).await.unwrap();
        let (status, headers, bytes) = body_bytes(result).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(headers[CONTENT_LENGTH], "10");
        assert_eq!(headers[CONTENT_TYPE], "audio/flac");
        assert_eq!(headers[ACCEPT_RANGES], "bytes");
        assert_eq!(bytes, b"0123456789");

        let result = serve(&media, Some("bytes=2-5".to_owned())).await.unwrap();
        let (status, headers, bytes) = body_bytes(result).await;
        assert_eq!(status, StatusCode::PARTIAL_CONTENT);
        assert_eq!(headers[CONTENT_LENGTH], "4");
        assert_eq!(headers[CONTENT_RANGE], "bytes 2-5/10");
        assert_eq!(bytes, b"2345");

        tokio::fs::remove_file(path).await.unwrap();
    }

    #[tokio::test]
    async fn an_open_ended_range_stops_at_the_end_of_the_file() {
        let (path, media) = local_media(b"0123456789").await;

        let result = serve(&media, Some("bytes=7-".to_owned())).await.unwrap();
        let (status, headers, bytes) = body_bytes(result).await;
        assert_eq!(status, StatusCode::PARTIAL_CONTENT);
        assert_eq!(headers[CONTENT_RANGE], "bytes 7-9/10");
        assert_eq!(bytes, b"789");

        tokio::fs::remove_file(path).await.unwrap();
    }

    #[tokio::test]
    async fn an_unsatisfiable_range_is_rejected_with_the_file_length() {
        let (path, media) = local_media(b"0123456789").await;

        let error = serve(&media, Some("bytes=10-".to_owned()))
            .await
            .err()
            .expect("a range beyond the file must fail");

        assert!(matches!(
            error,
            DeliveryError::Range {
                content_length: 10,
                ..
            }
        ));
        tokio::fs::remove_file(path).await.unwrap();
    }

    #[tokio::test]
    async fn a_vanished_file_is_a_local_read_failure_that_allows_fallback() {
        let (path, media) = local_media(b"0123456789").await;
        tokio::fs::remove_file(&path).await.unwrap();

        let error = serve(&media, None)
            .await
            .err()
            .expect("missing file must fail");

        assert!(matches!(error, DeliveryError::Local(_)));
        assert!(error.allows_fallback());
    }
}
