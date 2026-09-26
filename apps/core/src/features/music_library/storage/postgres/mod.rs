use std::sync::Arc;

use sqlx::PgPool;

use crate::features::music_library::availability::ProviderRegistry;

mod catalogue;
pub(crate) mod decode;
mod libraries;
mod library;
mod playlists;
mod reports;
pub(crate) mod schema;
mod sources;
mod state;

pub use libraries::PostgresLibraryStore;

#[cfg(all(test, feature = "postgres-integration"))]
mod test_contracts;
#[cfg(all(test, feature = "postgres-integration"))]
#[path = "test_libraries.rs"]
mod test_libraries;
#[cfg(all(test, feature = "postgres-integration"))]
#[path = "test_playlists.rs"]
mod test_playlists;
#[cfg(all(test, feature = "postgres-integration"))]
#[path = "test_provider_state.rs"]
mod test_provider_state;
#[cfg(all(test, feature = "postgres-integration"))]
#[path = "test_query_albums.rs"]
mod test_query_albums;
#[cfg(all(test, feature = "postgres-integration"))]
#[path = "test_query_tracks.rs"]
mod test_query_tracks;
#[cfg(all(test, feature = "postgres-integration"))]
#[path = "test_retention.rs"]
mod test_retention;
#[cfg(all(test, feature = "postgres-integration"))]
#[path = "test_track_repository.rs"]
mod test_track_repository;

pub struct PostgresTrackRepository {
    pool: PgPool,
    providers: Arc<ProviderRegistry>,
}

pub(crate) type PgStore = PostgresTrackRepository;

impl PostgresTrackRepository {
    pub fn new(pool: PgPool) -> Self {
        Self {
            pool,
            providers: Arc::new(ProviderRegistry::builtin()),
        }
    }
    pub(crate) fn with_registry(mut self, registry: ProviderRegistry) -> Self {
        self.providers = Arc::new(registry);
        self
    }

    pub(crate) fn library_provider_ids(&self) -> Vec<String> {
        self.providers
            .library_ids()
            .into_iter()
            .map(str::to_owned)
            .collect()
    }
}
