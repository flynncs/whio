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

pub(crate) fn mint(user: UserId, key: &CredentialKey) -> String {
    let body = format!("{}:{}", user.as_uuid(), now_secs() + TTL_SECS);
    let blob = encrypt_secret(key, body.as_bytes(), b"nd-session").expect("session seal");
    URL_SAFE_NO_PAD.encode(blob)
}

pub(crate) fn verify(token: &str, key: &CredentialKey) -> Option<UserId> {
    let blob = URL_SAFE_NO_PAD.decode(token).ok()?;
    let plain = decrypt_secret(key, &blob, b"nd-session").ok()?;
    let text = String::from_utf8(plain).ok()?;
    let (id, exp) = text.rsplit_once(':')?;
    if exp.parse::<u64>().ok()? <= now_secs() {
        return None;
    }
    Some(UserId::from_uuid(id.parse().ok()?))
}

#[cfg(test)]
mod tests {
    use super::*;
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
        let token = mint(id, &key());
        assert_eq!(verify(&token, &key()), Some(id));
    }

    #[test]
    fn wrong_key_rejected() {
        let token = mint(UserId::from_uuid(Uuid::now_v7()), &key());
        assert_eq!(verify(&token, &other_key()), None);
    }

    #[test]
    fn tampered_token_rejected() {
        let mut token = mint(UserId::from_uuid(Uuid::now_v7()), &key());
        token.push('x');
        assert_eq!(verify(&token, &key()), None);
    }

    #[test]
    fn garbage_rejected() {
        assert_eq!(verify("not-a-token", &key()), None);
        assert_eq!(verify("", &key()), None);
    }
}
