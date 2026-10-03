use std::fmt;
use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::Serialize;
use uuid::Uuid;

use super::domain::Principal;
use super::secrets::{CredentialKey, decrypt_secret, encrypt_secret};
use super::storage::destinations::{DestinationAccount, DestinationStore, DestinationStoreError};

const MAX_TOKEN_LEN: usize = 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DestinationProvider {
    ListenBrainz,
}

impl DestinationProvider {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "listenbrainz" => Some(Self::ListenBrainz),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::ListenBrainz => "listenbrainz",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DestinationError {
    #[error("destination credentials are not configured")]
    Disabled,

    #[error("the destination provider is not supported")]
    Unsupported,

    #[error("the destination rejected the credential")]
    InvalidCredential,

    #[error("a destination for this provider is already connected")]
    AlreadyConnected,

    #[error("the destination account was not found")]
    NotFound,

    #[error("the destination host is unavailable")]
    Unavailable,

    #[error("destination storage failed")]
    Repository(#[from] DestinationStoreError),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DestinationStatus {
    pub provider_id: String,
    pub username: Option<String>,
    pub enabled: bool,
    #[serde(serialize_with = "timestamp")]
    pub connected_at: DateTime<Utc>,
    #[serde(serialize_with = "optional_timestamp")]
    pub last_verified_at: Option<DateTime<Utc>>,
    pub revoked: bool,
}

pub struct DestinationCredential {
    pub destination_id: Uuid,
    pub provider_id: DestinationProvider,
    pub username: Option<String>,
    pub token: String,
}

impl fmt::Debug for DestinationCredential {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DestinationCredential")
            .field("destination_id", &self.destination_id)
            .field("provider_id", &self.provider_id)
            .field("username", &self.username)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidatedDestination {
    pub provider_id: DestinationProvider,
    pub username: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ValidationError {
    #[error("the destination rejected the credential")]
    InvalidCredential,

    #[error("the destination host is unavailable")]
    Unavailable,
}

#[async_trait]
pub trait DestinationValidator: Send + Sync {
    async fn validate(
        &self,
        provider_id: &DestinationProvider,
        token: &str,
    ) -> Result<ValidatedDestination, ValidationError>;
}

pub struct DestinationService {
    store: Arc<dyn DestinationStore>,
    key: Option<CredentialKey>,
    validator: Arc<dyn DestinationValidator>,
}

impl DestinationService {
    pub fn new(
        store: Arc<dyn DestinationStore>,
        key: Option<CredentialKey>,
        validator: Arc<dyn DestinationValidator>,
    ) -> Self {
        Self {
            store,
            key,
            validator,
        }
    }

    pub async fn connect(
        &self,
        caller: &Principal,
        provider_id: &str,
        token: &str,
    ) -> Result<DestinationStatus, DestinationError> {
        let provider =
            DestinationProvider::parse(provider_id).ok_or(DestinationError::Unsupported)?;
        let key = self.credential_key()?;

        if token.is_empty() || token.chars().count() > MAX_TOKEN_LEN {
            return Err(DestinationError::InvalidCredential);
        }

        let user_id = caller.user_id.as_uuid();
        let existing = self
            .store
            .find_account(user_id, provider.as_str())
            .await
            .map_err(DestinationError::Repository)?;

        if existing
            .as_ref()
            .is_some_and(|account| account.enabled && account.revoked_at.is_none())
        {
            return Err(DestinationError::AlreadyConnected);
        }

        let validated = self
            .validator
            .validate(&provider, token)
            .await
            .map_err(validation_failure)?;

        if validated.provider_id != provider {
            return Err(DestinationError::Unsupported);
        }

        let now = Utc::now();
        let destination_id = existing
            .as_ref()
            .map(|account| account.id)
            .unwrap_or_else(Uuid::now_v7);
        let created_at = existing
            .as_ref()
            .map(|account| account.created_at)
            .unwrap_or(now);

        let encrypted_secret = encrypt_secret(key, token.as_bytes(), destination_id.as_bytes())
            .map_err(|_| DestinationError::Repository(DestinationStoreError::Unavailable))?;

        let account = DestinationAccount {
            id: destination_id,
            user_id,
            provider_id: provider.as_str().to_owned(),
            external_username: validated.username,
            encrypted_secret,
            enabled: true,
            created_at,
            updated_at: now,
            last_verified_at: Some(now),
            revoked_at: None,
        };

        self.store
            .upsert_account(&account)
            .await
            .map_err(DestinationError::Repository)?;

        Ok(destination_status(&account))
    }

    pub async fn status(
        &self,
        caller: &Principal,
    ) -> Result<Vec<DestinationStatus>, DestinationError> {
        self.credential_key()?;

        let accounts = self
            .store
            .list_accounts(caller.user_id.as_uuid())
            .await
            .map_err(DestinationError::Repository)?;

        Ok(accounts.iter().map(destination_status).collect())
    }

    pub async fn set_enabled(
        &self,
        caller: &Principal,
        provider_id: &str,
        enabled: bool,
    ) -> Result<DestinationStatus, DestinationError> {
        let provider =
            DestinationProvider::parse(provider_id).ok_or(DestinationError::Unsupported)?;
        self.credential_key()?;
        let user_id = caller.user_id.as_uuid();

        let account = self
            .store
            .find_account(user_id, provider.as_str())
            .await
            .map_err(DestinationError::Repository)?
            .ok_or(DestinationError::NotFound)?;

        if !self
            .store
            .set_enabled(account.id, enabled)
            .await
            .map_err(DestinationError::Repository)?
        {
            return Err(DestinationError::NotFound);
        }

        let account = self
            .store
            .find_account(user_id, provider.as_str())
            .await
            .map_err(DestinationError::Repository)?
            .ok_or(DestinationError::NotFound)?;

        Ok(destination_status(&account))
    }

    pub async fn disconnect(
        &self,
        caller: &Principal,
        provider_id: &str,
    ) -> Result<(), DestinationError> {
        let provider =
            DestinationProvider::parse(provider_id).ok_or(DestinationError::Unsupported)?;
        self.credential_key()?;
        let user_id = caller.user_id.as_uuid();

        let account = self
            .store
            .find_account(user_id, provider.as_str())
            .await
            .map_err(DestinationError::Repository)?
            .ok_or(DestinationError::NotFound)?;

        if !self
            .store
            .revoke(account.id)
            .await
            .map_err(DestinationError::Repository)?
        {
            return Err(DestinationError::NotFound);
        }

        Ok(())
    }

    pub async fn enabled_credential(
        &self,
        user_id: Uuid,
        provider_id: &str,
    ) -> Result<Option<DestinationCredential>, DestinationError> {
        let provider =
            DestinationProvider::parse(provider_id).ok_or(DestinationError::Unsupported)?;
        let key = self.credential_key()?;

        let Some(account) = self
            .store
            .find_account(user_id, provider.as_str())
            .await
            .map_err(DestinationError::Repository)?
        else {
            return Ok(None);
        };

        if !account.enabled || account.revoked_at.is_some() {
            return Ok(None);
        }

        let plaintext = decrypt_secret(key, &account.encrypted_secret, account.id.as_bytes())
            .map_err(|_| DestinationError::Repository(DestinationStoreError::InvalidData))?;
        let token = String::from_utf8(plaintext)
            .map_err(|_| DestinationError::Repository(DestinationStoreError::InvalidData))?;

        Ok(Some(DestinationCredential {
            destination_id: account.id,
            provider_id: provider,
            username: account.external_username,
            token,
        }))
    }

    pub async fn enabled_destinations(
        &self,
        user_id: Uuid,
    ) -> Result<Vec<(Uuid, String)>, DestinationError> {
        let accounts = self
            .store
            .list_enabled(user_id)
            .await
            .map_err(DestinationError::Repository)?;

        Ok(accounts
            .into_iter()
            .map(|account| (account.id, account.provider_id))
            .collect())
    }

    fn credential_key(&self) -> Result<&CredentialKey, DestinationError> {
        self.key.as_ref().ok_or(DestinationError::Disabled)
    }
}

fn validation_failure(error: ValidationError) -> DestinationError {
    match error {
        ValidationError::InvalidCredential => DestinationError::InvalidCredential,
        ValidationError::Unavailable => DestinationError::Unavailable,
    }
}

fn destination_status(account: &DestinationAccount) -> DestinationStatus {
    DestinationStatus {
        provider_id: account.provider_id.clone(),
        username: account.external_username.clone(),
        enabled: account.enabled,
        connected_at: account.created_at,
        last_verified_at: account.last_verified_at,
        revoked: account.revoked_at.is_some(),
    }
}

fn timestamp<S: serde::Serializer>(
    value: &DateTime<Utc>,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    serializer.serialize_str(&value.to_rfc3339())
}

fn optional_timestamp<S: serde::Serializer>(
    value: &Option<DateTime<Utc>>,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    match value {
        Some(value) => serializer.serialize_some(&value.to_rfc3339()),
        None => serializer.serialize_none(),
    }
}
