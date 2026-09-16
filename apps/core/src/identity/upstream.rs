//! Upstream server registry and per-user foreign credentials.

use async_trait::async_trait;
use sqlx::PgPool;
use thiserror::Error;
use uuid::Uuid;

use super::secrets::{CredentialKey, SecretError, decrypt_secret, encrypt_secret};

#[derive(Debug, Error)]
pub enum UpstreamStoreError {
    #[error("user was not found")]
    UserNotFound,

    #[error("upstream server was not found")]
    ServerNotFound,

    #[error("that server is already registered")]
    ServerAlreadyExists,

    #[error("that user is already linked to that server")]
    AlreadyLinked,

    #[error("database operation failed")]
    Database(#[source] sqlx::Error),

    #[error("credential secret could not be encrypted")]
    Secret(#[from] SecretError),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpstreamServer {
    pub id: Uuid,
    pub base_url: String,
    pub admin_username: String,
    pub enabled: bool,
}

pub async fn add_server(
    pool: &PgPool,
    key: &CredentialKey,
    base_url: &str,
    admin_username: &str,
    admin_password: &str,
) -> Result<Uuid, UpstreamStoreError> {
    let id = Uuid::now_v7();
    let encrypted_admin_password = encrypt_secret(key, admin_password.as_bytes(), id.as_bytes())?;

    sqlx::query(
        r#"INSERT INTO upstream_servers
           (id, kind, base_url, admin_username, encrypted_admin_password)
           VALUES ($1, 'navidrome', $2, $3, $4)"#,
    )
    .bind(id)
    .bind(base_url)
    .bind(admin_username)
    .bind(encrypted_admin_password)
    .execute(pool)
    .await
    .map(|_| id)
    .map_err(|error| map_conflict(error, UpstreamStoreError::ServerAlreadyExists))
}

pub async fn list_servers(pool: &PgPool) -> Result<Vec<UpstreamServer>, UpstreamStoreError> {
    sqlx::query_as::<_, (Uuid, String, String, bool)>(
        "SELECT id, base_url, admin_username, enabled FROM upstream_servers ORDER BY base_url",
    )
    .fetch_all(pool)
    .await
    .map(|rows| {
        rows.into_iter()
            .map(|(id, base_url, admin_username, enabled)| UpstreamServer {
                id,
                base_url,
                admin_username,
                enabled,
            })
            .collect()
    })
    .map_err(UpstreamStoreError::Database)
}

async fn server_id(pool: &PgPool, base_url: &str) -> Result<Uuid, UpstreamStoreError> {
    sqlx::query_scalar::<_, Uuid>(
        "SELECT id FROM upstream_servers WHERE lower(base_url) = lower($1)",
    )
    .bind(base_url)
    .fetch_optional(pool)
    .await
    .map_err(UpstreamStoreError::Database)?
    .ok_or(UpstreamStoreError::ServerNotFound)
}

async fn user_id(pool: &PgPool, username: &str) -> Result<Uuid, UpstreamStoreError> {
    sqlx::query_scalar::<_, Uuid>("SELECT id FROM users WHERE lower(username) = lower($1)")
        .bind(username)
        .fetch_optional(pool)
        .await
        .map_err(UpstreamStoreError::Database)?
        .ok_or(UpstreamStoreError::UserNotFound)
}

pub async fn store_link(
    pool: &PgPool,
    key: &CredentialKey,
    username: &str,
    base_url: &str,
    upstream_username: &str,
    upstream_password: &str,
) -> Result<Uuid, UpstreamStoreError> {
    let user_id = user_id(pool, username).await?;
    let server_id = server_id(pool, base_url).await?;

    let id = Uuid::now_v7();
    let encrypted_password = encrypt_secret(key, upstream_password.as_bytes(), id.as_bytes())?;

    sqlx::query(
        r#"INSERT INTO upstream_links
           (id, user_id, server_id, upstream_username, encrypted_password)
           VALUES ($1, $2, $3, $4, $5)"#,
    )
    .bind(id)
    .bind(user_id)
    .bind(server_id)
    .bind(upstream_username)
    .bind(encrypted_password)
    .execute(pool)
    .await
    .map(|_| id)
    .map_err(|error| map_conflict(error, UpstreamStoreError::AlreadyLinked))
}

pub async fn update_link(
    pool: &PgPool,
    key: &CredentialKey,
    username: &str,
    base_url: &str,
    upstream_username: &str,
    upstream_password: &str,
) -> Result<bool, UpstreamStoreError> {
    let user_id = user_id(pool, username).await?;
    let server_id = server_id(pool, base_url).await?;

    let link_id: Option<Uuid> =
        sqlx::query_scalar("SELECT id FROM upstream_links WHERE user_id = $1 AND server_id = $2")
            .bind(user_id)
            .bind(server_id)
            .fetch_optional(pool)
            .await
            .map_err(UpstreamStoreError::Database)?;
    let Some(link_id) = link_id else {
        return Ok(false);
    };

    let encrypted_password = encrypt_secret(key, upstream_password.as_bytes(), link_id.as_bytes())?;
    sqlx::query(
        r#"UPDATE upstream_links
           SET upstream_username = $2, encrypted_password = $3, stale_at = NULL
           WHERE id = $1"#,
    )
    .bind(link_id)
    .bind(upstream_username)
    .bind(encrypted_password)
    .execute(pool)
    .await
    .map(|result| result.rows_affected() > 0)
    .map_err(UpstreamStoreError::Database)
}

pub async fn remove_link(
    pool: &PgPool,
    username: &str,
    base_url: &str,
) -> Result<bool, UpstreamStoreError> {
    let user_id = user_id(pool, username).await?;
    let server_id = server_id(pool, base_url).await?;

    sqlx::query("DELETE FROM upstream_links WHERE user_id = $1 AND server_id = $2")
        .bind(user_id)
        .bind(server_id)
        .execute(pool)
        .await
        .map(|result| result.rows_affected() > 0)
        .map_err(UpstreamStoreError::Database)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpstreamLink {
    pub link_id: Uuid,
    pub server_id: Uuid,
    pub server_scope: String,
    pub base_url: String,
    pub username: String,
    pub encrypted_password: Vec<u8>,
}

pub async fn links_for_user(
    pool: &PgPool,
    user_id: &Uuid,
) -> Result<Vec<UpstreamLink>, UpstreamStoreError> {
    sqlx::query_as::<_, (Uuid, Uuid, String, String, Vec<u8>)>(
        r#"SELECT links.id, links.server_id, servers.base_url, links.upstream_username,
                  links.encrypted_password
           FROM upstream_links AS links
           JOIN upstream_servers AS servers ON servers.id = links.server_id
           WHERE links.user_id = $1
             AND links.stale_at IS NULL
             AND servers.enabled
           ORDER BY servers.base_url"#,
    )
    .bind(user_id)
    .fetch_all(pool)
    .await
    .map(|rows| {
        rows.into_iter()
            .map(
                |(link_id, server_id, base_url, username, encrypted_password)| UpstreamLink {
                    link_id,
                    server_scope: server_id.to_string(),
                    server_id,
                    base_url,
                    username,
                    encrypted_password,
                },
            )
            .collect()
    })
    .map_err(UpstreamStoreError::Database)
}

pub async fn link_for_server(
    pool: &PgPool,
    user_id: &Uuid,
    server_id: &Uuid,
) -> Result<Option<UpstreamLink>, UpstreamStoreError> {
    sqlx::query_as::<_, (Uuid, Uuid, String, String, Vec<u8>)>(
        r#"SELECT links.id, links.server_id, servers.base_url, links.upstream_username,
                  links.encrypted_password
           FROM upstream_links AS links
           JOIN upstream_servers AS servers ON servers.id = links.server_id
           WHERE links.user_id = $1
             AND links.server_id = $2
             AND links.stale_at IS NULL
             AND servers.enabled"#,
    )
    .bind(user_id)
    .bind(server_id)
    .fetch_optional(pool)
    .await
    .map(|row| {
        row.map(
            |(link_id, server_id, base_url, username, encrypted_password)| UpstreamLink {
                link_id,
                server_scope: server_id.to_string(),
                server_id,
                base_url,
                username,
                encrypted_password,
            },
        )
    })
    .map_err(UpstreamStoreError::Database)
}

pub async fn mark_stale(
    pool: &PgPool,
    user_id: &Uuid,
    server_id: &Uuid,
) -> Result<(), UpstreamStoreError> {
    sqlx::query(
        r#"UPDATE upstream_links
           SET stale_at = CURRENT_TIMESTAMP
           WHERE user_id = $1 AND server_id = $2 AND stale_at IS NULL"#,
    )
    .bind(user_id)
    .bind(server_id)
    .execute(pool)
    .await
    .map(|_| ())
    .map_err(UpstreamStoreError::Database)
}

pub fn decrypt_password(
    key: &CredentialKey,
    link_id: Uuid,
    blob: &[u8],
) -> Result<String, UpstreamStoreError> {
    let plaintext = decrypt_secret(key, blob, link_id.as_bytes())?;
    String::from_utf8(plaintext)
        .map_err(|_| UpstreamStoreError::Secret(SecretError::DecryptionFailed))
}

fn map_conflict(error: sqlx::Error, conflict: UpstreamStoreError) -> UpstreamStoreError {
    if let sqlx::Error::Database(database_error) = &error
        && database_error.code().as_deref() == Some("23505")
    {
        return conflict;
    }

    UpstreamStoreError::Database(error)
}

#[async_trait]
pub trait UpstreamLinkStore: Send + Sync {
    async fn links_for_user(&self, user_id: &Uuid)
    -> Result<Vec<UpstreamLink>, UpstreamStoreError>;
    async fn link_for_server(
        &self,
        user_id: &Uuid,
        server_id: &Uuid,
    ) -> Result<Option<UpstreamLink>, UpstreamStoreError>;
    async fn mark_stale(&self, user_id: &Uuid, server_id: &Uuid) -> Result<(), UpstreamStoreError>;
}

pub struct PgUpstreamLinkStore {
    pool: PgPool,
}

impl PgUpstreamLinkStore {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl UpstreamLinkStore for PgUpstreamLinkStore {
    async fn links_for_user(
        &self,
        user_id: &Uuid,
    ) -> Result<Vec<UpstreamLink>, UpstreamStoreError> {
        links_for_user(&self.pool, user_id).await
    }

    async fn link_for_server(
        &self,
        user_id: &Uuid,
        server_id: &Uuid,
    ) -> Result<Option<UpstreamLink>, UpstreamStoreError> {
        link_for_server(&self.pool, user_id, server_id).await
    }

    async fn mark_stale(&self, user_id: &Uuid, server_id: &Uuid) -> Result<(), UpstreamStoreError> {
        mark_stale(&self.pool, user_id, server_id).await
    }
}

#[derive(Debug, Default)]
pub struct MemoryUpstreamLinkStore {
    links: parking_lot::Mutex<Vec<MemoryLink>>,
}

#[derive(Debug)]
struct MemoryLink {
    link_id: Uuid,
    server_id: Uuid,
    user_id: Uuid,
    base_url: String,
    username: String,
    encrypted_password: Vec<u8>,
    stale: bool,
}

impl MemoryUpstreamLinkStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert_link(
        &self,
        key: &CredentialKey,
        user_id: Uuid,
        server_id: Uuid,
        base_url: &str,
        username: &str,
        password: &str,
    ) -> Uuid {
        let link_id = Uuid::now_v7();
        let encrypted_password = encrypt_secret(key, password.as_bytes(), link_id.as_bytes())
            .expect("in-memory link encryption");
        self.links.lock().push(MemoryLink {
            link_id,
            server_id,
            user_id,
            base_url: base_url.to_owned(),
            username: username.to_owned(),
            encrypted_password,
            stale: false,
        });
        link_id
    }

    pub fn is_stale(&self, user_id: &Uuid, server_id: &Uuid) -> bool {
        self.links
            .lock()
            .iter()
            .any(|link| &link.user_id == user_id && &link.server_id == server_id && link.stale)
    }
}

#[async_trait]
impl UpstreamLinkStore for MemoryUpstreamLinkStore {
    async fn link_for_server(
        &self,
        user_id: &Uuid,
        server_id: &Uuid,
    ) -> Result<Option<UpstreamLink>, UpstreamStoreError> {
        Ok(self
            .links
            .lock()
            .iter()
            .find(|link| &link.user_id == user_id && &link.server_id == server_id && !link.stale)
            .map(|link| UpstreamLink {
                link_id: link.link_id,
                server_id: link.server_id,
                server_scope: link.server_id.to_string(),
                base_url: link.base_url.clone(),
                username: link.username.clone(),
                encrypted_password: link.encrypted_password.clone(),
            }))
    }

    async fn links_for_user(
        &self,
        user_id: &Uuid,
    ) -> Result<Vec<UpstreamLink>, UpstreamStoreError> {
        Ok(self
            .links
            .lock()
            .iter()
            .filter(|link| &link.user_id == user_id && !link.stale)
            .map(|link| UpstreamLink {
                link_id: link.link_id,
                server_id: link.server_id,
                server_scope: link.server_id.to_string(),
                base_url: link.base_url.clone(),
                username: link.username.clone(),
                encrypted_password: link.encrypted_password.clone(),
            })
            .collect())
    }

    async fn mark_stale(&self, user_id: &Uuid, server_id: &Uuid) -> Result<(), UpstreamStoreError> {
        for link in self
            .links
            .lock()
            .iter_mut()
            .filter(|link| &link.user_id == user_id && &link.server_id == server_id)
        {
            link.stale = true;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key() -> CredentialKey {
        CredentialKey::from_base64("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA")
            .expect("valid key")
    }

    #[test]
    fn vaulted_password_round_trips() {
        let link_id = Uuid::now_v7();
        let blob = encrypt_secret(&key(), b"foreign-password", link_id.as_bytes()).unwrap();

        assert_eq!(
            decrypt_password(&key(), link_id, &blob).unwrap(),
            "foreign-password"
        );
    }
}
