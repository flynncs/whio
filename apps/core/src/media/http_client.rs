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

        let response = req.send().await.map_err(MediaFetchError::Request)?;

        if response.status() != StatusCode::OK {
            return Err(MediaFetchError::UnexpectedStatus(response.status()));
        }

        let content_length = response.content_length();

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
    use super::*;

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
