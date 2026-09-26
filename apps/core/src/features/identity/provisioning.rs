use std::sync::Arc;

use async_trait::async_trait;
use thiserror::Error;
use uuid::Uuid;

use super::domain::{UserId, UserRole};
use super::secrets::{CredentialKey, SecretError, encrypt_secret, generate_app_secret};

#[derive(Debug, Error)]
pub enum ProvisioningError {
    #[error("user was not found")]
    UserNotFound,

    #[error("a user with that username already exists")]
    UserAlreadyExists,

    #[error("credential storage failed")]
    Storage(#[source] ProvisioningStoreError),

    #[error("credential secret could not be encrypted")]
    Secret(#[from] SecretError),
}

#[derive(Debug, Error)]
pub enum ProvisioningStoreError {
    #[error("a user with that username already exists")]
    UserAlreadyExists,

    #[error("credential storage unavailable")]
    Unavailable(#[source] Box<dyn std::error::Error + Send + Sync>),
}

#[async_trait]
pub(crate) trait ProvisioningStore: Send + Sync {
    async fn insert_user(
        &self,
        id: Uuid,
        username: &str,
        display_name: &str,
    ) -> Result<(), ProvisioningStoreError>;
    async fn user_id(&self, username: &str) -> Result<Option<Uuid>, ProvisioningStoreError>;
    async fn insert_password(
        &self,
        id: Uuid,
        user_id: Uuid,
        label: &str,
        encrypted_secret: Vec<u8>,
    ) -> Result<(), ProvisioningStoreError>;
    async fn revoke(&self, id: Uuid) -> Result<bool, ProvisioningStoreError>;
    async fn set_role(&self, user_id: UserId, role: UserRole)
    -> Result<(), ProvisioningStoreError>;
}

pub struct ProvisioningService {
    store: Arc<dyn ProvisioningStore>,
}

impl ProvisioningService {
    pub(crate) fn new(store: Arc<dyn ProvisioningStore>) -> Self {
        Self { store }
    }

    pub async fn create_user(
        &self,
        username: &str,
        display_name: &str,
    ) -> Result<Uuid, ProvisioningError> {
        let id = Uuid::now_v7();
        self.store
            .insert_user(id, username, display_name)
            .await
            .map_err(map_storage_error)?;
        Ok(id)
    }

    pub async fn mint_app_password(
        &self,
        key: &CredentialKey,
        username: &str,
        label: &str,
    ) -> Result<(Uuid, String), ProvisioningError> {
        let user_id = self
            .store
            .user_id(username)
            .await
            .map_err(map_storage_error)?
            .ok_or(ProvisioningError::UserNotFound)?;
        let id = Uuid::now_v7();
        let secret = generate_app_secret();
        let encrypted_secret = encrypt_secret(key, secret.as_bytes(), id.as_bytes())?;
        self.store
            .insert_password(id, user_id, label, encrypted_secret)
            .await
            .map_err(map_storage_error)?;
        Ok((id, secret))
    }

    pub async fn revoke_credential(&self, id: Uuid) -> Result<bool, ProvisioningError> {
        self.store.revoke(id).await.map_err(map_storage_error)
    }

    pub async fn assign_admin(&self, username: &str) -> Result<UserRole, ProvisioningError> {
        let user_id = self
            .store
            .user_id(username)
            .await
            .map_err(map_storage_error)?
            .ok_or(ProvisioningError::UserNotFound)?;
        self.store
            .set_role(UserId::from_uuid(user_id), UserRole::Admin)
            .await
            .map_err(map_storage_error)?;
        Ok(UserRole::Admin)
    }
}

fn map_storage_error(error: ProvisioningStoreError) -> ProvisioningError {
    match error {
        ProvisioningStoreError::UserAlreadyExists => ProvisioningError::UserAlreadyExists,
        other => ProvisioningError::Storage(other),
    }
}
