use std::sync::Arc;

use sqlx::PgPool;

use crate::tracks::ProviderRegistry;

mod catalogue;
pub(crate) mod decode;
mod library;
mod reports;
pub(crate) mod schema;
mod sources;
mod state;

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

    pub(crate) fn library_provider_ids(&self) -> Vec<String> {
        self.providers
            .library_ids()
            .into_iter()
            .map(str::to_owned)
            .collect()
    }
}
