mod postgres;
mod provisioning;
pub mod repository;
#[cfg(all(test, feature = "postgres-integration"))]
#[path = "test_repository.rs"]
mod test_repository;

use std::sync::Arc;

use sqlx::PgPool;

use super::provisioning::ProvisioningService;
use super::secrets::CredentialKey;
use super::service::CredentialService;

pub use postgres::PostgresCredentialRepository;

pub(super) fn credentials(pool: PgPool, key: CredentialKey) -> CredentialService {
    CredentialService::new(Arc::new(PostgresCredentialRepository::new(pool)), key)
}

pub(super) fn provisioning_service(pool: PgPool) -> ProvisioningService {
    ProvisioningService::new(Arc::new(provisioning::PostgresProvisioningStore::new(pool)))
}
