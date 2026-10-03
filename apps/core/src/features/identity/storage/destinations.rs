use std::fmt;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sqlx::PgPool;
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DestinationStoreError {
    #[error("destination storage is unavailable")]
    Unavailable,

    #[error("destination storage returned invalid data")]
    InvalidData,
}

#[derive(Clone)]
pub struct DestinationAccount {
    pub id: Uuid,
    pub user_id: Uuid,
    pub provider_id: String,
    pub external_username: Option<String>,
    pub encrypted_secret: Vec<u8>,
    pub enabled: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub last_verified_at: Option<DateTime<Utc>>,
    pub revoked_at: Option<DateTime<Utc>>,
}

impl fmt::Debug for DestinationAccount {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DestinationAccount")
            .field("id", &self.id)
            .field("user_id", &self.user_id)
            .field("provider_id", &self.provider_id)
            .field("external_username", &self.external_username)
            .field("enabled", &self.enabled)
            .field("created_at", &self.created_at)
            .field("updated_at", &self.updated_at)
            .field("last_verified_at", &self.last_verified_at)
            .field("revoked_at", &self.revoked_at)
            .finish_non_exhaustive()
    }
}

#[async_trait]
pub trait DestinationStore: Send + Sync {
    async fn upsert_account(
        &self,
        account: &DestinationAccount,
    ) -> Result<(), DestinationStoreError>;

    async fn find_account(
        &self,
        user_id: Uuid,
        provider_id: &str,
    ) -> Result<Option<DestinationAccount>, DestinationStoreError>;

    async fn list_accounts(
        &self,
        user_id: Uuid,
    ) -> Result<Vec<DestinationAccount>, DestinationStoreError>;

    async fn set_enabled(
        &self,
        destination_id: Uuid,
        enabled: bool,
    ) -> Result<bool, DestinationStoreError>;

    async fn revoke(&self, destination_id: Uuid) -> Result<bool, DestinationStoreError>;

    async fn list_enabled(
        &self,
        user_id: Uuid,
    ) -> Result<Vec<DestinationAccount>, DestinationStoreError>;
}

pub struct PostgresDestinationStore {
    pool: PgPool,
}

impl PostgresDestinationStore {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

type AccountRow = (
    Uuid,
    Uuid,
    String,
    Option<String>,
    Vec<u8>,
    bool,
    DateTime<Utc>,
    DateTime<Utc>,
    Option<DateTime<Utc>>,
    Option<DateTime<Utc>>,
);

fn account(row: AccountRow) -> DestinationAccount {
    let (
        id,
        user_id,
        provider_id,
        external_username,
        encrypted_secret,
        enabled,
        created_at,
        updated_at,
        last_verified_at,
        revoked_at,
    ) = row;

    DestinationAccount {
        id,
        user_id,
        provider_id,
        external_username,
        encrypted_secret,
        enabled,
        created_at,
        updated_at,
        last_verified_at,
        revoked_at,
    }
}

#[async_trait]
impl DestinationStore for PostgresDestinationStore {
    async fn upsert_account(
        &self,
        account: &DestinationAccount,
    ) -> Result<(), DestinationStoreError> {
        sqlx::query(
            r#"
            INSERT INTO destination_accounts
                (id, user_id, provider_id, external_username, encrypted_secret, enabled,
                 created_at, updated_at, last_verified_at, revoked_at)
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
            ON CONFLICT (user_id, provider_id) DO UPDATE SET
                external_username = EXCLUDED.external_username,
                encrypted_secret = EXCLUDED.encrypted_secret,
                enabled = EXCLUDED.enabled,
                updated_at = EXCLUDED.updated_at,
                last_verified_at = EXCLUDED.last_verified_at,
                revoked_at = EXCLUDED.revoked_at
            "#,
        )
        .bind(account.id)
        .bind(account.user_id)
        .bind(&account.provider_id)
        .bind(&account.external_username)
        .bind(&account.encrypted_secret)
        .bind(account.enabled)
        .bind(account.created_at)
        .bind(account.updated_at)
        .bind(account.last_verified_at)
        .bind(account.revoked_at)
        .execute(&self.pool)
        .await
        .map_err(|_| DestinationStoreError::Unavailable)?;

        Ok(())
    }

