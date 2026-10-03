use std::sync::Arc;

use sqlx::PgPool;

use crate::features::music_library::availability::ProviderRegistry;
use crate::features::music_library::delivery::DeliveryService;

mod artwork;
mod catalogue;
pub(crate) mod decode;
mod delivery;
mod enrichment;
mod history;
mod libraries;
mod library;
mod playlists;
mod queue;
mod reports;
pub(crate) mod schema;
mod sources;
mod state;

pub use libraries::PostgresLibraryStore;

#[cfg(all(test, feature = "postgres-integration"))]
#[path = "test_artist_name_read.rs"]
mod test_artist_name_read;
#[cfg(all(test, feature = "postgres-integration"))]
#[path = "test_artwork.rs"]
mod test_artwork;
#[cfg(all(test, feature = "postgres-integration"))]
#[path = "test_catalogue_keys.rs"]
mod test_catalogue_keys;
#[cfg(all(test, feature = "postgres-integration"))]
#[path = "test_contracts.rs"]
mod test_contracts;
#[cfg(all(test, feature = "postgres-integration"))]
#[path = "test_delivery.rs"]
mod test_delivery;
#[cfg(all(test, feature = "postgres-integration"))]
#[path = "test_discovery.rs"]
mod test_discovery;
#[cfg(all(test, feature = "postgres-integration"))]
#[path = "test_enrichment.rs"]
mod test_enrichment;
#[cfg(all(test, feature = "postgres-integration"))]
#[path = "test_history.rs"]
mod test_history;
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
#[path = "test_query_artists.rs"]
mod test_query_artists;
#[cfg(all(test, feature = "postgres-integration"))]
#[path = "test_query_tracks.rs"]
mod test_query_tracks;
#[cfg(all(test, feature = "postgres-integration"))]
#[path = "test_queue.rs"]
mod test_queue;
#[cfg(all(test, feature = "postgres-integration"))]
#[path = "test_retention.rs"]
mod test_retention;
#[cfg(all(test, feature = "postgres-integration"))]
#[path = "test_track_repository.rs"]
mod test_track_repository;

pub struct PostgresTrackRepository {
    pool: PgPool,
    providers: Arc<ProviderRegistry>,
    delivery: Option<Arc<DeliveryService>>,
}

pub(crate) type PgStore = PostgresTrackRepository;

impl PostgresTrackRepository {
    pub fn new(pool: PgPool) -> Self {
        Self {
            pool,
            providers: Arc::new(ProviderRegistry::builtin()),
            delivery: None,
        }
    }
    pub(crate) fn with_registry(mut self, registry: ProviderRegistry) -> Self {
        self.providers = Arc::new(registry);
        self
    }

    pub fn with_delivery(mut self, delivery: Arc<DeliveryService>) -> Self {
        self.delivery = Some(delivery);
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
