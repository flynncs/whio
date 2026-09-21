mod catalogue;
mod error;
mod library;
mod playlist;
mod state;

pub use catalogue::CatalogueWrite;
pub use error::{SourceFileFacts, TrackRepositoryError};
pub use library::{LibraryRead, SourceLookup};
pub use playlist::PlaylistStore;
pub use state::{DuplicateReport, Retention, StateStore};

use async_trait::async_trait;

#[async_trait]
pub trait TrackRepository:
    LibraryRead + CatalogueWrite + SourceLookup + StateStore + DuplicateReport + PlaylistStore
{
}

#[async_trait]
impl<T> TrackRepository for T where
    T: LibraryRead + CatalogueWrite + SourceLookup + StateStore + DuplicateReport + PlaylistStore
{
}