    async fn find_account(
        &self,
        user_id: Uuid,
        provider_id: &str,
    ) -> Result<Option<DestinationAccount>, DestinationStoreError> {
        let row = sqlx::query_as::<_, AccountRow>(
            r#"
            SELECT id, user_id, provider_id, external_username, encrypted_secret, enabled,
                   created_at, updated_at, last_verified_at, revoked_at
            FROM destination_accounts
            WHERE user_id = $1 AND provider_id = $2
            "#,
        )
        .bind(user_id)
        .bind(provider_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|_| DestinationStoreError::Unavailable)?;

        Ok(row.map(account))
    }

    async fn list_accounts(
        &self,
        user_id: Uuid,
    ) -> Result<Vec<DestinationAccount>, DestinationStoreError> {
        let rows = sqlx::query_as::<_, AccountRow>(
            r#"
            SELECT id, user_id, provider_id, external_username, encrypted_secret, enabled,
                   created_at, updated_at, last_verified_at, revoked_at
            FROM destination_accounts
            WHERE user_id = $1
            ORDER BY provider_id
            "#,
        )
        .bind(user_id)
        .fetch_all(&self.pool)
        .await
        .map_err(|_| DestinationStoreError::Unavailable)?;

        Ok(rows.into_iter().map(account).collect())
    }

    async fn set_enabled(
        &self,
        destination_id: Uuid,
        enabled: bool,
    ) -> Result<bool, DestinationStoreError> {
        let result = sqlx::query(
            "UPDATE destination_accounts SET enabled = $2, updated_at = CURRENT_TIMESTAMP WHERE id = $1",
        )
        .bind(destination_id)
        .bind(enabled)
        .execute(&self.pool)
        .await
        .map_err(|_| DestinationStoreError::Unavailable)?;

        Ok(result.rows_affected() > 0)
    }

    async fn revoke(&self, destination_id: Uuid) -> Result<bool, DestinationStoreError> {
        let result = sqlx::query(
            "UPDATE destination_accounts SET revoked_at = CURRENT_TIMESTAMP, updated_at = CURRENT_TIMESTAMP \
             WHERE id = $1 AND revoked_at IS NULL",
        )
        .bind(destination_id)
        .execute(&self.pool)
        .await
        .map_err(|_| DestinationStoreError::Unavailable)?;

        Ok(result.rows_affected() > 0)
    }

    async fn list_enabled(
        &self,
        user_id: Uuid,
    ) -> Result<Vec<DestinationAccount>, DestinationStoreError> {
        let rows = sqlx::query_as::<_, AccountRow>(
            r#"
            SELECT id, user_id, provider_id, external_username, encrypted_secret, enabled,
                   created_at, updated_at, last_verified_at, revoked_at
            FROM destination_accounts
            WHERE user_id = $1 AND enabled AND revoked_at IS NULL
            ORDER BY provider_id
            "#,
        )
        .bind(user_id)
        .fetch_all(&self.pool)
        .await
        .map_err(|_| DestinationStoreError::Unavailable)?;

        Ok(rows.into_iter().map(account).collect())
    }
}

pub(crate) struct UnavailableDestinationStore;

#[async_trait]
impl DestinationStore for UnavailableDestinationStore {
    async fn upsert_account(
        &self,
        _account: &DestinationAccount,
    ) -> Result<(), DestinationStoreError> {
        Err(DestinationStoreError::Unavailable)
    }

    async fn find_account(
        &self,
        _user_id: Uuid,
        _provider_id: &str,
    ) -> Result<Option<DestinationAccount>, DestinationStoreError> {
        Err(DestinationStoreError::Unavailable)
    }

    async fn list_accounts(
        &self,
        _user_id: Uuid,
    ) -> Result<Vec<DestinationAccount>, DestinationStoreError> {
        Err(DestinationStoreError::Unavailable)
    }

    async fn set_enabled(&self, _id: Uuid, _enabled: bool) -> Result<bool, DestinationStoreError> {
        Err(DestinationStoreError::Unavailable)
    }

    async fn revoke(&self, _id: Uuid) -> Result<bool, DestinationStoreError> {
        Err(DestinationStoreError::Unavailable)
    }

    async fn list_enabled(
        &self,
        _user_id: Uuid,
    ) -> Result<Vec<DestinationAccount>, DestinationStoreError> {
        Err(DestinationStoreError::Unavailable)
    }
}
