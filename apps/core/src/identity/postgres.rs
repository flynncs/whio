use async_trait::async_trait;
use sqlx::PgPool;
use uuid::Uuid;

use super::domain::{ApiKeyCredential, AppPasswordCredential, User, UserId};
use super::repository::{CredentialRepository, CredentialStoreError};

pub struct PostgresCredentialRepository {
    pool: PgPool,
}

impl PostgresCredentialRepository {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl CredentialRepository for PostgresCredentialRepository {
    async fn find_user_by_username(
        &self,
        username: &str,
    ) -> Result<Option<User>, CredentialStoreError> {
        let row = sqlx::query_as::<_, (Uuid, String, String)>(
            r#"
            SELECT id, username, display_name
            FROM users
            WHERE lower(username) = lower($1)
            "#,
        )
        .bind(username)
        .fetch_optional(&self.pool)
        .await
        .map_err(|_| CredentialStoreError::Unavailable)?;

        Ok(row.map(|(id, username, display_name)| User {
            id: UserId::from_uuid(id),
            username,
            display_name,
        }))
    }

    async fn find_active_api_key_by_digest(
        &self,
        digest: [u8; 32],
    ) -> Result<Option<ApiKeyCredential>, CredentialStoreError> {
        let row = sqlx::query_as::<_, (Uuid, Uuid)>(
            r#"
            SELECT id, user_id
            FROM api_credentials
            WHERE lookup_digest = $1 AND revoked_at IS NULL
            "#,
        )
        .bind(digest.as_slice())
        .fetch_optional(&self.pool)
        .await
        .map_err(|_| CredentialStoreError::Unavailable)?;

        Ok(row.map(|(id, user_id)| ApiKeyCredential {
            id,
            user_id: UserId::from_uuid(user_id),
        }))
    }

    async fn list_active_app_passwords(
        &self,
        user_id: UserId,
    ) -> Result<Vec<AppPasswordCredential>, CredentialStoreError> {
        let rows = sqlx::query_as::<_, (Uuid, Uuid, Vec<u8>)>(
            r#"
            SELECT id, user_id, encrypted_secret
            FROM subsonic_app_passwords
            WHERE user_id = $1 AND revoked_at IS NULL
            ORDER BY created_at
            "#,
        )
        .bind(user_id.as_uuid())
        .fetch_all(&self.pool)
        .await
        .map_err(|_| CredentialStoreError::Unavailable)?;

        Ok(rows
            .into_iter()
            .map(|(id, user_id, encrypted_secret)| AppPasswordCredential {
                id,
                user_id: UserId::from_uuid(user_id),
                encrypted_secret,
            })
            .collect())
    }
}
