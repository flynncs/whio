use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;

use crate::identity::domain::UserId;
use crate::identity::secrets::{CredentialKey, decrypt_secret, encrypt_secret};

const TTL_SECS: u64 = 48 * 3600;

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock")
        .as_secs()
}

pub(crate) fn mint(user: UserId, credential: uuid::Uuid, key: &CredentialKey) -> String {
    let body = format!(
        "{}:{}:{}",
        user.as_uuid(),
        credential,
        now_secs() + TTL_SECS
    );
    let blob = encrypt_secret(key, body.as_bytes(), b"nd-session").expect("session seal");
    URL_SAFE_NO_PAD.encode(blob)
}

pub(crate) fn verify(token: &str, key: &CredentialKey) -> Option<(UserId, uuid::Uuid)> {
    let blob = URL_SAFE_NO_PAD.decode(token).ok()?;
    let plain = decrypt_secret(key, &blob, b"nd-session").ok()?;
    let text = String::from_utf8(plain).ok()?;
    let (ids, exp) = text.rsplit_once(':')?;
    if exp.parse::<u64>().ok()? <= now_secs() {
        return None;
    }
    let (user, credential) = ids.rsplit_once(':')?;
    Some((
        UserId::from_uuid(user.parse().ok()?),
        credential.parse().ok()?,
    ))
}

pub(crate) async fn authorize(
    credential: &crate::identity::service::CredentialService,
    token: &str,
) -> Result<Option<UserId>, crate::identity::service::AuthError> {
    let Some((user, credential_id)) = verify(token, credential.key()) else {
        return Ok(None);
    };

    Ok(credential
        .app_password_active(credential_id)
        .await?
        .then_some(user))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::{
        domain::{ApiKeyCredential, AppPasswordCredential, User, UserId},
        repository::{CredentialRepository, CredentialStoreError},
        service::AuthError,
    };
    use async_trait::async_trait;
    use std::sync::Arc;
    use uuid::Uuid;

    fn key() -> CredentialKey {
        CredentialKey::from_base64("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA").unwrap()
    }

    fn other_key() -> CredentialKey {
        CredentialKey::from_base64("AQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQE").unwrap()
    }

    #[test]
    fn roundtrip_preserves_user() {
        let id = UserId::from_uuid(Uuid::now_v7());
        let credential = Uuid::now_v7();
        let token = mint(id, credential, &key());
        assert_eq!(verify(&token, &key()), Some((id, credential)));
    }

    #[test]
    fn wrong_key_rejected() {
        let token = mint(UserId::from_uuid(Uuid::now_v7()), Uuid::now_v7(), &key());
        assert_eq!(verify(&token, &other_key()), None);
    }

    #[test]
    fn tampered_token_rejected() {
        let mut token = mint(UserId::from_uuid(Uuid::now_v7()), Uuid::now_v7(), &key());
        token.push('x');
        assert_eq!(verify(&token, &key()), None);
    }

    #[test]
    fn legacy_two_part_tokens_are_rejected() {
        use base64::Engine;
        let body = format!("{}:{}", Uuid::now_v7(), 9_999_999_999u64,);
        let blob = crate::identity::secrets::encrypt_secret(&key(), body.as_bytes(), b"nd-session")
            .unwrap();
        let token = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(blob);
        assert_eq!(verify(&token, &key()), None);
    }

    #[test]
    fn garbage_rejected() {
        assert_eq!(verify("not-a-token", &key()), None);
        assert_eq!(verify("", &key()), None);
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

    #[tokio::test]
    async fn authorize_propagates_storage_failures() {
        let user = UserId::from_uuid(Uuid::now_v7());
        let credential_id = Uuid::now_v7();
        let key = key();
        let credential = crate::identity::service::CredentialService::new(
            Arc::new(UnavailableCredentialStore),
            key.clone(),
        );
        let token = mint(user, credential_id, &key);

        assert_eq!(
            authorize(&credential, &token).await,
            Err(AuthError::Storage(CredentialStoreError::Unavailable))
        );
    }
}
