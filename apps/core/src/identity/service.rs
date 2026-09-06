use std::sync::Arc;

use md5::{Digest, Md5};
use subtle::ConstantTimeEq;
use tracing::warn;

use crate::identity::domain::User;
use crate::identity::secrets::{decrypt_secret, lookup_digest};

use super::domain::Principal;
use super::repository::{CredentialRepository, CredentialStoreError};
use super::secrets::CredentialKey;

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
            } => {
                self.authenticate_app_password(username, &|candidate_password| {
                    token_matches(candidate_password, salt, token)
                })
                .await
            }
            PresentedAuth::Password { username, password } => {
                let expected = password.as_bytes().to_vec();
                self.authenticate_app_password(username, &|candidate_password| {
                    constant_time_eq(candidate_password.as_bytes(), &expected)
                })
                .await
            }
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

    async fn authenticate_app_password(
        &self,
        username: &str,
        matches: &(dyn Fn(&str) -> bool + Send + Sync),
    ) -> Result<Principal, AuthError> {
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
                return Ok(Principal { user_id: user.id });
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
mod tests {
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    use async_trait::async_trait;
    use base64::Engine;
    use base64::engine::general_purpose::{STANDARD_NO_PAD, URL_SAFE_NO_PAD};
    use md5::{Digest, Md5};
    use uuid::Uuid;

    use super::*;
    use crate::identity::domain::{ApiKeyCredential, AppPasswordCredential, User, UserId};
    use crate::identity::secrets::{encrypt_secret, generate_app_secret, lookup_digest};

    // test-local fixture standing in for postgres; lives and dies with this
    // module instead of shipping as product surface
    struct StubRepository {
        users: Mutex<HashMap<String, User>>,
        api_keys: Mutex<Vec<([u8; 32], ApiKeyCredential)>>,
        app_passwords: Mutex<Vec<AppPasswordCredential>>,
    }

    impl StubRepository {
        fn insert_user(&self, user: User) {
            self.users
                .lock()
                .expect("users lock")
                .insert(user.username.to_lowercase(), user);
        }

        fn insert_api_key(&self, credential: ApiKeyCredential, secret: &str) {
            let digest = lookup_digest(secret);
            self.api_keys
                .lock()
                .expect("api keys lock")
                .push((digest, credential));
        }

        fn insert_app_password(
            &self,
            key: &CredentialKey,
            credential_id: Uuid,
            user_id: UserId,
            secret: &str,
        ) {
            let encrypted_secret = encrypt_secret(key, secret.as_bytes(), credential_id.as_bytes())
                .expect("valid inputs");
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

    #[async_trait]
    impl CredentialRepository for StubRepository {
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
    }

    struct Fixture {
        service: CredentialService,
        repository: Arc<StubRepository>,
        key: CredentialKey,
        user_id: UserId,
    }

    fn fixture() -> Fixture {
        let key =
            CredentialKey::from_base64(&STANDARD_NO_PAD.encode([11u8; 32])).expect("valid key");
        let repository = Arc::new(StubRepository {
            users: Mutex::new(HashMap::new()),
            api_keys: Mutex::new(Vec::new()),
            app_passwords: Mutex::new(Vec::new()),
        });
        let user_id = UserId::from_uuid(Uuid::now_v7());
        repository.insert_user(User {
            id: user_id,
            username: "flynn".to_string(),
            display_name: "Flynn".to_string(),
        });

        let service = CredentialService::new(repository.clone(), key.clone());
        Fixture {
            service,
            repository,
            key,
            user_id,
        }
    }

    fn mint(fixture: &Fixture) -> String {
        let secret = generate_app_secret();
        fixture.repository.insert_app_password(
            &fixture.key,
            Uuid::now_v7(),
            fixture.user_id,
            &secret,
        );
        secret
    }

    fn subsonic_token(password: &str, salt: &str) -> String {
        let mut hasher = Md5::new();
        hasher.update(password.as_bytes());
        hasher.update(salt.as_bytes());
        hasher
            .finalize()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    }

    #[tokio::test]
    async fn api_key_authenticates_to_principal() {
        let fixture = fixture();
        let secret = generate_app_secret();
        fixture.repository.insert_api_key(
            ApiKeyCredential {
                id: Uuid::now_v7(),
                user_id: fixture.user_id,
            },
            &secret,
        );

        let result = fixture
            .service
            .authenticate(&PresentedAuth::ApiKey {
                secret: secret.clone(),
            })
            .await;

        assert_eq!(
            result,
            Ok(Principal {
                user_id: fixture.user_id
            })
        );
    }

    #[tokio::test]
    async fn api_key_with_unknown_secret_is_rejected() {
        let fixture = fixture();

        let result = fixture
            .service
            .authenticate(&PresentedAuth::ApiKey {
                secret: format!("whio_{}", URL_SAFE_NO_PAD.encode([9u8; 32])),
            })
            .await;

        assert_eq!(result, Err(AuthError::InvalidCredentials));
    }

    #[tokio::test]
    async fn token_authenticates_with_minted_password() {
        let fixture = fixture();
        let secret = mint(&fixture);
        let salt = "abcdef123456";

        let result = fixture
            .service
            .authenticate(&PresentedAuth::Token {
                username: "flynn".to_string(),
                token: subsonic_token(&secret, salt),
                salt: salt.to_string(),
            })
            .await;

        assert_eq!(
            result,
            Ok(Principal {
                user_id: fixture.user_id
            })
        );
    }

    #[tokio::test]
    async fn token_with_wrong_salt_is_rejected() {
        let fixture = fixture();
        let secret = mint(&fixture);

        let result = fixture
            .service
            .authenticate(&PresentedAuth::Token {
                username: "flynn".to_string(),
                token: subsonic_token(&secret, "other-salt"),
                salt: "real-salt".to_string(),
            })
            .await;

        assert_eq!(result, Err(AuthError::InvalidCredentials));
    }

    #[tokio::test]
    async fn unknown_user_and_wrong_password_are_indistinguishable() {
        let fixture = fixture();
        mint(&fixture);

        let unknown_user = fixture
            .service
            .authenticate(&PresentedAuth::Token {
                username: "nobody".to_string(),
                token: subsonic_token("whio_x", "salt"),
                salt: "salt".to_string(),
            })
            .await
            .unwrap_err();

        let known_user_bad_secret = fixture
            .service
            .authenticate(&PresentedAuth::Token {
                username: "flynn".to_string(),
                token: subsonic_token("whio_not_the_secret", "salt"),
                salt: "salt".to_string(),
            })
            .await
            .unwrap_err();

        assert_eq!(unknown_user, known_user_bad_secret);
    }

    #[tokio::test]
    async fn plaintext_password_authenticates() {
        let fixture = fixture();
        let secret = mint(&fixture);

        let result = fixture
            .service
            .authenticate(&PresentedAuth::Password {
                username: "flynn".to_string(),
                password: secret,
            })
            .await;

        assert_eq!(
            result,
            Ok(Principal {
                user_id: fixture.user_id
            })
        );
    }

    #[tokio::test]
    async fn wrong_plaintext_password_is_rejected() {
        let fixture = fixture();
        mint(&fixture);

        let result = fixture
            .service
            .authenticate(&PresentedAuth::Password {
                username: "flynn".to_string(),
                password: "whio_definitely_not_it".to_string(),
            })
            .await;

        assert_eq!(result, Err(AuthError::InvalidCredentials));
    }
}
