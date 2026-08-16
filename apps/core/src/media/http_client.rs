use reqwest::{
    StatusCode,
    header::{CONTENT_RANGE, RANGE},
};

use tokio_stream::StreamExt;

use crate::{
    media::{
        ByteRange, MediaFetcher, MediaInfo,
        ports::{MediaBody, MediaFetchError},
    },
    playback::PlayableMedia,
};

pub struct HttpMediaFetcher {
    client: reqwest::Client,
}

impl HttpMediaFetcher {
    pub fn new(client: reqwest::Client) -> Self {
        Self { client }
    }
}

#[async_trait::async_trait]
impl MediaFetcher for HttpMediaFetcher {
    async fn probe(&self, media: &PlayableMedia) -> Result<MediaInfo, MediaFetchError> {
        let mut req = self.client.get(media.url().as_url().clone());

        for (name, val) in media.headers() {
            req = req.header(name, val);
        }

        let resp = req
            .header(RANGE, "bytes=0-0")
            .send()
            .await
            .map_err(MediaFetchError::Request)?;

        match resp.status() {
            StatusCode::PARTIAL_CONTENT => {
                let content_range = resp
                    .headers()
                    .get(CONTENT_RANGE)
                    .ok_or(MediaFetchError::InvalidContentRange)?
                    .to_str()
                    .map_err(|_| MediaFetchError::InvalidContentRange)?;

                let (parsed_range, length) = parse_content_range(content_range)?;

                if parsed_range.start() != 0 || parsed_range.end() != 0 {
                    return Err(MediaFetchError::InvalidContentRange);
                }

                return Ok(MediaInfo {
                    content_length: length,
                    supports_ranges: true,
                });
            }
            StatusCode::OK => return Err(MediaFetchError::RangesUnsupported),
            status => return Err(MediaFetchError::UnexpectedStatus(status)),
        }
    }

    async fn open_continuous(&self, media: &PlayableMedia) -> Result<MediaBody, MediaFetchError> {
        let mut req = self.client.get(media.url().as_url().clone());

        for (name, val) in media.headers() {
            req = req.header(name, val);
        }

        let response = req
            .header(RANGE, "bytes=0-")
            .send()
            .await
            .map_err(MediaFetchError::Request)?;

        let content_length = match response.status() {
            StatusCode::OK => response.content_length(),
            StatusCode::PARTIAL_CONTENT => {
                let content_range = response
                    .headers()
                    .get(CONTENT_RANGE)
                    .ok_or(MediaFetchError::InvalidContentRange)?
                    .to_str()
                    .map_err(|_| MediaFetchError::InvalidContentRange)?;
                let (range, length) = parse_content_range(content_range)?;

                if range.start() != 0 || range.end() + 1 != length {
                    return Err(MediaFetchError::InvalidContentRange);
                }

                Some(length)
            }
            status => return Err(MediaFetchError::UnexpectedStatus(status)),
        };

        let chunks = response
            .bytes_stream()
            .map(|chunk| chunk.map_err(MediaFetchError::Request));

        Ok(MediaBody {
            content_length,
            chunks: Box::pin(chunks),
        })
    }
}

fn parse_content_range(content_range: &str) -> Result<(ByteRange, u64), MediaFetchError> {
    let (unit, rest) = content_range
        .split_once(' ')
        .ok_or(MediaFetchError::InvalidContentRange)?;

    if !unit.eq_ignore_ascii_case("bytes") {
        return Err(MediaFetchError::InvalidContentRange);
    }

    let (range, length) = rest
        .split_once('/')
        .ok_or(MediaFetchError::InvalidContentRange)?;

    let (start, end) = range
        .split_once('-')
        .ok_or(MediaFetchError::InvalidContentRange)?;

    let start = start
        .parse::<u64>()
        .map_err(|_| MediaFetchError::InvalidContentRange)?;
    let end = end
        .parse::<u64>()
        .map_err(|_| MediaFetchError::InvalidContentRange)?;

    let length = length
        .parse::<u64>()
        .map_err(|_| MediaFetchError::InvalidContentRange)?;

    if start > end || length == 0 || end >= length {
        return Err(MediaFetchError::InvalidContentRange);
    }

    Ok((ByteRange { start, end }, length))
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use axum::{
        Router,
        body::Body,
        http::{HeaderMap, StatusCode, header},
        response::Response,
        routing::get,
    };
    use tokio::net::TcpListener;

    use crate::playback::{MediaMetadata, PlaybackUrl};

    use super::*;

    async fn range_only_media(headers: HeaderMap) -> Response {
        if headers
            .get(header::RANGE)
            .and_then(|value| value.to_str().ok())
            != Some("bytes=0-")
        {
            return Response::builder()
                .status(StatusCode::FORBIDDEN)
                .body(Body::empty())
                .unwrap();
        }

        Response::builder()
            .status(StatusCode::PARTIAL_CONTENT)
            .header(header::CONTENT_RANGE, "bytes 0-3/4")
            .header(header::CONTENT_LENGTH, "4")
            .body(Body::from("test"))
            .unwrap()
    }

    #[tokio::test]
    async fn continuous_fetch_uses_an_open_ended_range() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(
                listener,
                Router::new().route("/media", get(range_only_media)),
            )
            .await
            .unwrap();
        });

        let media = PlayableMedia::new(
            PlaybackUrl::new(format!("http://{address}/media")).unwrap(),
            HashMap::new(),
            None,
            MediaMetadata::new(None, None, None, None, None).unwrap(),
        );
        let fetcher = HttpMediaFetcher::new(reqwest::Client::new());

        let body = fetcher.open_continuous(&media).await.unwrap();
        assert_eq!(body.content_length, Some(4));

        let bytes = body
            .chunks
            .collect::<Result<Vec<_>, _>>()
            .await
            .unwrap()
            .concat();
        assert_eq!(bytes, b"test");

        server.abort();
    }

    #[test]
    fn parses_content_range() {
        let (range, length) = parse_content_range("bytes 100-199/1000").unwrap();

        assert_eq!(range.start(), 100);
        assert_eq!(range.end(), 199);
        assert_eq!(length, 1000);
    }

    #[test]
    fn rejects_invalid_content_ranges() {
        for value in [
            "bits 0-0/1000",
            "bytes 100-99/1000",
            "bytes 0-1000/1000",
            "bytes 0-0/0",
        ] {
            assert!(parse_content_range(value).is_err(), "accepted {value}");
        }
    }
}
