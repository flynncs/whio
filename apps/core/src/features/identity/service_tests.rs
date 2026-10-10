use std::collections::HashMap;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};

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
    calls: AtomicUsize,
    fail_user_lookup: AtomicBool,
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
        self.calls.fetch_add(1, Ordering::Relaxed);
        if self.fail_user_lookup.load(Ordering::Relaxed) {
            return Err(CredentialStoreError::Unavailable);
        }
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
        self.calls.fetch_add(1, Ordering::Relaxed);
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
    now: Arc<Mutex<chrono::DateTime<chrono::Utc>>>,
}

fn fixture() -> Fixture {
    fixture_with_burst(10)
}

fn fixture_with_burst(burst: u32) -> Fixture {
    let key = CredentialKey::from_base64(&STANDARD_NO_PAD.encode([11u8; 32])).expect("valid key");
    let repository = Arc::new(StubRepository {
        users: Mutex::new(HashMap::new()),
        api_keys: Mutex::new(Vec::new()),
        app_passwords: Mutex::new(Vec::new()),
        calls: AtomicUsize::new(0),
        fail_user_lookup: AtomicBool::new(false),
    });
    let user_id = UserId::from_uuid(Uuid::now_v7());
    repository.insert_user(User {
        id: user_id,
        username: "flynn".to_string(),
        display_name: "Flynn".to_string(),
        role: UserRole::Member,
    });

    let (clock, now) = manual_clock();
    let service = CredentialService::new_with_clock(
        repository.clone(),
        key.clone(),
        throttle_config(burst),
        clock,
    );
    Fixture {
        service,
        repository,
        key,
        user_id,
        now,
    }
}

fn throttle_config(burst: u32) -> crate::features::identity::LoginThrottleConfig {
    crate::features::identity::LoginThrottleConfig {
        burst: std::num::NonZeroU32::new(burst).unwrap(),
        replenish: std::time::Duration::from_secs(60),
    }
}

fn manual_clock() -> (
    crate::platform::clock::Clock,
    Arc<Mutex<chrono::DateTime<chrono::Utc>>>,
) {
    let now = Arc::new(Mutex::new(chrono::Utc::now()));
    let clock_now = Arc::clone(&now);
    (
        crate::platform::clock::Clock::from_fn(move || *clock_now.lock().expect("clock lock")),
        now,
    )
}

fn context() -> crate::platform::request::RequestContext {
    crate::platform::request::RequestContext::new(crate::platform::request::RequestId::generate())
}

fn context_with_ip(ip: &str) -> crate::platform::request::RequestContext {
    context().with_client_ip(ip.parse().unwrap())
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
        .authenticate(
            &PresentedAuth::ApiKey {
                secret: secret.clone(),
            },
            &context(),
        )
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
        .authenticate(
            &PresentedAuth::ApiKey {
                secret: secret.clone(),
            },
            &context(),
        )
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
        .authenticate(
            &PresentedAuth::ApiKey {
                secret: format!("whio_{}", URL_SAFE_NO_PAD.encode([9u8; 32])),
            },
            &context(),
        )
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
        .authenticate(
            &PresentedAuth::Token {
                username: "flynn".to_string(),
                token: subsonic_token(&secret, salt),
                salt: salt.to_string(),
            },
            &context(),
        )
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
        .authenticate(
            &PresentedAuth::Token {
                username: "flynn".to_string(),
                token: subsonic_token(&secret, "other-salt"),
                salt: "real-salt".to_string(),
            },
            &context(),
        )
        .await;

    assert_eq!(result, Err(AuthError::InvalidCredentials));
}

