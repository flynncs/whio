//! principals and client credentials; storage design in ADR-002

pub(crate) mod api;
pub mod destinations;
pub mod domain;
pub mod provisioning;
pub mod secrets;
pub mod service;
mod storage;

pub use api::destinations::{DestinationState, routes as destinations_router};

#[cfg(any(test, feature = "postgres-integration"))]
pub mod test_support {
    pub use super::storage::repository::{CredentialRepository, CredentialStoreError};
}

pub fn credentials(pool: sqlx::PgPool, key: secrets::CredentialKey) -> service::CredentialService {
    storage::credentials(pool, key)
}

pub fn destinations(
    pool: sqlx::PgPool,
    key: Option<secrets::CredentialKey>,
    validator: std::sync::Arc<dyn destinations::DestinationValidator>,
) -> destinations::DestinationService {
    storage::destination_service(pool, key, validator)
}

pub fn disabled_destinations(
    validator: std::sync::Arc<dyn destinations::DestinationValidator>,
) -> destinations::DestinationService {
    storage::disabled_destination_service(validator)
}

pub fn provision(pool: sqlx::PgPool) -> provisioning::ProvisioningService {
    storage::provisioning_service(pool)
}
