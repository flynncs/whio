use async_trait::async_trait;
use sqlx::PgPool;
use uuid::Uuid;

use crate::features::identity::domain::{UserId, UserRole};
use crate::features::identity::provisioning::{ProvisioningStore, ProvisioningStoreError};

pub(super) struct PostgresProvisioningStore {
    pool: PgPool,
}

impl PostgresProvisioningStore {
    pub(super) fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl ProvisioningStore for PostgresProvisioningStore {
    async fn insert_user(
        &self,
        id: Uuid,
        username: &str,
        display_name: &str,
    ) -> Result<(), ProvisioningStoreError> {
        sqlx::query("INSERT INTO users (id, username, display_name) VALUES ($1, $2, $3)")
            .bind(id)
            .bind(username)
            .bind(display_name)
            .execute(&self.pool)
            .await
            .map_err(map_database_error)?;
        Ok(())
    }

    async fn user_id(&self, username: &str) -> Result<Option<Uuid>, ProvisioningStoreError> {
        sqlx::query_scalar::<_, Uuid>("SELECT id FROM users WHERE lower(username) = lower($1)")
            .bind(username)
            .fetch_optional(&self.pool)
            .await
            .map_err(unavailable)
    }

    async fn insert_password(
        &self,
        id: Uuid,
        user_id: Uuid,
        label: &str,
        encrypted_secret: Vec<u8>,
    ) -> Result<(), ProvisioningStoreError> {
        sqlx::query(
            r#"INSERT INTO subsonic_app_passwords
               (id, user_id, label, encrypted_secret)
               VALUES ($1, $2, $3, $4)"#,
        )
        .bind(id)
        .bind(user_id)
        .bind(label)
        .bind(encrypted_secret)
        .execute(&self.pool)
        .await
        .map_err(unavailable)?;
        Ok(())
    }

    async fn revoke(&self, id: Uuid) -> Result<bool, ProvisioningStoreError> {
        let result = sqlx::query(
            "UPDATE subsonic_app_passwords SET revoked_at = CURRENT_TIMESTAMP WHERE id = $1 AND revoked_at IS NULL",
        )
        .bind(id)
        .execute(&self.pool)
        .await
        .map_err(unavailable)?;

        if result.rows_affected() > 0 {
            return Ok(true);
        }

        let result = sqlx::query(
            "UPDATE api_credentials SET revoked_at = CURRENT_TIMESTAMP WHERE id = $1 AND revoked_at IS NULL",
        )
        .bind(id)
        .execute(&self.pool)
        .await
        .map_err(unavailable)?;
        Ok(result.rows_affected() > 0)
    }

    async fn set_role(
        &self,
        user_id: UserId,
        role: UserRole,
    ) -> Result<(), ProvisioningStoreError> {
        sqlx::query("UPDATE users SET role = $1 WHERE id = $2")
            .bind(role.as_str())
            .bind(user_id.as_uuid())
            .execute(&self.pool)
            .await
            .map_err(unavailable)?;
        Ok(())
    }
}

fn map_database_error(error: sqlx::Error) -> ProvisioningStoreError {
    if let sqlx::Error::Database(database_error) = &error
        && database_error.code().as_deref() == Some("23505")
    {
        return ProvisioningStoreError::UserAlreadyExists;
    }
    unavailable(error)
}

fn unavailable(error: sqlx::Error) -> ProvisioningStoreError {
    ProvisioningStoreError::Unavailable(Box::new(error))
}
