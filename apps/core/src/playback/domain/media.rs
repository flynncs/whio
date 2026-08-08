use std::{collections::HashMap, matches};

use chrono::{DateTime, Utc};
use url::Url;

use crate::playback::domain::error::ValidationError;

#[derive(Debug, Clone, PartialEq)]
pub struct PlaybackUrl(Url);

impl PlaybackUrl {
    pub fn new(value: String) -> Result<Self, ValidationError> {
        let url = Url::parse(&value).map_err(|_| ValidationError::InvalidUrl)?;

        if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
            return Err(ValidationError::InvalidUrl);
        }

        Ok(Self(url))
    }

    pub fn as_url(&self) -> &Url {
        &self.0
    }

    pub fn into_url(self) -> Url {
        self.0
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct PlayableMedia {
    url: PlaybackUrl,
    headers: HashMap<String, String>,
    expires_at: Option<DateTime<Utc>>,
    metadata: MediaMetadata,
}

impl PlayableMedia {
    pub fn new(
        url: PlaybackUrl,
        headers: HashMap<String, String>,
        expires_at: Option<DateTime<Utc>>,
        metadata: MediaMetadata,
    ) -> Self {
        Self {
            url,
            headers,
            expires_at,
            metadata,
        }
    }

    pub fn into_parts(
        self,
    ) -> (
        PlaybackUrl,
        HashMap<String, String>,
        Option<DateTime<Utc>>,
        MediaMetadata,
    ) {
        (self.url, self.headers, self.expires_at, self.metadata)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct MediaMetadata {
    content_type: Option<String>,
    content_length_bytes: Option<u64>,
    codec: Option<String>,
    bitrate_kbps: Option<f64>,
    duration_ms: Option<u64>,
}

impl MediaMetadata {
    pub fn new(
        content_type: Option<String>,
        content_length_bytes: Option<u64>,
        codec: Option<String>,
        bitrate_kbps: Option<f64>,
        duration_ms: Option<u64>,
    ) -> Result<Self, ValidationError> {
        if content_type.as_deref().is_some_and(str::is_empty) {
            return Err(ValidationError::Empty {
                field: "content_type",
            });
        }

        if codec.as_deref().is_some_and(str::is_empty) {
            return Err(ValidationError::Empty { field: "codec" });
        }

        if bitrate_kbps.is_some_and(|bitrate| !bitrate.is_finite() || bitrate <= 0.0) {
            return Err(ValidationError::InvalidBitrate);
        }

        Ok(Self {
            content_type,
            content_length_bytes,
            codec,
            bitrate_kbps,
            duration_ms,
        })
    }

    pub fn content_type(&self) -> Option<&str> {
        self.content_type.as_deref()
    }

    pub fn content_length_bytes(&self) -> Option<u64> {
        self.content_length_bytes
    }

    pub fn codec(&self) -> Option<&str> {
        self.codec.as_deref()
    }

    pub fn bitrate_kbps(&self) -> Option<f64> {
        self.bitrate_kbps
    }

    pub fn duration_ms(&self) -> Option<u64> {
        self.duration_ms
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn playback_url_accepts_only_hosted_http_urls() {
        for value in ["https://media.example.test/audio", "http://localhost/audio"] {
            assert!(PlaybackUrl::new(value.to_owned()).is_ok(), "{value}");
        }

        for value in ["not a URL", "file:///tmp/audio", "https://"] {
            assert!(PlaybackUrl::new(value.to_owned()).is_err(), "{value}");
        }
    }

    #[test]
    fn media_metadata_rejects_invalid_optional_values() {
        assert!(matches!(
            MediaMetadata::new(Some(String::new()), None, None, None, None),
            Err(ValidationError::Empty {
                field: "content_type"
            })
        ));
        assert!(matches!(
            MediaMetadata::new(None, None, Some(String::new()), None, None),
            Err(ValidationError::Empty { field: "codec" })
        ));

        for bitrate in [0.0, -1.0, f64::NAN, f64::INFINITY] {
            assert!(matches!(
                MediaMetadata::new(None, None, None, Some(bitrate), None),
                Err(ValidationError::InvalidBitrate)
            ));
        }
    }
}
