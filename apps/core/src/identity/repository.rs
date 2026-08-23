use async_trait::async_trait;

use super::domain::{ApiKeyCredential, AppPasswordCredential, User, UserId};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CredentialStoreError {
    #[error("credential storage is unavailable")]
    Unavailable,

    #[error("credential storage returned invalid data")]
    InvalidData,
}

#[async_trait]
pub trait CredentialRepository: Send + Sync {
    async fn find_user_by_username(
        &self,
        username: &str,
    ) -> Result<Option<User>, CredentialStoreError>;

    async fn find_active_api_key_by_digest(
        &self,
        digest: [u8; 32],
    ) -> Result<Option<ApiKeyCredential>, CredentialStoreError>;

    async fn list_active_app_passwords(
        &self,
        user_id: UserId,
    ) -> Result<Vec<AppPasswordCredential>, CredentialStoreError>;
}