#[tokio::test]
async fn unknown_user_and_wrong_password_are_indistinguishable() {
    let fixture = fixture();
    mint(&fixture);

    let unknown_user = fixture
        .service
        .authenticate(
            &PresentedAuth::Token {
                username: "nobody".to_string(),
                token: subsonic_token("whio_x", "salt"),
                salt: "salt".to_string(),
            },
            &context(),
        )
        .await
        .unwrap_err();

    let known_user_bad_secret = fixture
        .service
        .authenticate(
            &PresentedAuth::Token {
                username: "flynn".to_string(),
                token: subsonic_token("whio_not_the_secret", "salt"),
                salt: "salt".to_string(),
            },
            &context(),
        )
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
        .authenticate(
            &PresentedAuth::Password {
                username: "flynn".to_string(),
                password: secret,
            },
            &context(),
        )
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
        .authenticate(
            &PresentedAuth::Password {
                username: "flynn".to_string(),
                password: "whio_definitely_not_it".to_string(),
            },
            &context(),
        )
        .await;

    assert_eq!(result, Err(AuthError::InvalidCredentials));
}
#[tokio::test]
async fn account_is_refused_after_burst_failures_across_ips() {
    let fixture = fixture_with_burst(2);
    let secret = mint(&fixture);
    for ip in ["192.0.2.1", "192.0.2.2"] {
        assert_eq!(
            fixture
                .service
                .authenticate_password("flynn", "wrong", &context_with_ip(ip))
                .await,
            Err(AuthError::InvalidCredentials)
        );
    }
    let calls = fixture.repository.calls.load(Ordering::Relaxed);
    assert_eq!(
        fixture
            .service
            .authenticate_password("flynn", &secret, &context_with_ip("192.0.2.3"))
            .await,
        Err(AuthError::InvalidCredentials)
    );
    assert_eq!(fixture.repository.calls.load(Ordering::Relaxed), calls);
}

#[tokio::test]
async fn ip_is_refused_after_burst_failures_across_accounts() {
    let fixture = fixture_with_burst(2);
    for username in ["first", "second"] {
        assert_eq!(
            fixture
                .service
                .authenticate_password(username, "wrong", &context_with_ip("192.0.2.10"))
                .await,
            Err(AuthError::InvalidCredentials)
        );
    }
    let calls = fixture.repository.calls.load(Ordering::Relaxed);
    assert_eq!(
        fixture
            .service
            .authenticate_password("flynn", "not-checked", &context_with_ip("192.0.2.10"))
            .await,
        Err(AuthError::InvalidCredentials)
    );
    assert_eq!(fixture.repository.calls.load(Ordering::Relaxed), calls);
}

#[tokio::test]
async fn refused_attempt_with_the_right_password_makes_no_repository_call() {
    let fixture = fixture_with_burst(1);
    let secret = mint(&fixture);
    assert_eq!(
        fixture
            .service
            .authenticate_password("flynn", "wrong", &context())
            .await,
        Err(AuthError::InvalidCredentials)
    );
    let calls = fixture.repository.calls.load(Ordering::Relaxed);
    assert_eq!(
        fixture
            .service
            .authenticate_password("flynn", &secret, &context())
            .await,
        Err(AuthError::InvalidCredentials)
    );
    assert_eq!(fixture.repository.calls.load(Ordering::Relaxed), calls);
}

#[tokio::test]
async fn allowance_returns_after_the_replenish_interval() {
    let fixture = fixture_with_burst(1);
    let secret = mint(&fixture);
    assert_eq!(
        fixture
            .service
            .authenticate_password("flynn", "wrong", &context())
            .await,
        Err(AuthError::InvalidCredentials)
    );
    *fixture.now.lock().expect("clock lock") += chrono::Duration::seconds(60);
    assert!(
        fixture
            .service
            .authenticate_password("flynn", &secret, &context())
            .await
            .is_ok()
    );
}

#[tokio::test]
async fn success_consumes_no_allowance() {
    let fixture = fixture_with_burst(1);
    let secret = mint(&fixture);
    assert!(
        fixture
            .service
            .authenticate_password("flynn", &secret, &context())
            .await
            .is_ok()
    );
    let calls = fixture.repository.calls.load(Ordering::Relaxed);
    assert_eq!(
        fixture
            .service
            .authenticate_password("flynn", "wrong", &context())
            .await,
        Err(AuthError::InvalidCredentials)
    );
    assert!(fixture.repository.calls.load(Ordering::Relaxed) > calls);
    *fixture.now.lock().expect("clock lock") += chrono::Duration::seconds(60);
    assert!(
        fixture
            .service
            .authenticate_password("flynn", &secret, &context())
            .await
            .is_ok()
    );
}

