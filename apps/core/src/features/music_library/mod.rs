pub(crate) use storage::contracts::TrackRepositoryError;
pub use storage::contracts::{ArtworkStore, DeliveryStore, EnrichmentGuard, ListenStore};
pub use storage::postgres::PostgresTrackRepository;
pub use storage::{
    ArtworkCacheWiring, DeliveryWiring, LibraryWiring, MetadataWiring, MusicServices,
    W4Dependencies, construct,
};
pub mod availability;

pub mod domain;
mod storage;
#[cfg(any(test, feature = "postgres-integration"))]
pub mod test_support {
    pub use super::storage::contracts::{
        CatalogueWrite, DeliveryStore, DuplicateReport, LibraryRead, LibraryStore, ListenStore,
        PlaylistStore, RestoreQueueStore, Retention, SourceFileFacts, SourceLookup, StateStore,
        TrackRepositoryError,
    };
    #[cfg(feature = "postgres-integration")]
    pub use super::storage::postgres::{PostgresLibraryStore, PostgresTrackRepository};
}

pub(crate) mod api;
pub use api::history::{ListenDeliveryState, delivery_routes};
pub mod artwork;
pub mod catalogue;
pub mod delivery;
pub mod discovery;
pub mod history;
pub mod library;
pub mod library_state;
pub mod maintenance;
pub mod playlists;
pub mod providers;
pub mod queue;

pub(crate) use storage::maintenance;
pub mod sources;

#[cfg(test)]
#[path = "discovery_tests.rs"]
mod discovery_tests;

#[cfg(test)]
#[path = "delivery_tests.rs"]
mod delivery_tests;
