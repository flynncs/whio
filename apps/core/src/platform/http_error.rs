#[derive(Debug, thiserror::Error)]
pub enum FetchError {
    #[error("media request failed")]
    Request(#[source] reqwest::Error),
    #[error("media response returned unexpected status: {0}")]
    UnexpectedStatus(reqwest::StatusCode),
    #[error("media response did not include a content length")]
    MissingContentLength,
    #[error("media response did not include a valid content range")]
    InvalidContentRange,
    #[error("media source does not support byte ranges")]
    RangesUnsupported,
    #[error("media destination is not allowed")]
    DisallowedDestination,
    #[error("media response was too large")]
    ResponseTooLarge,
    #[error("media conversion failed")]
    Conversion,
}