#[tokio::test]
async fn storage_error_consumes_no_allowance() {
    let fixture = fixture_with_burst(1);
    fixture
        .repository
        .fail_user_lookup
        .store(true, Ordering::Relaxed);
    assert_eq!(
        fixture
            .service
            .authenticate_password("flynn", "wrong", &context())
            .await,
        Err(AuthError::Storage(CredentialStoreError::Unavailable))
    );
    fixture
        .repository
        .fail_user_lookup
        .store(false, Ordering::Relaxed);
    let secret = mint(&fixture);
    assert!(
        fixture
            .service
            .authenticate_password("flynn", &secret, &context())
            .await
            .is_ok()
    );
}

#[tokio::test]
async fn unknown_username_counts_like_a_wrong_password() {
    let fixture = fixture_with_burst(1);
    let secret = mint(&fixture);
    assert_eq!(
        fixture
            .service
            .authenticate_password("nobody", "wrong", &context_with_ip("192.0.2.44"))
            .await,
        Err(AuthError::InvalidCredentials)
    );
    assert_eq!(
        fixture
            .service
            .authenticate_password("flynn", &secret, &context_with_ip("192.0.2.44"))
            .await,
        Err(AuthError::InvalidCredentials)
    );
}

#[tokio::test]
async fn username_key_is_case_insensitive() {
    let fixture = fixture_with_burst(1);
    let secret = mint(&fixture);
    assert_eq!(
        fixture
            .service
            .authenticate_password("FLYNN", "wrong", &context())
            .await,
        Err(AuthError::InvalidCredentials)
    );
    assert_eq!(
        fixture
            .service
            .authenticate_password("flynn", &secret, &context())
            .await,
        Err(AuthError::InvalidCredentials)
    );
}

