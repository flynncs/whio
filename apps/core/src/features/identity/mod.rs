//! principals and client credentials; storage design in ADR-002

pub(crate) mod api;
pub mod domain;
pub mod provisioning;
pub mod secrets;
pub mod service;
mod storage;
#[cfg(any(test, feature = "postgres-integration"))]
pub mod test_support {
    pub use super::storage::repository::{CredentialRepository, CredentialStoreError};
}

pub fn credentials(pool: sqlx::PgPool, key: secrets::CredentialKey) -> service::CredentialService {
    storage::credentials(pool, key)
}

pub fn provision(pool: sqlx::PgPool) -> provisioning::ProvisioningService {
    storage::provisioning_service(pool)
}
