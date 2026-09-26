use async_trait::async_trait;
use sqlx::PgPool;
use uuid::Uuid;

use super::super::domain::{ApiKeyCredential, AppPasswordCredential, User, UserId, UserRole};
use super::repository::{CredentialRepository, CredentialStoreError};

fn parse_role(role: &str) -> Result<UserRole, CredentialStoreError> {
    UserRole::parse(role).ok_or(CredentialStoreError::InvalidData)
}

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
        let row = sqlx::query_as::<_, (Uuid, String, String, String)>(
            r#"
            SELECT id, username, display_name, role
            FROM users
            WHERE lower(username) = lower($1)
            "#,
        )
        .bind(username)
        .fetch_optional(&self.pool)
        .await
        .map_err(|_| CredentialStoreError::Unavailable)?;

        row.map(|(id, username, display_name, role)| {
            Ok(User {
                id: UserId::from_uuid(id),
                username,
                display_name,
                role: parse_role(&role)?,
            })
        })
        .transpose()
    }

    async fn list_users(&self) -> Result<Vec<User>, CredentialStoreError> {
        let rows = sqlx::query_as::<_, (Uuid, String, String, String)>(
            r#"
            SELECT id, username, display_name, role
            FROM users
            ORDER BY lower(username), id
            "#,
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|_| CredentialStoreError::Unavailable)?;

        rows.into_iter()
            .map(|(id, username, display_name, role)| {
                Ok(User {
                    id: UserId::from_uuid(id),
                    username,
                    display_name,
                    role: parse_role(&role)?,
                })
            })
            .collect()
    }

    async fn find_user_by_id(&self, id: UserId) -> Result<Option<User>, CredentialStoreError> {
        let row = sqlx::query_as::<_, (Uuid, String, String, String)>(
            r#"
            SELECT id, username, display_name, role
            FROM users
            WHERE id = $1
            "#,
        )
        .bind(id.as_uuid())
        .fetch_optional(&self.pool)
        .await
        .map_err(|_| CredentialStoreError::Unavailable)?;

        row.map(|(id, username, display_name, role)| {
            Ok(User {
                id: UserId::from_uuid(id),
                username,
                display_name,
                role: parse_role(&role)?,
            })
        })
        .transpose()
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
    async fn find_active_app_password(
        &self,
        credential_id: uuid::Uuid,
    ) -> Result<Option<AppPasswordCredential>, CredentialStoreError> {
        let row = sqlx::query_as::<_, (Uuid, Uuid, Vec<u8>)>(
            r#"
            SELECT id, user_id, encrypted_secret
            FROM subsonic_app_passwords
            WHERE id = $1 AND revoked_at IS NULL
            "#,
        )
        .bind(credential_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|_| CredentialStoreError::Unavailable)?;

        Ok(
            row.map(|(id, user_id, encrypted_secret)| AppPasswordCredential {
                id,
                user_id: UserId::from_uuid(user_id),
                encrypted_secret,
            }),
        )
    }
}
