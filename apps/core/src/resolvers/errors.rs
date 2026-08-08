use thiserror::Error;

#[derive(Debug, Error)]
pub enum ResolverError {
    #[error("resolver request failed")]
    Request(#[source] reqwest::Error),

    #[error("resolver returned malformed JSON")]
    MalformedResponse(#[source] Box<dyn std::error::Error + Send + Sync>),

    #[error("resolver transport failed")]
    Transport(#[source] std::io::Error),

    #[error("resolver returned invalid catalogue data")]
    InvalidResponse(#[source] crate::tracks::ValidationError),

    #[error("resolver returned invalid playback data")]
    InvalidPlaybackResponse(#[source] crate::playback::ValidationError),

    #[error("resolver rejected the request")]
    InvalidRequest,

    #[error("resolver provider is unavailable")]
    ProviderUnavailable,

    #[error("resolver failed internally")]
    Internal,

    #[error("resolver returned an unexpected HTTP status: {0}")]
    UnexpectedStatus(reqwest::StatusCode),

    #[error("resolver source was not found")]
    SourceNotFound,

    #[error("resolver does not support the provider")]
    UnsupportedProvider,

    #[error("resolver could not resolve playback")]
    ResolutionFailed,
}
