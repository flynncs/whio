use std::error::Error;

use thiserror::Error;

use crate::features::music_library::domain::ValidationError;

#[derive(Debug, Error)]
pub enum ProviderError {
    #[error("provider is disabled")]
    Disabled,
    #[error("provider request failed")]
    Request(#[source] Box<dyn Error + Send + Sync>),
    #[error("provider returned malformed data")]
    MalformedResponse(#[source] Box<dyn Error + Send + Sync>),
    #[error("provider returned invalid catalogue data")]
    InvalidCatalogueData(#[source] ValidationError),
    #[error("provider rejected the request")]
    InvalidRequest,
    #[error("provider is unavailable")]
    ProviderUnavailable,
    #[error("upstream credentials were rejected")]
    CredentialsRejected,
    #[error("linked account access could not be checked")]
    Access,
    #[error("provider failed internally")]
    Internal,
    #[error("provider source was not found")]
    SourceNotFound,
    #[error("provider does not support the source")]
    UnsupportedProvider,
}

impl ProviderError {
    pub fn request(error: impl Error + Send + Sync + 'static) -> Self {
        Self::Request(Box::new(error))
    }

    pub fn malformed(error: impl Error + Send + Sync + 'static) -> Self {
        Self::MalformedResponse(Box::new(error))
    }
}
