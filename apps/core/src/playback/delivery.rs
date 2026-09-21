use axum::{
    body::Body,
    http::{HeaderMap, StatusCode},
};

use crate::{http::range::RangeError, media::MediaFetchError};

/// one resolved representation, delivered to the client.
pub struct DeliveryResult {
    pub status: StatusCode,
    pub headers: HeaderMap,
    pub body: Body,
}

/// failures raised while moving bytes: upstream media, spool and resource limits.
#[derive(Debug, thiserror::Error)]
pub enum DeliveryError {
    #[error("media fetch failed")]
    Media(#[from] MediaFetchError),

    #[error("playback spool failed")]
    Spool,

    #[error("local media could not be read")]
    Local(#[source] std::io::Error),

    #[error("client range was not satisfiable")]
    Range {
        content_length: u64,
        #[source]
        error: RangeError,
    },

    #[error("playback resource limit reached")]
    Overloaded,
}

impl DeliveryError {
    /// a failed attempt may try the next eligible source. matches the previous
    /// behaviour: direct, media and local failures fall back; spool, range and
    /// overload end the request.
    pub fn allows_fallback(&self) -> bool {
        !matches!(self, Self::Spool | Self::Range { .. } | Self::Overloaded)
    }

    /// the same source may be retried on the same request.
    pub fn allows_retry(&self) -> bool {
        match self {
            Self::Media(MediaFetchError::Request(error)) => {
                error.is_connect() || error.is_timeout()
            }
            Self::Media(MediaFetchError::UnexpectedStatus(status)) => {
                matches!(
                    *status,
                    StatusCode::FORBIDDEN | StatusCode::TOO_MANY_REQUESTS
                ) || status.is_server_error()
            }
            _ => false,
        }
    }
}
