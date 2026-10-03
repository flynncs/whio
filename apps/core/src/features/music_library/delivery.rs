use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Serialize, Serializer};
use sqlx::PgConnection;
use uuid::Uuid;

use crate::features::identity::destinations::DestinationCredential;
use crate::features::music_library::domain::{Album, ArtistCredit, ListenSubmission, Track};
use crate::features::music_library::storage::contracts::{DeliveryStore, TrackRepositoryError};
use crate::jobs::listening_delivery::{
    LISTEN_DELIVERY_MAX_ATTEMPTS, LISTEN_DELIVERY_PRIORITY, LISTEN_DELIVERY_QUEUE,
    ListeningDeliveryV1,
};
use crate::platform::jobs::{JobsError, PendingJob, enqueue};

pub const LISTEN_MINIMUM_TS: i64 = 1_033_430_400;
pub const PROVIDER_IDEMPOTENCE: &str = "timestamp_only";
pub const SUBMISSION_CLIENT: &str = concat!("whio/", env!("CARGO_PKG_VERSION"));
pub const RATE_LIMIT_RETRY_AFTER: Duration = Duration::from_secs(60);
pub const SNOOZE_DEADLINE: Duration = Duration::from_secs(3_600);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListenIdentity {
    pub user_id: Uuid,
    pub origin: String,
    pub track_id: Uuid,
    pub listen_at: DateTime<Utc>,
    pub session_key: String,
}

