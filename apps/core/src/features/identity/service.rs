use std::sync::Arc;

use md5::{Digest, Md5};
use subtle::ConstantTimeEq;
use tracing::warn;

use crate::features::identity::domain::User;
use crate::features::identity::secrets::{decrypt_secret, lookup_digest};
use crate::platform::{clock::Clock, request::RequestContext};

use super::domain::{Principal, UserId, UserRole};
use super::secrets::CredentialKey;
use super::storage::repository::{CredentialRepository, CredentialStoreError};
use super::throttle::{LoginThrottle, LoginThrottleConfig};

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
    throttle: LoginThrottle,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirectoryUser {
    pub id: UserId,
    pub username: String,
    pub display_name: String,
    pub role: UserRole,
}

fn directory_user(user: User) -> DirectoryUser {
    DirectoryUser {
        id: user.id,
        username: user.username,
        display_name: user.display_name,
        role: user.role,
    }
}

impl CredentialService {
    pub fn new(
        repository: Arc<dyn CredentialRepository>,
        key: CredentialKey,
        throttle_config: LoginThrottleConfig,
    ) -> Self {
        Self::new_with_clock(repository, key, throttle_config, Clock::system())
    }

    pub(crate) fn new_with_clock(
        repository: Arc<dyn CredentialRepository>,
        key: CredentialKey,
        throttle_config: LoginThrottleConfig,
        clock: Clock,
    ) -> Self {
        Self {
            repository,
            key: Arc::new(key),
            throttle: LoginThrottle::new(throttle_config, clock),
        }
    }

    pub async fn authenticate(
        &self,
        presented: &PresentedAuth,
        context: &RequestContext,
    ) -> Result<Principal, AuthError> {
        let account = match presented {
            PresentedAuth::ApiKey { .. } => None,
            PresentedAuth::Token { username, .. } | PresentedAuth::Password { username, .. } => {
                Some(username.to_lowercase())
            }
        };
        self.throttled(account.as_deref(), context, async {
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
                    .authenticate_app_password(username, &|candidate_password| {
                        constant_time_eq(candidate_password.as_bytes(), password.as_bytes())
                    })
                    .await
                    .map(|(principal, _)| principal),
            }
        })
        .await
    }

    async fn authenticate_api_key(&self, secret: &str) -> Result<Principal, AuthError> {
        let digest = lookup_digest(secret);
        let credential = self
            .repository
            .find_active_api_key_by_digest(digest)
            .await?;

        match credential {
            Some(cred) => {
                let user = self
                    .repository
                    .find_user_by_id(cred.user_id)
                    .await?
                    .ok_or(AuthError::InvalidCredentials)?;
                Ok(Principal {
                    user_id: user.id,
                    role: user.role,
                })
            }
            None => Err(AuthError::InvalidCredentials),
        }
    }

    pub async fn authenticate_password(
        &self,
        username: &str,
        password: &str,
        context: &RequestContext,
    ) -> Result<(Principal, uuid::Uuid), AuthError> {
        self.throttled(Some(&username.to_lowercase()), context, async {
            self.authenticate_app_password(username, &|candidate_password| {
                constant_time_eq(candidate_password.as_bytes(), password.as_bytes())
            })
            .await
        })
        .await
    }

    async fn throttled<T>(
        &self,
        account: Option<&str>,
        context: &RequestContext,
        check: impl std::future::Future<Output = Result<T, AuthError>>,
    ) -> Result<T, AuthError> {
        let ip = context.client_ip();
        if !self.throttle.allowed(account, ip) {
            warn!(
                request_id = context.request_id().as_str(),
                "authentication attempt refused by throttle"
            );
            return Err(AuthError::InvalidCredentials);
        }
        match check.await {
            Err(AuthError::InvalidCredentials) => {
                self.throttle.record_failure(account, ip);
                Err(AuthError::InvalidCredentials)
            }
            result => result,
        }
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
                return Ok((
                    Principal {
                        user_id: user.id,
                        role: user.role,
                    },
                    credential.id,
                ));
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

    pub async fn directory(
        &self,
        viewer: &Principal,
    ) -> Result<Vec<DirectoryUser>, CredentialStoreError> {
        let users = self.repository.list_users().await?;
        if viewer.is_admin() {
            return Ok(users.into_iter().map(directory_user).collect());
        }
        Ok(users
            .into_iter()
            .filter(|user| user.id == viewer.user_id)
            .map(directory_user)
            .collect())
    }

    pub async fn visible_user(
        &self,
        viewer: &Principal,
        username: &str,
    ) -> Result<Option<DirectoryUser>, CredentialStoreError> {
        let user = self.repository.find_user_by_username(username).await?;
        Ok(user
            .filter(|user| user.id == viewer.user_id || viewer.is_admin())
            .map(directory_user))
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
