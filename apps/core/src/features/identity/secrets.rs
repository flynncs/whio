use std::fmt;

use aws_lc_rs::aead::{self, Aad, LessSafeKey, Nonce, UnboundKey};
use base64::Engine;
use base64::engine::general_purpose::{STANDARD_NO_PAD, URL_SAFE_NO_PAD};
use sha2::{Digest, Sha256};

pub const SECRET_PREFIX: &str = "whio_";

const BLOB_VERSION: u8 = 1;
const KEY_LEN: usize = 32;
const SECRET_RANDOM_LEN: usize = 32;
const TAG_LEN: usize = 16;
const NONCE_LEN: usize = 12;
const MIN_BLOB_LEN: usize = 1 + NONCE_LEN + TAG_LEN;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SecretError {
    #[error("credential key must decode to exactly {KEY_LEN} bytes")]
    InvalidKeyLength,

    #[error("credential key is not valid base64")]
    InvalidKeyEncoding,

    #[error("encrypted secret has unsupported version {0}")]
    UnsupportedVersion(u8),

    #[error("encrypted secret blob is malformed")]
    MalformedBlob,

    // wrong key, tampering and aad mismatch must stay indistinguishable
    #[error("encrypted secret failed authentication")]
    DecryptionFailed,
}

#[derive(Clone)]
pub struct CredentialKey {
    bytes: [u8; KEY_LEN],
}

impl fmt::Debug for CredentialKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CredentialKey").finish_non_exhaustive()
    }
}

impl CredentialKey {
    pub fn from_base64(raw: &str) -> Result<Self, SecretError> {
        let trimmed = raw.trim_end_matches("=");

        let decoded = STANDARD_NO_PAD
            .decode(trimmed)
            .map_err(|_| SecretError::InvalidKeyEncoding)?;

        let bytes: [u8; KEY_LEN] = decoded
            .try_into()
            .map_err(|_| SecretError::InvalidKeyLength)?;

        Ok(Self { bytes })
    }
}

pub fn generate_app_secret() -> String {
    let mut raw = [0u8; SECRET_RANDOM_LEN];
    aws_lc_rs::rand::fill(&mut raw).expect("system rng");

    let mut secret = String::with_capacity(SECRET_PREFIX.len() + 43);
    secret.push_str(SECRET_PREFIX);
    secret.push_str(&URL_SAFE_NO_PAD.encode(raw));
    secret
}

pub fn lookup_digest(secret: &str) -> [u8; 32] {
    Sha256::digest(secret.as_bytes()).into()
}

// blob layout: version byte || nonce || ciphertext|| tag
pub fn encrypt_secret(
    key: &CredentialKey,
    plaintext: &[u8],
    associated_data: &[u8],
) -> Result<Vec<u8>, SecretError> {
    let mut nonce_bytes = [0u8; NONCE_LEN];
    aws_lc_rs::rand::fill(&mut nonce_bytes).expect("system rng");

    let mut sealed = plaintext.to_vec();
    let unbound = UnboundKey::new(&aead::AES_256_GCM, &key.bytes).expect("key length invariant");
    let nonce = Nonce::try_assume_unique_for_key(&nonce_bytes).expect("nonce length typed");
    LessSafeKey::new(unbound)
        .seal_in_place_append_tag(nonce, Aad::from(associated_data), &mut sealed)
        .expect("aes-gcm seal");

    let mut out = Vec::with_capacity(MIN_BLOB_LEN + plaintext.len());
    // if we ever decide to support new version, we can route old to its old logic
    // and not explode
    out.push(BLOB_VERSION);
    out.extend_from_slice(&nonce_bytes);
    out.extend_from_slice(&sealed);

    Ok(out)
}

