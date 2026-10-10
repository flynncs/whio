pub mod destinations;
mod postgres;
mod provisioning;
pub mod repository;
#[cfg(all(test, feature = "postgres-integration"))]
#[path = "test_repository.rs"]
mod test_repository;

use std::sync::Arc;

use sqlx::PgPool;

use super::destinations::{DestinationService, DestinationValidator};
use super::provisioning::ProvisioningService;
use super::secrets::CredentialKey;
use super::{service::CredentialService, throttle::LoginThrottleConfig};

pub use postgres::PostgresCredentialRepository;

pub(super) fn credentials(
    pool: PgPool,
    key: CredentialKey,
    throttle: LoginThrottleConfig,
) -> CredentialService {
    CredentialService::new(
        Arc::new(PostgresCredentialRepository::new(pool)),
        key,
        throttle,
    )
}

pub(super) fn disabled_destination_service(
    validator: Arc<dyn DestinationValidator>,
) -> DestinationService {
    DestinationService::new(
        Arc::new(self::destinations::UnavailableDestinationStore),
        None,
        validator,
    )
}

pub(super) fn destination_service(
    pool: PgPool,
    key: Option<CredentialKey>,
    validator: Arc<dyn DestinationValidator>,
) -> DestinationService {
    DestinationService::new(
        Arc::new(self::destinations::PostgresDestinationStore::new(pool)),
        key,
        validator,
    )
}

pub(super) fn provisioning_service(pool: PgPool) -> ProvisioningService {
    ProvisioningService::new(Arc::new(provisioning::PostgresProvisioningStore::new(pool)))
}