impl ListenIdentity {
    pub fn from_submission(submission: &ListenSubmission) -> Self {
        Self {
            user_id: submission.principal(),
            origin: submission.origin().as_str().to_owned(),
            track_id: *submission.track_id().as_uuid(),
            listen_at: submission.listen_at(),
            session_key: submission
                .session_id()
                .map(|session| session.as_str().to_owned())
                .unwrap_or_default(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum DeliveryState {
    Pending,
    Delivered,
    Ignored,
    Rejected,
    Exhausted,
}

impl DeliveryState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Delivered => "delivered",
            Self::Ignored => "ignored",
            Self::Rejected => "rejected",
            Self::Exhausted => "exhausted",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "pending" => Some(Self::Pending),
            "delivered" => Some(Self::Delivered),
            "ignored" => Some(Self::Ignored),
            "rejected" => Some(Self::Rejected),
            "exhausted" => Some(Self::Exhausted),
            _ => None,
        }
    }

    pub fn is_terminal(self) -> bool {
        !matches!(self, Self::Pending)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeliveryOutcome {
    Delivered,
    Ignored,
    Retryable,
    Rejected,
    AlreadyTerminal,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeliveryFailure {
    pub class: &'static str,
    pub message: String,
}

impl DeliveryFailure {
    pub fn new(class: &'static str, message: impl Into<String>) -> Self {
        Self {
            class,
            message: message.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeliveryRow {
    pub id: Uuid,
    pub user_id: Uuid,
    pub origin: String,
    pub track_id: Uuid,
    pub listen_at: DateTime<Utc>,
    pub session_key: String,
    pub destination_id: Uuid,
    pub provider_id: String,
    pub state: DeliveryState,
    pub attempts: i32,
    pub provider_idempotence: String,
    pub provider_reference: Option<String>,
    pub last_error: Option<String>,
    pub last_error_class: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl DeliveryRow {
    pub fn pending(listen: &ListenIdentity, destination_id: Uuid, provider_id: String) -> Self {
        let now = Utc::now();

        Self {
            id: Uuid::now_v7(),
            user_id: listen.user_id,
            origin: listen.origin.clone(),
            track_id: listen.track_id,
            listen_at: listen.listen_at,
            session_key: listen.session_key.clone(),
            destination_id,
            provider_id,
            state: DeliveryState::Pending,
            attempts: 0,
            provider_idempotence: PROVIDER_IDEMPOTENCE.to_owned(),
            provider_reference: None,
            last_error: None,
            last_error_class: None,
            created_at: now,
            updated_at: now,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DeliveryStatus {
    pub destination_id: Uuid,
    pub provider_id: String,
    pub state: DeliveryState,
    pub attempts: i32,
    pub provider_idempotence: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    #[serde(serialize_with = "timestamp")]
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, thiserror::Error)]
pub enum DeliveryError {
    #[error("the listen destination is disabled")]
    Disabled,

    #[error("listen delivery is not configured")]
    Unavailable,

    #[error("the accepted listen cannot be delivered as stored")]
    InvalidData,

    #[error("the destination credential is not configured")]
    CredentialMissing,

    #[error("the destination rejected the credential")]
    CredentialRejected,

    #[error("the listen provider is unavailable")]
    ProviderUnavailable,

    #[error("the listen provider rate limited the submission")]
    RateLimited { retry_after: Option<Duration> },

    #[error("listen delivery storage failed")]
    Repository(#[source] TrackRepositoryError),
}

impl From<JobsError> for DeliveryError {
    fn from(_: JobsError) -> Self {
        Self::Unavailable
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ScrobbleListen {
    pub listened_at: i64,
    pub artist_name: String,
    pub track_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recording_mbid: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub release_mbid: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub release_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SubmissionOutcome {
    Accepted,
    Filtered { message: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SubmissionError {
    CredentialRejected,
    Rejected { message: String },
    RateLimited { retry_after: Option<Duration> },
    Unavailable,
}

#[async_trait]
pub trait ListenSender: Send + Sync {
    async fn submit(
        &self,
        credential: &str,
        listen: &ScrobbleListen,
    ) -> Result<SubmissionOutcome, SubmissionError>;
}

#[async_trait]
pub trait DeliveryDestinations: Send + Sync {
    async fn enabled_destinations(
        &self,
        user_id: Uuid,
    ) -> Result<Vec<(Uuid, String)>, DeliveryError>;

    async fn enabled_credential(
        &self,
        user_id: Uuid,
        provider_id: &str,
    ) -> Result<Option<DestinationCredential>, DeliveryError>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListenPayload {
    pub track_name: String,
    pub artist_name: String,
    pub recording_mbid: Option<String>,
    pub release_mbid: Option<String>,
    pub release_name: Option<String>,
    pub duration_ms: Option<u64>,
}

impl ListenPayload {
    pub fn from_track(track: &Track, album: Option<&Album>) -> Result<Self, DeliveryError> {
        let artist_name = track
            .artists()
            .iter()
            .map(ArtistCredit::name)
            .collect::<Vec<_>>()
            .join(", ");

        if artist_name.is_empty() || track.title().is_empty() {
            return Err(DeliveryError::InvalidData);
        }

        Ok(Self {
            track_name: track.title().to_owned(),
            artist_name,
            recording_mbid: track
                .musicbrainz_recording_id()
                .filter(|value| !value.is_empty())
                .map(str::to_owned),
            release_mbid: album
                .and_then(Album::musicbrainz_release_id)
                .filter(|value| !value.is_empty())
                .map(str::to_owned),
            release_name: album
                .map(Album::title)
                .filter(|title| !title.is_empty())
                .map(str::to_owned),
            duration_ms: track.duration_ms().filter(|duration| *duration > 0),
        })
    }

    pub fn scrobble(&self, listened_at: i64, client: &str) -> ScrobbleListen {
        ScrobbleListen {
            listened_at,
            artist_name: self.artist_name.clone(),
            track_name: self.track_name.clone(),
            recording_mbid: self.recording_mbid.clone(),
            release_mbid: self.release_mbid.clone(),
            release_name: self.release_name.clone(),
            duration_ms: self.duration_ms,
            client: Some(client.to_owned()),
        }
    }
}

#[async_trait]
pub trait ListenSource: Send + Sync {
    async fn listen_payload(&self, track_id: Uuid) -> Result<Option<ListenPayload>, DeliveryError>;
}

pub struct DeliveryService {
    store: Arc<dyn DeliveryStore>,
    source: Arc<dyn ListenSource>,
    sender: Arc<dyn ListenSender>,
    destinations: Arc<dyn DeliveryDestinations>,
}

impl DeliveryService {
    pub fn new(
        store: Arc<dyn DeliveryStore>,
        source: Arc<dyn ListenSource>,
        sender: Arc<dyn ListenSender>,
        destinations: Arc<dyn DeliveryDestinations>,
    ) -> Self {
        Self {
            store,
            source,
            sender,
            destinations,
        }
    }

    pub async fn enabled_destinations(
        &self,
        user_id: Uuid,
    ) -> Result<Vec<(Uuid, String)>, DeliveryError> {
        self.destinations.enabled_destinations(user_id).await
    }

    pub async fn record_accepted(
        &self,
        tx: &mut PgConnection,
        listen: &ListenIdentity,
        destinations: &[(Uuid, String)],
    ) -> Result<usize, DeliveryError> {
        let mut recorded = 0;
        for (destination_id, provider_id) in destinations {
            let row = DeliveryRow::pending(listen, *destination_id, provider_id.clone());
            self.store
                .insert_pending(&mut *tx, &row)
                .await
                .map_err(DeliveryError::Repository)?;

            let payload = ListeningDeliveryV1::for_delivery(listen, *destination_id, row.id);
            enqueue(
                &mut *tx,
                PendingJob {
                    queue: LISTEN_DELIVERY_QUEUE,
                    payload: &payload,
                    priority: LISTEN_DELIVERY_PRIORITY,
                    max_attempts: LISTEN_DELIVERY_MAX_ATTEMPTS,
                    operation_id: payload.operation_id,
                    coalesce: false,
                },
            )
            .await?;

            recorded += 1;
        }

        Ok(recorded)
    }

    pub async fn deliver(&self, delivery_id: Uuid) -> Result<DeliveryOutcome, DeliveryError> {
        let row = self
            .store
            .find(delivery_id)
            .await
            .map_err(DeliveryError::Repository)?
            .ok_or(DeliveryError::InvalidData)?;

        if row.state.is_terminal() {
            return Ok(DeliveryOutcome::AlreadyTerminal);
        }

        let Some(payload) = self.source.listen_payload(row.track_id).await? else {
            return self
                .reject(&row, "track_missing", "the listened track no longer exists")
                .await;
        };

        let listened_at = row.listen_at.timestamp();
        if listened_at < LISTEN_MINIMUM_TS {
            return self
                .reject(
                    &row,
                    "before_provider_minimum",
                    "the accepted listen predates the provider minimum timestamp",
                )
                .await;
        }

        let credential = match self
            .destinations
            .enabled_credential(row.user_id, &row.provider_id)
            .await
        {
            Ok(Some(credential)) => credential,
            Ok(None) => return Err(DeliveryError::Disabled),
            Err(DeliveryError::Disabled) => return Err(DeliveryError::CredentialMissing),
            Err(error) => return Err(error),
        };

        if credential.destination_id != row.destination_id {
            return Err(DeliveryError::Disabled);
        }

        let listen = payload.scrobble(listened_at, SUBMISSION_CLIENT);

        match self.sender.submit(&credential.token, &listen).await {
            Ok(SubmissionOutcome::Accepted) => {
                if !self
                    .transition(&row, DeliveryState::Delivered, row.attempts + 1, None, None)
                    .await?
                {
                    return Ok(DeliveryOutcome::AlreadyTerminal);
                }

                Ok(DeliveryOutcome::Delivered)
            }
            Ok(SubmissionOutcome::Filtered { message }) => {
                let failure = DeliveryFailure::new("filtered", message);
                if !self
                    .transition(
                        &row,
                        DeliveryState::Ignored,
                        row.attempts + 1,
                        None,
                        Some(&failure),
                    )
                    .await?
                {
                    return Ok(DeliveryOutcome::AlreadyTerminal);
                }

                Ok(DeliveryOutcome::Ignored)
            }
            Err(SubmissionError::CredentialRejected) => {
                let failure = DeliveryFailure::new(
                    "credential_rejected",
                    "the destination rejected the stored credential",
                );
                self.transition(
                    &row,
                    DeliveryState::Rejected,
                    row.attempts + 1,
                    None,
                    Some(&failure),
                )
                .await?;

                Err(DeliveryError::CredentialRejected)
            }
            Err(SubmissionError::Rejected { message }) => {
                let failure = DeliveryFailure::new("request_rejected", message);
                self.transition(
                    &row,
                    DeliveryState::Rejected,
                    row.attempts + 1,
                    None,
                    Some(&failure),
                )
                .await?;

                Err(DeliveryError::InvalidData)
            }
            Err(SubmissionError::RateLimited { retry_after }) => {
                self.rate_limited(&row, retry_after).await
            }
            Err(SubmissionError::Unavailable) => self.unavailable(&row).await,
        }
    }

    pub async fn status(
        &self,
        user_id: Uuid,
        state: Option<DeliveryState>,
        limit: u32,
    ) -> Result<Vec<DeliveryStatus>, DeliveryError> {
        self.store
            .list_for_user(user_id, state, limit)
            .await
            .map_err(DeliveryError::Repository)
    }

    async fn reject(
        &self,
        row: &DeliveryRow,
        class: &'static str,
        message: &'static str,
    ) -> Result<DeliveryOutcome, DeliveryError> {
        let failure = DeliveryFailure::new(class, message);
        self.transition(
            row,
            DeliveryState::Rejected,
            row.attempts,
            None,
            Some(&failure),
        )
        .await?;

        Ok(DeliveryOutcome::Rejected)
    }

    async fn rate_limited(
        &self,
        row: &DeliveryRow,
        retry_after: Option<Duration>,
    ) -> Result<DeliveryOutcome, DeliveryError> {
        let delay = retry_after.unwrap_or(RATE_LIMIT_RETRY_AFTER);
        let elapsed = Utc::now().signed_duration_since(row.created_at);
        let deadline =
            chrono::Duration::from_std(SNOOZE_DEADLINE).map_err(|_| DeliveryError::InvalidData)?;

        if elapsed >= deadline {
            let failure = DeliveryFailure::new(
                "rate_limited_deadline",
                "the provider rate limit outlived the delivery deadline",
            );
            self.transition(
                row,
                DeliveryState::Exhausted,
                row.attempts,
                None,
                Some(&failure),
            )
            .await?;

            return Ok(DeliveryOutcome::Rejected);
        }

        let failure = DeliveryFailure::new(
            "rate_limited",
            format!(
                "the provider rate limited the submission for {}s",
                delay.as_secs()
            ),
        );
        self.transition(
            row,
            DeliveryState::Pending,
            row.attempts,
            None,
            Some(&failure),
        )
        .await?;

        Err(DeliveryError::RateLimited {
            retry_after: Some(delay),
        })
    }

    async fn unavailable(&self, row: &DeliveryRow) -> Result<DeliveryOutcome, DeliveryError> {
        let attempts = row.attempts + 1;
        let failure =
            DeliveryFailure::new("provider_unavailable", "the listen provider did not answer");

        if attempts >= i32::from(LISTEN_DELIVERY_MAX_ATTEMPTS) {
            self.transition(
                row,
                DeliveryState::Exhausted,
                attempts,
                None,
                Some(&failure),
            )
            .await?;

            return Ok(DeliveryOutcome::Rejected);
        }

        self.transition(row, DeliveryState::Pending, attempts, None, Some(&failure))
            .await?;

        Ok(DeliveryOutcome::Retryable)
    }

    async fn transition(
        &self,
        row: &DeliveryRow,
        to: DeliveryState,
        attempts: i32,
        provider_reference: Option<&str>,
        failure: Option<&DeliveryFailure>,
    ) -> Result<bool, DeliveryError> {
        self.store
            .transition(row.id, row.state, to, attempts, provider_reference, failure)
            .await
            .map_err(DeliveryError::Repository)
    }
}

fn timestamp<S: Serializer>(value: &DateTime<Utc>, serializer: S) -> Result<S::Ok, S::Error> {
    serializer.serialize_str(&value.to_rfc3339())
}
