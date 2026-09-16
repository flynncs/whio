// fixtures are a toolbox; each suite uses a subset
#![allow(dead_code)]

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use async_trait::async_trait;
use base64::Engine;
use base64::engine::general_purpose::STANDARD_NO_PAD;
use uuid::Uuid;
use whio_core::identity::{
    domain::{ApiKeyCredential, AppPasswordCredential, User, UserId},
    repository::{CredentialRepository, CredentialStoreError},
    secrets::{CredentialKey, encrypt_secret, generate_app_secret, lookup_digest},
    service::CredentialService,
};

pub const TEST_USERNAME: &str = "test-user";
pub const TEST_PASSWORD: &str = "whio-test-password";
pub const NATIVE_AUTH_QUERY: &str = "?u=test-user&v=1.16.1&c=integration-test&p=whio-test-password";

pub fn credential_service() -> Arc<CredentialService> {
    let key =
        CredentialKey::from_base64(&STANDARD_NO_PAD.encode([11u8; 32])).expect("valid test key");
    let store = Arc::new(MemoryCredentialStore::new());
    let user_id = UserId::from_uuid(Uuid::now_v7());
    store.insert_user(User {
        id: user_id,
        username: TEST_USERNAME.to_owned(),
        display_name: "Integration Test".to_owned(),
    });
    store.insert_app_password(&key, TEST_PASSWORD, user_id);
    Arc::new(CredentialService::new(store, key))
}

pub fn credential_service_with_storage_failure() -> Arc<CredentialService> {
    let key =
        CredentialKey::from_base64(&STANDARD_NO_PAD.encode([11u8; 32])).expect("valid test key");
    Arc::new(CredentialService::new(
        Arc::new(UnavailableCredentialStore),
        key,
    ))
}

struct UnavailableCredentialStore;

#[async_trait]
impl CredentialRepository for UnavailableCredentialStore {
    async fn find_user_by_username(
        &self,
        _username: &str,
    ) -> Result<Option<User>, CredentialStoreError> {
        Err(CredentialStoreError::Unavailable)
    }

    async fn find_active_api_key_by_digest(
        &self,
        _digest: [u8; 32],
    ) -> Result<Option<ApiKeyCredential>, CredentialStoreError> {
        Err(CredentialStoreError::Unavailable)
    }

    async fn list_active_app_passwords(
        &self,
        _user_id: UserId,
    ) -> Result<Vec<AppPasswordCredential>, CredentialStoreError> {
        Err(CredentialStoreError::Unavailable)
    }

    async fn find_active_app_password(
        &self,
        _credential_id: Uuid,
    ) -> Result<Option<AppPasswordCredential>, CredentialStoreError> {
        Err(CredentialStoreError::Unavailable)
    }
}

pub struct MemoryCredentialStore {
    users: Mutex<HashMap<String, User>>,
    api_keys: Mutex<Vec<([u8; 32], ApiKeyCredential)>>,
    app_passwords: Mutex<Vec<AppPasswordCredential>>,
}

impl MemoryCredentialStore {
    pub fn new() -> Self {
        Self {
            users: Mutex::new(HashMap::new()),
            api_keys: Mutex::new(Vec::new()),
            app_passwords: Mutex::new(Vec::new()),
        }
    }

    pub fn insert_user(&self, user: User) {
        self.users
            .lock()
            .expect("users lock")
            .insert(user.username.to_lowercase(), user);
    }

    pub fn insert_api_key(&self, credential: ApiKeyCredential, secret: &str) {
        let digest = lookup_digest(secret);
        self.api_keys
            .lock()
            .expect("api keys lock")
            .push((digest, credential));
    }

    pub fn insert_app_password(&self, key: &CredentialKey, secret: &str, user_id: UserId) {
        let credential_id = Uuid::now_v7();
        let encrypted_secret =
            encrypt_secret(key, secret.as_bytes(), credential_id.as_bytes()).expect("valid inputs");
        self.app_passwords
            .lock()
            .expect("app passwords lock")
            .push(AppPasswordCredential {
                id: credential_id,
                user_id,
                encrypted_secret,
            });
    }
}

impl Default for MemoryCredentialStore {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl CredentialRepository for MemoryCredentialStore {
    async fn find_user_by_username(
        &self,
        username: &str,
    ) -> Result<Option<User>, CredentialStoreError> {
        Ok(self
            .users
            .lock()
            .expect("users lock")
            .get(&username.to_lowercase())
            .cloned())
    }

    async fn find_active_api_key_by_digest(
        &self,
        digest: [u8; 32],
    ) -> Result<Option<ApiKeyCredential>, CredentialStoreError> {
        Ok(self
            .api_keys
            .lock()
            .expect("api keys lock")
            .iter()
            .find(|(stored, _)| *stored == digest)
            .map(|(_, credential)| credential.clone()))
    }

    async fn list_active_app_passwords(
        &self,
        user_id: UserId,
    ) -> Result<Vec<AppPasswordCredential>, CredentialStoreError> {
        Ok(self
            .app_passwords
            .lock()
            .expect("app passwords lock")
            .iter()
            .filter(|credential| credential.user_id == user_id)
            .cloned()
            .collect())
    }

    async fn find_active_app_password(
        &self,
        credential_id: Uuid,
    ) -> Result<Option<AppPasswordCredential>, CredentialStoreError> {
        Ok(self
            .app_passwords
            .lock()
            .expect("app passwords lock")
            .iter()
            .find(|credential| credential.id == credential_id)
            .cloned())
    }
}

fn test_credential_key() -> CredentialKey {
    CredentialKey::from_base64(&STANDARD_NO_PAD.encode([11u8; 32])).expect("valid test key")
}

// a service whose store knows `flynn` plus one api key; returns the secret
pub fn credential_service_with_known_api_key() -> (Arc<CredentialService>, String) {
    let key = test_credential_key();
    let store = Arc::new(MemoryCredentialStore::new());
    let user_id = UserId::from_uuid(Uuid::now_v7());
    store.insert_user(User {
        id: user_id,
        username: "flynn".to_owned(),
        display_name: "Flynn".to_owned(),
    });

    let secret = generate_app_secret();
    store.insert_api_key(
        ApiKeyCredential {
            id: Uuid::now_v7(),
            user_id,
        },
        &secret,
    );

    (Arc::new(CredentialService::new(store, key)), secret)
}

// a service whose store knows `flynn` plus one recoverable app password
pub fn credential_service_with_known_app_password() -> (Arc<CredentialService>, String) {
    let key = test_credential_key();
    let store = Arc::new(MemoryCredentialStore::new());
    let user_id = UserId::from_uuid(Uuid::now_v7());
    store.insert_user(User {
        id: user_id,
        username: "flynn".to_owned(),
        display_name: "Flynn".to_owned(),
    });

    let secret = generate_app_secret();
    store.insert_app_password(&key, &secret, user_id);

    (Arc::new(CredentialService::new(store, key)), secret)
}
