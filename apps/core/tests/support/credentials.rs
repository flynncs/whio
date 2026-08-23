// fixtures are a toolbox; each suite uses a subset
#![allow(dead_code)]

use std::sync::Arc;

use async_trait::async_trait;
use base64::Engine;
use base64::engine::general_purpose::STANDARD_NO_PAD;
use whio_core::identity::{
    domain::{User, UserId},
    repository::{CredentialRepository, CredentialStoreError},
    secrets::CredentialKey,
    service::CredentialService,
};

// http tests do not exercise authentication yet; nothing resolves
struct NoCredentialStore;

#[async_trait]
impl CredentialRepository for NoCredentialStore {
    async fn find_user_by_username(
        &self,
        _username: &str,
    ) -> Result<Option<User>, CredentialStoreError> {
        Ok(None)
    }

    async fn find_active_api_key_by_digest(
        &self,
        _digest: [u8; 32],
    ) -> Result<Option<whio_core::identity::domain::ApiKeyCredential>, CredentialStoreError> {
        Ok(None)
    }

    async fn list_active_app_passwords(
        &self,
        _user_id: UserId,
    ) -> Result<Vec<whio_core::identity::domain::AppPasswordCredential>, CredentialStoreError> {
        Ok(Vec::new())
    }
}

pub fn credential_service() -> Arc<CredentialService> {
    let key =
        CredentialKey::from_base64(&STANDARD_NO_PAD.encode([11u8; 32])).expect("valid test key");
    Arc::new(CredentialService::new(Arc::new(NoCredentialStore), key))
}
