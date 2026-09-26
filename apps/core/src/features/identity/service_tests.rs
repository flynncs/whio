use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use base64::Engine;
use base64::engine::general_purpose::{STANDARD_NO_PAD, URL_SAFE_NO_PAD};
use md5::{Digest, Md5};
use uuid::Uuid;

use super::*;
use crate::features::identity::domain::{
    ApiKeyCredential, AppPasswordCredential, User, UserId, UserRole,
};
use crate::features::identity::secrets::{encrypt_secret, generate_app_secret, lookup_digest};

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

    async fn list_users(&self) -> Result<Vec<User>, CredentialStoreError> {
        let mut users: Vec<User> = self
            .users
            .lock()
            .expect("users lock")
            .values()
            .cloned()
            .collect();
        users.sort_by(|a, b| {
            a.username
                .to_lowercase()
                .cmp(&b.username.to_lowercase())
                .then_with(|| a.id.as_uuid().cmp(&b.id.as_uuid()))
        });
        Ok(users)
    }

    async fn find_user_by_id(&self, id: UserId) -> Result<Option<User>, CredentialStoreError> {
        Ok(self
            .users
            .lock()
            .expect("users lock")
            .values()
            .find(|user| user.id == id)
            .cloned())
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

struct Fixture {
    service: CredentialService,
    repository: Arc<StubRepository>,
    key: CredentialKey,
    user_id: UserId,
}

fn fixture() -> Fixture {
    let key = CredentialKey::from_base64(&STANDARD_NO_PAD.encode([11u8; 32])).expect("valid key");
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
        role: UserRole::Member,
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
    fixture
        .repository
        .insert_app_password(&fixture.key, Uuid::now_v7(), fixture.user_id, &secret);
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
            user_id: fixture.user_id,
            role: UserRole::Member
        })
    );
}

#[tokio::test]
async fn api_key_principal_carries_the_stored_role() {
    let fixture = fixture();
    let admin_id = UserId::from_uuid(Uuid::now_v7());
    fixture.repository.insert_user(User {
        id: admin_id,
        username: "root".to_string(),
        display_name: "Root".to_string(),
        role: UserRole::Admin,
    });
    let secret = generate_app_secret();
    fixture.repository.insert_api_key(
        ApiKeyCredential {
            id: Uuid::now_v7(),
            user_id: admin_id,
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
            user_id: admin_id,
            role: UserRole::Admin
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
            user_id: fixture.user_id,
            role: UserRole::Member
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
            user_id: fixture.user_id,
            role: UserRole::Member
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
#[tokio::test]
async fn visible_user_returns_the_viewer() {
    let fixture = fixture();
    let viewer = Principal {
        user_id: fixture.user_id,
        role: UserRole::Member,
    };

    let user = fixture
        .service
        .visible_user(&viewer, "flynn")
        .await
        .expect("storage succeeds");
    assert_eq!(user.map(|user| user.username), Some("flynn".to_string()));
}

#[tokio::test]
async fn visible_user_hides_other_and_unknown_users() {
    let fixture = fixture();
    fixture.repository.insert_user(User {
        id: UserId::from_uuid(Uuid::now_v7()),
        username: "other".to_string(),
        display_name: "Other".to_string(),
        role: UserRole::Member,
    });
    let viewer = Principal {
        user_id: fixture.user_id,
        role: UserRole::Member,
    };

    for username in ["other", "nobody"] {
        let user = fixture
            .service
            .visible_user(&viewer, username)
            .await
            .expect("storage succeeds");
        assert!(user.is_none(), "username: {username}");
    }
}

#[tokio::test]
async fn directory_is_scoped_by_role() {
    let fixture = fixture();
    let admin_id = UserId::from_uuid(Uuid::now_v7());
    fixture.repository.insert_user(User {
        id: admin_id,
        username: "root".to_string(),
        display_name: "Root".to_string(),
        role: UserRole::Admin,
    });
    let secret = mint(&fixture);
    let credential_id = fixture
        .repository
        .app_passwords
        .lock()
        .expect("app passwords lock")[0]
        .id;

    let member = Principal {
        user_id: fixture.user_id,
        role: UserRole::Member,
    };
    let listed = fixture
        .service
        .directory(&member)
        .await
        .expect("storage succeeds");
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].username, "flynn");
    assert_eq!(listed[0].role, UserRole::Member);

    let admin = Principal {
        user_id: admin_id,
        role: UserRole::Admin,
    };
    let listed = fixture
        .service
        .directory(&admin)
        .await
        .expect("storage succeeds");
    assert_eq!(
        listed
            .iter()
            .map(|user| user.username.as_str())
            .collect::<Vec<_>>(),
        vec!["flynn", "root"]
    );

    let rendered = format!("{listed:?}");
    assert!(!rendered.contains(&secret));
    assert!(!rendered.contains(&credential_id.to_string()));
}