pub fn decrypt_secret(
    key: &CredentialKey,
    blob: &[u8],
    associated_data: &[u8],
) -> Result<Vec<u8>, SecretError> {
    if blob.len() < MIN_BLOB_LEN {
        return Err(SecretError::MalformedBlob);
    }
    if blob[0] != BLOB_VERSION {
        return Err(SecretError::UnsupportedVersion(blob[0]));
    }

    let mut nonce_bytes = [0u8; NONCE_LEN];
    nonce_bytes.copy_from_slice(&blob[1..1 + NONCE_LEN]);
    let mut ct_tag = blob[1 + NONCE_LEN..].to_vec();

    let unbound = UnboundKey::new(&aead::AES_256_GCM, &key.bytes).expect("key length invariant");
    let opening_key = LessSafeKey::new(unbound);
    let nonce = Nonce::try_assume_unique_for_key(&nonce_bytes).expect("nonce length typed");

    let plaintext = opening_key
        .open_in_place(nonce, Aad::from(associated_data), &mut ct_tag)
        .map_err(|_| SecretError::DecryptionFailed)?;

    Ok(plaintext.to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::engine::general_purpose::STANDARD;

    fn key_from(bytes: [u8; KEY_LEN]) -> CredentialKey {
        CredentialKey::from_base64(&STANDARD_NO_PAD.encode(bytes)).expect("valid test key")
    }

    fn test_key() -> CredentialKey {
        key_from([7u8; KEY_LEN])
    }

    // sha256("abc")
    const ABC_SHA256: [u8; 32] = [
        0xba, 0x78, 0x16, 0xbf, 0x8f, 0x01, 0xcf, 0xea, //
        0x41, 0x41, 0x40, 0xde, 0x5d, 0xae, 0x22, 0x23, //
        0xb0, 0x03, 0x61, 0xa3, 0x96, 0x17, 0x7a, 0x9c, //
        0xb4, 0x10, 0xff, 0x61, 0xf2, 0x00, 0x15, 0xad,
    ];

    #[test]
    fn round_trip_preserves_plaintext() {
        let key = test_key();
        let plaintext = b"whio_minted_secret_value";
        let aad = b"credential-uuid-as-bytes";

        let blob = encrypt_secret(&key, plaintext, aad).expect("seal succeeds");

        assert_eq!(blob[0], BLOB_VERSION);

        let recovered = decrypt_secret(&key, &blob, aad).expect("open succeeds");
        assert_eq!(recovered, plaintext.to_vec());
    }

    #[test]
    fn wrong_associated_data_fails() {
        let key = test_key();
        let blob = encrypt_secret(&key, b"secret", b"credential-A").expect("seal succeeds");

        let error = decrypt_secret(&key, &blob, b"credential-B").unwrap_err();

        assert_eq!(error, SecretError::DecryptionFailed);
    }

    #[test]
    fn tampered_ciphertext_fails() {
        let key = test_key();
        let aad = b"credential-uuid-as-bytes";

        let mut flipped_ciphertext =
            encrypt_secret(&key, b"a plaintext of some length", aad).expect("seal succeeds");
        let first_ciphertext_byte = 1 + NONCE_LEN;
        flipped_ciphertext[first_ciphertext_byte] ^= 0x01;
        assert_eq!(
            decrypt_secret(&key, &flipped_ciphertext, aad),
            Err(SecretError::DecryptionFailed)
        );

        let mut flipped_tag =
            encrypt_secret(&key, b"a plaintext of some length", aad).expect("seal succeeds");
        *flipped_tag.last_mut().expect("non-empty blob") ^= 0x01;
        assert_eq!(
            decrypt_secret(&key, &flipped_tag, aad),
            Err(SecretError::DecryptionFailed)
        );
    }

    #[test]
    fn wrong_key_fails() {
        let sealing_key = test_key();
        let other_key = key_from([9u8; KEY_LEN]);
        let aad = b"credential-uuid-as-bytes";

        let blob = encrypt_secret(&sealing_key, b"secret", aad).expect("seal succeeds");

        assert_eq!(
            decrypt_secret(&other_key, &blob, aad),
            Err(SecretError::DecryptionFailed)
        );
    }

    #[test]
    fn unknown_blob_version_is_rejected() {
        let key = test_key();
        let aad = b"credential-uuid-as-bytes";
        let mut blob = encrypt_secret(&key, b"secret", aad).expect("seal succeeds");

        blob[0] = 0xFF;

        assert_eq!(
            decrypt_secret(&key, &blob, aad),
            Err(SecretError::UnsupportedVersion(255))
        );
    }

    #[test]
    fn short_blob_is_malformed() {
        let key = test_key();
        let aad = b"credential-uuid-as-bytes";
        let blob = encrypt_secret(&key, b"secret", aad).expect("seal succeeds");

        let truncated = &blob[..MIN_BLOB_LEN - 1];
        assert_eq!(
            decrypt_secret(&key, truncated, aad),
            Err(SecretError::MalformedBlob)
        );
        assert_eq!(
            decrypt_secret(&key, &[], aad),
            Err(SecretError::MalformedBlob)
        );
    }

    #[test]
    fn nonce_differs_between_encryptions() {
        let key = test_key();
        let aad = b"credential-uuid-as-bytes";

        let first = encrypt_secret(&key, b"identical plaintext", aad).expect("seal succeeds");
        let second = encrypt_secret(&key, b"identical plaintext", aad).expect("seal succeeds");

        fn nonce_bytes(blob: &[u8]) -> &[u8] {
            &blob[1..1 + NONCE_LEN]
        }

        assert_ne!(nonce_bytes(&first), nonce_bytes(&second));
    }

    #[test]
    fn generated_secret_has_expected_shape() {
        let secret = generate_app_secret();

        assert!(secret.starts_with(SECRET_PREFIX));
        let suffix = &secret[SECRET_PREFIX.len()..];
        // 32 bytes encode to exactly 43 unpadded base64url chars
        assert_eq!(suffix.len(), 43);
        let raw = URL_SAFE_NO_PAD
            .decode(suffix)
            .expect("suffix is valid base64url");
        assert_eq!(raw.len(), SECRET_RANDOM_LEN);

        assert_ne!(generate_app_secret(), secret);
    }

    #[test]
    fn lookup_digest_matches_known_sha256_vector() {
        assert_eq!(lookup_digest("abc"), ABC_SHA256);
    }

    #[test]
    fn key_parsing_rejects_wrong_lengths() {
        let short = STANDARD_NO_PAD.encode([0u8; KEY_LEN - 1]);
        let long = STANDARD_NO_PAD.encode([0u8; KEY_LEN + 1]);

        // CredentialKey has no PartialEq, so match the variant
        assert!(matches!(
            CredentialKey::from_base64(&short),
            Err(SecretError::InvalidKeyLength)
        ));
        assert!(matches!(
            CredentialKey::from_base64(&long),
            Err(SecretError::InvalidKeyLength)
        ));

        // - is outside the standard alphabet
        assert!(matches!(
            CredentialKey::from_base64("--not-base64--"),
            Err(SecretError::InvalidKeyEncoding)
        ));
    }

    #[test]
    fn key_parsing_tolerates_padding() {
        let padded = STANDARD.encode([7u8; KEY_LEN]);
        let unpadded = STANDARD_NO_PAD.encode([7u8; KEY_LEN]);

        let from_padded = CredentialKey::from_base64(&padded).expect("padding tolerated");
        let from_unpadded = CredentialKey::from_base64(&unpadded).expect("canonical form");

        assert_eq!(from_padded.bytes.len(), KEY_LEN);
        assert_eq!(from_padded.bytes, from_unpadded.bytes);
    }
}
