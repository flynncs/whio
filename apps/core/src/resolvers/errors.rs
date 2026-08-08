use thiserror::Error;

#[derive(Debug, Error)]
pub enum ResolverError {
    #[error("resolver request failed")]
    Request(#[source] reqwest::Error),

    #[error("resolver returned malformed JSON")]
    MalformedResponse(#[source] Box<dyn std::error::Error + Send + Sync>),

    #[error("resolver transport failed")]
    Transport(#[source] std::io::Error),

    #[error("search limit must be between 1 and 25")]
    InvalidLimit,

    #[error("search query must be between 1 and 500 characters")]
    InvalidQuery,

    #[error("request ID must be between 1 and 128 characters")]
    InvalidRequestId,

    #[error("resolver returned invalid catalogue data")]
    InvalidResponse(#[source] crate::catalogue::ValidationError),

    #[error("resolver rejected the request")]
    InvalidRequest,

    #[error("resolver provider is unavailable")]
    ProviderUnavailable,

    #[error("resolver failed internally")]
    Internal,

    #[error("resolver returned an unexpected HTTP status: {0}")]
    UnexpectedStatus(reqwest::StatusCode),
}
