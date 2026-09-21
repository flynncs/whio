mod catalogue;
mod error;
mod library;
mod state;

pub use catalogue::CatalogueWrite;
pub use error::{SourceFileFacts, TrackRepositoryError};
pub use library::{LibraryRead, SourceLookup};
pub use state::{DuplicateReport, StateStore};

use async_trait::async_trait;

#[async_trait]
pub trait TrackRepository:
    LibraryRead + CatalogueWrite + SourceLookup + StateStore + DuplicateReport
{
}

#[async_trait]
impl<T> TrackRepository for T where
    T: LibraryRead + CatalogueWrite + SourceLookup + StateStore + DuplicateReport
{
}
