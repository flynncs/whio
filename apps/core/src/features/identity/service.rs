use std::sync::Arc;

use md5::{Digest, Md5};
use subtle::ConstantTimeEq;
use tracing::warn;

use crate::features::identity::domain::User;
use crate::features::identity::secrets::{decrypt_secret, lookup_digest};

use super::domain::Principal;
use super::secrets::CredentialKey;
use super::storage::repository::{CredentialRepository, CredentialStoreError};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PresentedAuth {
    Password {
        username: String,
        password: String,
    },
    Token {
        username: String,
        token: String,
        salt: String,
    },
    ApiKey {
        secret: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AuthError {
    #[error("invalid credentials")]
    InvalidCredentials,

    #[error("credential storage failed")]
    Storage(#[from] CredentialStoreError),
}

pub struct CredentialService {
    repository: Arc<dyn CredentialRepository>,
    key: Arc<CredentialKey>,
}

impl CredentialService {
    pub fn new(repository: Arc<dyn CredentialRepository>, key: CredentialKey) -> Self {
        Self {
            repository,
            key: Arc::new(key),
        }
    }

    pub async fn authenticate(&self, presented: &PresentedAuth) -> Result<Principal, AuthError> {
        match presented {
            PresentedAuth::ApiKey { secret } => self.authenticate_api_key(secret).await,
            PresentedAuth::Token {
                username,
                token,
                salt,
            } => self
                .authenticate_app_password(username, &|candidate_password| {
                    token_matches(candidate_password, salt, token)
                })
                .await
                .map(|(principal, _)| principal),
            PresentedAuth::Password { username, password } => self
                .authenticate_password(username, password)
                .await
                .map(|(principal, _)| principal),
        }
    }

    async fn authenticate_api_key(&self, secret: &str) -> Result<Principal, AuthError> {
        let digest = lookup_digest(secret);
        let credential = self
            .repository
            .find_active_api_key_by_digest(digest)
            .await?;

        match credential {
            Some(cred) => Ok(Principal {
                user_id: cred.user_id,
            }),
            None => Err(AuthError::InvalidCredentials),
        }
    }

    pub async fn authenticate_password(
        &self,
        username: &str,
        password: &str,
    ) -> Result<(Principal, uuid::Uuid), AuthError> {
        let expected = password.as_bytes().to_vec();
        self.authenticate_app_password(username, &|candidate_password| {
            constant_time_eq(candidate_password.as_bytes(), &expected)
        })
        .await
    }

    pub async fn app_password_active(&self, credential_id: uuid::Uuid) -> Result<bool, AuthError> {
        self.repository
            .find_active_app_password(credential_id)
            .await
            .map(|credential| credential.is_some())
            .map_err(AuthError::Storage)
    }

    async fn authenticate_app_password(
        &self,
        username: &str,
        matches: &(dyn Fn(&str) -> bool + Send + Sync),
    ) -> Result<(Principal, uuid::Uuid), AuthError> {
        let user = self.repository.find_user_by_username(username).await?;
        let Some(user) = user else {
            return Err(AuthError::InvalidCredentials);
        };

        for credential in self.repository.list_active_app_passwords(user.id).await? {
            let recovered = match decrypt_secret(
                &self.key,
                &credential.encrypted_secret,
                credential.id.as_bytes(),
            ) {
                Ok(bytes) => bytes,
                Err(_) => {
                    warn!(credential = %credential.id, "undecryptable stored app password");
                    continue;
                }
            };

            let Some(candidate) = String::from_utf8(recovered).ok() else {
                warn!(credential = %credential.id, "stored app password is not valid utf8");
                continue;
            };

            if matches(&candidate) {
                return Ok((Principal { user_id: user.id }, credential.id));
            }
        }

        Err(AuthError::InvalidCredentials)
    }

    pub(crate) fn key(&self) -> &CredentialKey {
        &self.key
    }

    pub(crate) async fn find_user(&self, username: &str) -> Result<Option<User>, AuthError> {
        self.repository
            .find_user_by_username(username)
            .await
            .map_err(AuthError::Storage)
    }

    pub(crate) async fn visible_user(
        &self,
        viewer: &Principal,
        username: &str,
    ) -> Result<Option<User>, CredentialStoreError> {
        let user = self.repository.find_user_by_username(username).await?;
        Ok(user.filter(|user| user.id == viewer.user_id))
    }
}

fn token_matches(password: &str, salt: &str, token_hex: &str) -> bool {
    let Ok(expected) = hex::decode(token_hex) else {
        return false;
    };

    let mut hasher = Md5::new();
    hasher.update(password.as_bytes());
    hasher.update(salt.as_bytes());

    constant_time_eq(hasher.finalize().as_slice(), &expected)
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.ct_eq(b).into()
}

#[cfg(test)]
#[path = "service_tests.rs"]
mod tests;
