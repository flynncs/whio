pub mod api;
pub mod config;
pub mod domain;
pub mod pin;
pub mod ports;
pub mod selection;
pub mod service;
pub(crate) mod storage;

#[cfg(any(test, feature = "postgres-integration"))]
pub mod test_support {
    use std::sync::Arc;

    use sqlx::PgPool;
    use uuid::Uuid;

    use super::config::AcquisitionConfig;
    use super::domain::RequestState;
    use super::service::{AcquisitionPorts, AcquisitionService};
    use super::storage::PostgresAcquisitionStore;
    use crate::features::music_library::catalogue::LibraryService;
    use crate::platform::clock::Clock;

    pub fn service(
        pool: PgPool,
        ports: AcquisitionPorts,
        library: Arc<LibraryService>,
        config: AcquisitionConfig,
    ) -> Arc<AcquisitionService> {
        Arc::new(AcquisitionService::new(
            Arc::new(PostgresAcquisitionStore::new(pool)),
            ports,
            library,
            config,
            Clock::system(),
        ))
    }

    pub async fn mark_terminal(
        pool: PgPool,
        id: Uuid,
        state: RequestState,
    ) -> Result<(), sqlx::Error> {
        PostgresAcquisitionStore::new(pool)
            .set_terminal(id, state, None)
            .await
            .map_err(|_| sqlx::Error::RowNotFound)
    }
}

#[cfg(all(test, feature = "postgres-integration"))]
#[path = "service_tests.rs"]
mod service_tests;
