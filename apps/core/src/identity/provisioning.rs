use sqlx::PgPool;
use thiserror::Error;
use uuid::Uuid;

use super::secrets::{CredentialKey, SecretError, encrypt_secret, generate_app_secret};

#[derive(Debug, Error)]
pub enum ProvisioningError {
    #[error("user was not found")]
    UserNotFound,

    #[error("a user with that username already exists")]
    UserAlreadyExists,

    #[error("database operation failed")]
    Database(#[source] sqlx::Error),

    #[error("credential secret could not be encrypted")]
    Secret(#[from] SecretError),
}

pub async fn create_user(
    pool: &PgPool,
    username: &str,
    display_name: &str,
) -> Result<Uuid, ProvisioningError> {
    let id = Uuid::now_v7();
    sqlx::query("INSERT INTO users (id, username, display_name) VALUES ($1, $2, $3)")
        .bind(id)
        .bind(username)
        .bind(display_name)
        .execute(pool)
        .await
        .map(|_| id)
        .map_err(map_database_error)
}

pub async fn mint_app_password(
    pool: &PgPool,
    key: &CredentialKey,
    username: &str,
    label: &str,
) -> Result<(Uuid, String), ProvisioningError> {
    let user_id =
        sqlx::query_scalar::<_, Uuid>("SELECT id FROM users WHERE lower(username) = lower($1)")
            .bind(username)
            .fetch_optional(pool)
            .await
            .map_err(ProvisioningError::Database)?
            .ok_or(ProvisioningError::UserNotFound)?;

    let id = Uuid::now_v7();
    let secret = generate_app_secret();
    let encrypted_secret = encrypt_secret(key, secret.as_bytes(), id.as_bytes())?;

    sqlx::query(
        r#"INSERT INTO subsonic_app_passwords
           (id, user_id, label, encrypted_secret)
           VALUES ($1, $2, $3, $4)"#,
    )
    .bind(id)
    .bind(user_id)
    .bind(label)
    .bind(encrypted_secret)
    .execute(pool)
    .await
    .map_err(ProvisioningError::Database)?;

    Ok((id, secret))
}

pub async fn revoke_credential(pool: &PgPool, id: Uuid) -> Result<bool, ProvisioningError> {
    let result = sqlx::query(
        "UPDATE subsonic_app_passwords SET revoked_at = CURRENT_TIMESTAMP WHERE id = $1 AND revoked_at IS NULL",
    )
    .bind(id)
    .execute(pool)
    .await
    .map_err(ProvisioningError::Database)?;

    if result.rows_affected() > 0 {
        return Ok(true);
    }

    let result = sqlx::query(
        "UPDATE api_credentials SET revoked_at = CURRENT_TIMESTAMP WHERE id = $1 AND revoked_at IS NULL",
    )
    .bind(id)
    .execute(pool)
    .await
    .map_err(ProvisioningError::Database)?;

    Ok(result.rows_affected() > 0)
}

fn map_database_error(error: sqlx::Error) -> ProvisioningError {
    if let sqlx::Error::Database(database_error) = &error {
        if database_error.code().as_deref() == Some("23505") {
            return ProvisioningError::UserAlreadyExists;
        }
    }

    ProvisioningError::Database(error)
}
