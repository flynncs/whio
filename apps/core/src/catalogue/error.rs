use thiserror::Error;

use crate::{providers::ProviderError, tracks::TrackRepositoryError};

#[derive(Debug, Error)]
pub enum CatalogueError {
    #[error("catalogue provider failed")]
    Provider(#[source] ProviderError),

    #[error("track repository failed")]
    Repository(#[source] TrackRepositoryError),
}