#[tokio::test]
async fn api_key_failures_count_against_ip_only() {
    let fixture = fixture_with_burst(1);
    let invalid = PresentedAuth::ApiKey {
        secret: "invalid".to_owned(),
    };
    assert_eq!(
        fixture
            .service
            .authenticate(&invalid, &context_with_ip("192.0.2.45"))
            .await,
        Err(AuthError::InvalidCredentials)
    );
    let secret = mint(&fixture);
    assert_eq!(
        fixture
            .service
            .authenticate_password("flynn", &secret, &context_with_ip("192.0.2.45"))
            .await,
        Err(AuthError::InvalidCredentials)
    );
    assert!(
        fixture
            .service
            .authenticate_password("flynn", &secret, &context_with_ip("192.0.2.46"))
            .await
            .is_ok()
    );
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

mod destination_credentials {
    use std::sync::{Arc, Mutex};

    use async_trait::async_trait;
    use base64::Engine;
    use base64::engine::general_purpose::STANDARD_NO_PAD;
    use chrono::Utc;
    use uuid::Uuid;

    use crate::features::identity::destinations::{
        DestinationError, DestinationProvider, DestinationService, DestinationValidator,
        ValidatedDestination, ValidationError,
    };
    use crate::features::identity::domain::{Principal, UserId, UserRole};
    use crate::features::identity::secrets::CredentialKey;
    use crate::features::identity::storage::destinations::{
        DestinationAccount, DestinationStore, DestinationStoreError,
    };

    const TOKEN: &str = "lb-token-abc";
    const NONCE_LEN: usize = 12;

    #[derive(Default)]
    struct FakeStore {
        accounts: Mutex<Vec<DestinationAccount>>,
        reads: Mutex<usize>,
        writes: Mutex<usize>,
    }

    impl FakeStore {
        fn accounts(&self) -> Vec<DestinationAccount> {
            self.accounts.lock().unwrap().clone()
        }
    }

    #[async_trait]
    impl DestinationStore for FakeStore {
        async fn upsert_account(
            &self,
            account: &DestinationAccount,
        ) -> Result<(), DestinationStoreError> {
            *self.writes.lock().unwrap() += 1;
            let mut accounts = self.accounts.lock().unwrap();

            match accounts.iter_mut().find(|stored| {
                stored.user_id == account.user_id && stored.provider_id == account.provider_id
            }) {
                Some(stored) => *stored = account.clone(),
                None => accounts.push(account.clone()),
            }

            Ok(())
        }

        async fn find_account(
            &self,
            user_id: Uuid,
            provider_id: &str,
        ) -> Result<Option<DestinationAccount>, DestinationStoreError> {
            *self.reads.lock().unwrap() += 1;

            Ok(self
                .accounts
                .lock()
                .unwrap()
                .iter()
                .find(|account| account.user_id == user_id && account.provider_id == provider_id)
                .cloned())
        }

        async fn list_accounts(
            &self,
            user_id: Uuid,
        ) -> Result<Vec<DestinationAccount>, DestinationStoreError> {
            *self.reads.lock().unwrap() += 1;

            Ok(self
                .accounts
                .lock()
                .unwrap()
                .iter()
                .filter(|account| account.user_id == user_id)
                .cloned()
                .collect())
        }

        async fn set_enabled(
            &self,
            destination_id: Uuid,
            enabled: bool,
        ) -> Result<bool, DestinationStoreError> {
            match self
                .accounts
                .lock()
                .unwrap()
                .iter_mut()
                .find(|account| account.id == destination_id)
            {
                Some(account) => {
                    account.enabled = enabled;
                    Ok(true)
                }
                None => Ok(false),
            }
        }

        async fn revoke(&self, destination_id: Uuid) -> Result<bool, DestinationStoreError> {
            match self
                .accounts
                .lock()
                .unwrap()
                .iter_mut()
                .find(|account| account.id == destination_id && account.revoked_at.is_none())
            {
                Some(account) => {
                    account.revoked_at = Some(Utc::now());
                    Ok(true)
                }
                None => Ok(false),
            }
        }

        async fn list_enabled(
            &self,
            user_id: Uuid,
        ) -> Result<Vec<DestinationAccount>, DestinationStoreError> {
            Ok(self
                .accounts
                .lock()
                .unwrap()
                .iter()
                .filter(|account| {
                    account.user_id == user_id && account.enabled && account.revoked_at.is_none()
                })
                .cloned()
                .collect())
        }
    }

    struct FakeValidator {
        calls: Mutex<Vec<(DestinationProvider, String)>>,
        answer: Mutex<Result<ValidatedDestination, ValidationError>>,
    }

    impl FakeValidator {
        fn accepting(username: Option<&str>) -> Arc<Self> {
            Arc::new(Self {
                calls: Mutex::new(Vec::new()),
                answer: Mutex::new(Ok(ValidatedDestination {
                    provider_id: DestinationProvider::ListenBrainz,
                    username: username.map(str::to_owned),
                })),
            })
        }

        fn refusing(error: ValidationError) -> Arc<Self> {
            Arc::new(Self {
                calls: Mutex::new(Vec::new()),
                answer: Mutex::new(Err(error)),
            })
        }

        fn answer(&self, answer: Result<ValidatedDestination, ValidationError>) {
            *self.answer.lock().unwrap() = answer;
        }

        fn calls(&self) -> Vec<(DestinationProvider, String)> {
            self.calls.lock().unwrap().clone()
        }
    }

    #[async_trait]
    impl DestinationValidator for FakeValidator {
        async fn validate(
            &self,
            provider_id: &DestinationProvider,
            token: &str,
        ) -> Result<ValidatedDestination, ValidationError> {
            self.calls
                .lock()
                .unwrap()
                .push((*provider_id, token.to_owned()));

            self.answer.lock().unwrap().clone()
        }
    }

    fn key() -> CredentialKey {
        CredentialKey::from_base64(&STANDARD_NO_PAD.encode([11u8; 32])).expect("valid test key")
    }

    fn principal() -> Principal {
        Principal {
            user_id: UserId::from_uuid(Uuid::now_v7()),
            role: UserRole::Member,
        }
    }

    fn service(
        validator: Arc<FakeValidator>,
        key: Option<CredentialKey>,
    ) -> (Arc<FakeStore>, DestinationService) {
        let store = Arc::new(FakeStore::default());

        (
            Arc::clone(&store),
            DestinationService::new(store, key, validator),
        )
    }

    fn nonce(account: &DestinationAccount) -> Vec<u8> {
        account.encrypted_secret[1..1 + NONCE_LEN].to_vec()
    }

    #[tokio::test]
    async fn a_connected_destination_stores_ciphertext_with_a_fresh_nonce_per_account() {
        let validator = FakeValidator::accepting(Some("flynnteh"));
        let (store, service) = service(Arc::clone(&validator), Some(key()));

        let first = service
            .connect(&principal(), "listenbrainz", TOKEN)
            .await
            .expect("a valid credential connects");
        let second = service
            .connect(&principal(), "listenbrainz", TOKEN)
            .await
            .expect("a valid credential connects");

        assert_eq!(first.provider_id, "listenbrainz");
        assert_eq!(first.username.as_deref(), Some("flynnteh"));
        assert!(!first.revoked);
        assert!(first.last_verified_at.is_some());
        assert_eq!(second.provider_id, "listenbrainz");

        let accounts = store.accounts();
        assert_eq!(accounts.len(), 2);
        assert_ne!(accounts[0].id, accounts[1].id);
        assert_eq!(store.writes.lock().unwrap().clone(), 2);

        for account in &accounts {
            assert_ne!(account.encrypted_secret, TOKEN.as_bytes());
            assert!(
                !account
                    .encrypted_secret
                    .windows(TOKEN.len())
                    .any(|window| window == TOKEN.as_bytes()),
                "the plaintext token must not appear in the stored blob"
            );
            assert_eq!(account.provider_id, "listenbrainz");
            assert!(account.enabled);
            assert!(account.revoked_at.is_none());
        }

        assert_ne!(nonce(&accounts[0]), nonce(&accounts[1]));
    }

    #[tokio::test]
    async fn an_invalid_credential_stores_nothing() {
        for answer in [
            ValidationError::InvalidCredential,
            ValidationError::Unavailable,
        ] {
            let validator = FakeValidator::refusing(answer.clone());
            let (store, service) = service(Arc::clone(&validator), Some(key()));
            let caller = principal();

            let error = service
                .connect(&caller, "listenbrainz", "bad-token")
                .await
                .expect_err("a refused credential must not connect");

            let expected = match answer {
                ValidationError::InvalidCredential => DestinationError::InvalidCredential,
                ValidationError::Unavailable => DestinationError::Unavailable,
            };

            assert_eq!(error, expected);
            assert!(store.accounts().is_empty());
            assert_eq!(store.writes.lock().unwrap().clone(), 0);
            assert_eq!(validator.calls().len(), 1);
        }
    }

    #[tokio::test]
    async fn a_second_connect_never_replaces_the_verified_credential() {
        let validator = FakeValidator::accepting(Some("flynnteh"));
        let (store, service) = service(Arc::clone(&validator), Some(key()));
        let caller = principal();

        service
            .connect(&caller, "listenbrainz", TOKEN)
            .await
            .expect("a valid credential connects");
        let stored = store.accounts()[0].encrypted_secret.clone();

        assert_eq!(
            service
                .connect(&caller, "listenbrainz", "other-token")
                .await,
            Err(DestinationError::AlreadyConnected)
        );

        assert_eq!(store.accounts()[0].encrypted_secret, stored);
        assert_eq!(store.writes.lock().unwrap().clone(), 1);
        assert_eq!(validator.calls().len(), 1);
    }

    #[tokio::test]
    async fn the_status_projection_serialises_without_the_token_or_the_ciphertext() {
        let validator = FakeValidator::accepting(Some("flynnteh"));
        let (store, service) = service(Arc::clone(&validator), Some(key()));
        let caller = principal();

        service
            .connect(&caller, "listenbrainz", TOKEN)
            .await
            .expect("a valid credential connects");

        let statuses = service.status(&caller).await.expect("storage succeeds");
        let value = serde_json::to_value(&statuses).expect("statuses serialise");
        let mut members = value[0]
            .as_object()
            .expect("a status object")
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>();
        members.sort_unstable();

        assert_eq!(
            members,
            vec![
                "connected_at",
                "enabled",
                "last_verified_at",
                "provider_id",
                "revoked",
                "username",
            ]
        );
        assert!(!value.to_string().contains(TOKEN));
        assert!(!value.to_string().contains("token"));
        assert!(store.accounts()[0].encrypted_secret.len() > TOKEN.len());
    }

    #[tokio::test]
    async fn disabling_hides_the_destination_and_revoking_makes_the_credential_unusable() {
        let validator = FakeValidator::accepting(Some("flynnteh"));
        let (store, service) = service(Arc::clone(&validator), Some(key()));
        let caller = principal();
        let user_id = caller.user_id.as_uuid();

        service
            .connect(&caller, "listenbrainz", TOKEN)
            .await
            .expect("a valid credential connects");

        let disabled = service
            .set_enabled(&caller, "listenbrainz", false)
            .await
            .expect("the account is disabled");
        assert!(!disabled.enabled);
        assert!(
            service
                .enabled_destinations(user_id)
                .await
                .expect("storage succeeds")
                .is_empty()
        );
        assert_eq!(store.accounts().len(), 1);

        let enabled = service
            .set_enabled(&caller, "listenbrainz", true)
            .await
            .expect("the account is enabled");
        assert!(enabled.enabled);
        assert_eq!(
            service
                .enabled_destinations(user_id)
                .await
                .expect("storage succeeds")
                .len(),
            1
        );

        service
            .disconnect(&caller, "listenbrainz")
            .await
            .expect("the account is revoked");

        let revoked = service.status(&caller).await.expect("storage succeeds");
        assert!(revoked[0].revoked);
        assert!(revoked[0].enabled);
        assert_eq!(store.accounts().len(), 1);
        assert!(
            service
                .enabled_credential(user_id, "listenbrainz")
                .await
                .expect("storage succeeds")
                .is_none()
        );
        assert!(
            service
                .enabled_destinations(user_id)
                .await
                .expect("storage succeeds")
                .is_empty()
        );
    }

    #[tokio::test]
    async fn conflicts_missing_accounts_and_unknown_providers_are_typed_errors() {
        let validator = FakeValidator::accepting(None);
        let (store, service) = service(Arc::clone(&validator), Some(key()));
        let caller = principal();

        assert_eq!(
            service.set_enabled(&caller, "listenbrainz", true).await,
            Err(DestinationError::NotFound)
        );
        assert_eq!(
            service.disconnect(&caller, "listenbrainz").await,
            Err(DestinationError::NotFound)
        );
        assert_eq!(
            service.connect(&caller, "lastfm", TOKEN).await,
            Err(DestinationError::Unsupported)
        );
        assert_eq!(
            service.set_enabled(&caller, "lastfm", true).await,
            Err(DestinationError::Unsupported)
        );
        assert_eq!(
            service.disconnect(&caller, "lastfm").await,
            Err(DestinationError::Unsupported)
        );
        assert!(validator.calls().is_empty());
        assert!(store.accounts().is_empty());

        service
            .connect(&caller, "listenbrainz", TOKEN)
            .await
            .expect("a valid credential connects");

        assert_eq!(
            service.connect(&caller, "listenbrainz", TOKEN).await,
            Err(DestinationError::AlreadyConnected)
        );
        assert_eq!(validator.calls().len(), 1);
    }

    #[tokio::test]
    async fn two_principals_never_see_each_others_destinations_or_credentials() {
        let validator = FakeValidator::accepting(Some("first"));
        let (store, service) = service(Arc::clone(&validator), Some(key()));
        let first = principal();
        let second = principal();

        service
            .connect(&first, "listenbrainz", "token-first")
            .await
            .expect("the first credential connects");
        validator.answer(Ok(ValidatedDestination {
            provider_id: DestinationProvider::ListenBrainz,
            username: Some("second".to_owned()),
        }));
        service
            .connect(&second, "listenbrainz", "token-second")
            .await
            .expect("the second credential connects");

        let first_status = service.status(&first).await.expect("storage succeeds");
        let second_status = service.status(&second).await.expect("storage succeeds");

        assert_eq!(first_status.len(), 1);
        assert_eq!(second_status.len(), 1);
        assert_eq!(first_status[0].username.as_deref(), Some("first"));
        assert_eq!(second_status[0].username.as_deref(), Some("second"));

        let first_credential = service
            .enabled_credential(first.user_id.as_uuid(), "listenbrainz")
            .await
            .expect("storage succeeds")
            .expect("the first credential");
        let second_credential = service
            .enabled_credential(second.user_id.as_uuid(), "listenbrainz")
            .await
            .expect("storage succeeds")
            .expect("the second credential");

        assert_eq!(first_credential.token, "token-first");
        assert_eq!(second_credential.token, "token-second");
        assert_ne!(
            first_credential.destination_id,
            second_credential.destination_id
        );
        assert_eq!(store.accounts().len(), 2);
        assert!(
            service
                .enabled_destinations(Uuid::now_v7())
                .await
                .expect("storage succeeds")
                .is_empty()
        );
    }

    #[tokio::test]
    async fn the_stored_token_round_trips_and_a_changed_row_id_fails_closed() {
        let validator = FakeValidator::accepting(Some("flynnteh"));
        let (store, service) = service(Arc::clone(&validator), Some(key()));
        let caller = principal();
        let user_id = caller.user_id.as_uuid();

        service
            .connect(&caller, "listenbrainz", TOKEN)
            .await
            .expect("a valid credential connects");

        let credential = service
            .enabled_credential(user_id, "listenbrainz")
            .await
            .expect("storage succeeds")
            .expect("the stored credential");

        assert_eq!(credential.token, TOKEN);
        assert_eq!(credential.provider_id, DestinationProvider::ListenBrainz);
        assert_eq!(credential.username.as_deref(), Some("flynnteh"));
        assert_eq!(credential.destination_id, store.accounts()[0].id);
        assert!(!format!("{credential:?}").contains(TOKEN));

        store.accounts.lock().unwrap()[0].id = Uuid::now_v7();

        assert!(matches!(
            service.enabled_credential(user_id, "listenbrainz").await,
            Err(DestinationError::Repository(
                DestinationStoreError::InvalidData
            ))
        ));
    }

    #[tokio::test]
    async fn an_absent_credential_key_answers_disabled_without_touching_the_store() {
        let validator = FakeValidator::accepting(Some("flynnteh"));
        let (store, service) = service(Arc::clone(&validator), None);
        let caller = principal();
        let user_id = caller.user_id.as_uuid();

        assert_eq!(
            service.connect(&caller, "listenbrainz", TOKEN).await,
            Err(DestinationError::Disabled)
        );
        assert_eq!(
            service.status(&caller).await,
            Err(DestinationError::Disabled)
        );
        assert_eq!(
            service.set_enabled(&caller, "listenbrainz", true).await,
            Err(DestinationError::Disabled)
        );
        assert_eq!(
            service.disconnect(&caller, "listenbrainz").await,
            Err(DestinationError::Disabled)
        );
        assert!(matches!(
            service.enabled_credential(user_id, "listenbrainz").await,
            Err(DestinationError::Disabled)
        ));
        assert_eq!(store.reads.lock().unwrap().clone(), 0);
        assert_eq!(store.writes.lock().unwrap().clone(), 0);
        assert!(validator.calls().is_empty());
    }

    #[tokio::test]
    async fn the_validator_runs_once_per_connect_and_never_for_reads_or_writes() {
        let validator = FakeValidator::accepting(None);
        let (_, service) = service(Arc::clone(&validator), Some(key()));
        let caller = principal();
        let user_id = caller.user_id.as_uuid();

        service
            .connect(&caller, "listenbrainz", TOKEN)
            .await
            .expect("a valid credential connects");

        assert_eq!(validator.calls().len(), 1);
        assert_eq!(validator.calls()[0].1, TOKEN);

        service.status(&caller).await.expect("storage succeeds");
        service
            .set_enabled(&caller, "listenbrainz", false)
            .await
            .expect("the account is disabled");
        service
            .set_enabled(&caller, "listenbrainz", true)
            .await
            .expect("the account is enabled");
        service
            .enabled_credential(user_id, "listenbrainz")
            .await
            .expect("storage succeeds");
        service
            .enabled_destinations(user_id)
            .await
            .expect("storage succeeds");

        assert_eq!(validator.calls().len(), 1);

        service
            .disconnect(&caller, "listenbrainz")
            .await
            .expect("the account is revoked");

        assert_eq!(validator.calls().len(), 1);

        service
            .connect(&caller, "listenbrainz", TOKEN)
            .await
            .expect("a revoked account re-verifies before it replaces");

        assert_eq!(validator.calls().len(), 2);
    }
}
