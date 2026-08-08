use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ValidationError {
    #[error("playback URL is invalid")]
    InvalidUrl,

    #[error("{field} must not be empty")]
    Empty { field: &'static str },

    #[error("bitrate is invalid")]
    InvalidBitrate,

    #[error("content length is invalid")]
    InvalidContentLength,

    #[error("duration is invalid")]
    InvalidDuration,
}
