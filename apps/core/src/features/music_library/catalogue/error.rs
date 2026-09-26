use thiserror::Error;

use crate::{
    features::music_library::catalogue::ProviderError,
    features::music_library::storage::contracts::TrackRepositoryError,
};

#[derive(Debug, Error)]
pub enum CatalogueError {
    #[error("catalogue provider failed")]
    Provider(#[source] ProviderError),

    #[error("track repository failed")]
    Repository(#[source] TrackRepositoryError),
}
