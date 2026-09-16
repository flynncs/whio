use std::error::Error;

use thiserror::Error;

use crate::{
    playback::ValidationError as PlaybackValidationError,
    tracks::ValidationError as TrackValidationError,
};

#[derive(Debug, Error)]
pub enum ProviderError {
    #[error("provider is disabled")]
    Disabled,

    #[error("provider request failed")]
    Request(#[source] Box<dyn Error + Send + Sync>),

    #[error("provider returned malformed data")]
    MalformedResponse(#[source] Box<dyn Error + Send + Sync>),

    #[error("provider returned invalid data")]
    InvalidResponse(#[source] Box<dyn Error + Send + Sync>),

    #[error("provider returned invalid catalogue data")]
    InvalidCatalogueData(#[source] TrackValidationError),

    #[error("provider returned invalid playback data")]
    InvalidPlaybackData(#[source] PlaybackValidationError),

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

    #[error("provider could not resolve playback")]
    ResolutionFailed,
}

impl ProviderError {
    pub fn request(error: impl Error + Send + Sync + 'static) -> Self {
        Self::Request(Box::new(error))
    }

    pub fn malformed(error: impl Error + Send + Sync + 'static) -> Self {
        Self::MalformedResponse(Box::new(error))
    }

    pub fn invalid_response(error: impl Error + Send + Sync + 'static) -> Self {
        Self::InvalidResponse(Box::new(error))
    }
}
