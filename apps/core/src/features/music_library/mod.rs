pub use storage::{MusicServices, construct};
pub mod availability;

pub mod domain;
mod storage;
#[cfg(any(test, feature = "postgres-integration"))]
pub mod test_support {
    pub use super::storage::contracts::{
        CatalogueWrite, DuplicateReport, LibraryRead, PlaylistStore, Retention, SourceFileFacts,
        SourceLookup, StateStore, TrackRepositoryError,
    };
    #[cfg(feature = "postgres-integration")]
    pub use super::storage::postgres::PostgresTrackRepository;
}

pub(crate) mod api;
pub mod artwork;
pub mod catalogue;
pub mod library;
pub mod library_state;
pub mod maintenance;
pub mod playlists;
pub mod providers;

pub(crate) use storage::maintenance;
pub mod sources;
