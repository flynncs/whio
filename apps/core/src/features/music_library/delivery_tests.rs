use std::sync::Mutex;

use async_trait::async_trait;
use chrono::{Duration as ChronoDuration, Utc};
use serde_json::json;
use sqlx::PgConnection;
use uuid::Uuid;

use crate::features::identity::destinations::{DestinationCredential, DestinationProvider};
use crate::features::music_library::delivery::{
    DeliveryDestinations, DeliveryError, DeliveryFailure, DeliveryOutcome, DeliveryRow,
    DeliveryService, DeliveryState, DeliveryStatus, LISTEN_MINIMUM_TS, ListenIdentity,
    ListenPayload, ListenSender, ListenSource, PROVIDER_IDEMPOTENCE, SUBMISSION_CLIENT,
    ScrobbleListen, SubmissionError, SubmissionOutcome,
};
use crate::features::music_library::domain::{
    Album, AlbumId, ArtistCredit, ListenOrigin, ListenSessionId, ListenSubmission, Track, TrackId,
    TrackMetadata,
};
use crate::features::music_library::storage::contracts::{DeliveryStore, TrackRepositoryError};

fn track(title: &str, credits: Vec<ArtistCredit>, recording_mbid: Option<&str>) -> Track {
    let metadata = TrackMetadata::new(title.to_owned(), credits, Some(240_000))
        .unwrap()
        .with_recording_id(recording_mbid.map(str::to_owned))
        .unwrap();

    Track::new(TrackId::new(), metadata)
}

fn credited(name: &str, mbid: Option<&str>) -> ArtistCredit {
    ArtistCredit::new(name.to_owned(), mbid.map(str::to_owned)).unwrap()
}

fn album(title: &str, release_mbid: Option<&str>) -> Album {
    Album::new(
        AlbumId::new(),
        title.to_owned(),
        None,
        None,
        None,
        None,
        None,
        release_mbid.map(str::to_owned),
    )
    .unwrap()
}

fn submission(principal: Uuid, listen_at: chrono::DateTime<Utc>) -> ListenSubmission {
    ListenSubmission::new(
        principal,
        ListenOrigin::Native,
        TrackId::new(),
        listen_at,
        Some(ListenSessionId::new("session-a".to_owned()).unwrap()),
        None,
    )
}

fn row(listen: &ListenIdentity, destination_id: Uuid, state: DeliveryState) -> DeliveryRow {
    let mut row = DeliveryRow::pending(listen, destination_id, "listenbrainz".to_owned());
    row.state = state;

    row
}

#[derive(Default)]
struct FakeStore {
    rows: Mutex<Vec<DeliveryRow>>,
}

impl FakeStore {
    fn seed(&self, row: DeliveryRow) {
        self.rows.lock().unwrap().push(row);
    }

    fn row(&self, delivery_id: Uuid) -> DeliveryRow {
        self.rows
            .lock()
            .unwrap()
            .iter()
            .find(|row| row.id == delivery_id)
            .cloned()
            .expect("the seeded delivery row")
    }
}

#[async_trait]
impl DeliveryStore for FakeStore {
    async fn insert_pending(
        &self,
        _tx: &mut PgConnection,
        row: &DeliveryRow,
    ) -> Result<(), TrackRepositoryError> {
        self.seed(row.clone());

        Ok(())
    }

    async fn find(&self, delivery_id: Uuid) -> Result<Option<DeliveryRow>, TrackRepositoryError> {
        Ok(self
            .rows
            .lock()
            .unwrap()
            .iter()
            .find(|row| row.id == delivery_id)
            .cloned())
    }

    async fn transition(
        &self,
        delivery_id: Uuid,
        from: DeliveryState,
        to: DeliveryState,
        attempts: i32,
        provider_reference: Option<&str>,
        failure: Option<&DeliveryFailure>,
    ) -> Result<bool, TrackRepositoryError> {
        let mut rows = self.rows.lock().unwrap();
        let Some(row) = rows
            .iter_mut()
            .find(|row| row.id == delivery_id && row.state == from)
        else {
            return Ok(false);
        };

        row.state = to;
        row.attempts = attempts;
        row.provider_reference = provider_reference.map(str::to_owned);
        row.last_error = failure.map(|failure| failure.message.clone());
        row.last_error_class = failure.map(|failure| failure.class.to_owned());
        row.updated_at = Utc::now();

        Ok(true)
    }

    async fn list_for_user(
        &self,
        user_id: Uuid,
        state: Option<DeliveryState>,
        limit: u32,
    ) -> Result<Vec<DeliveryStatus>, TrackRepositoryError> {
        Ok(self
            .rows
            .lock()
            .unwrap()
            .iter()
            .filter(|row| row.user_id == user_id)
            .filter(|row| state.is_none_or(|state| row.state == state))
            .take(limit as usize)
            .map(|row| DeliveryStatus {
                destination_id: row.destination_id,
                provider_id: row.provider_id.clone(),
                state: row.state,
                attempts: row.attempts,
                provider_idempotence: row.provider_idempotence.clone(),
                last_error: row.last_error.clone(),
                updated_at: row.updated_at,
            })
            .collect())
    }
}

struct FakeSource {
    payload: Option<ListenPayload>,
}

#[async_trait]
impl ListenSource for FakeSource {
    async fn listen_payload(
        &self,
        _track_id: Uuid,
    ) -> Result<Option<ListenPayload>, DeliveryError> {
        Ok(self.payload.clone())
    }
}

#[derive(Default)]
struct FakeSender {
    calls: Mutex<Vec<(String, ScrobbleListen)>>,
    answer: Mutex<Option<Result<SubmissionOutcome, SubmissionError>>>,
}

impl FakeSender {
    fn answering(answer: Result<SubmissionOutcome, SubmissionError>) -> Self {
        Self {
            calls: Mutex::new(Vec::new()),
            answer: Mutex::new(Some(answer)),
        }
    }

    fn calls(&self) -> Vec<(String, ScrobbleListen)> {
        self.calls.lock().unwrap().clone()
    }
}

#[async_trait]
impl ListenSender for FakeSender {
    async fn submit(
        &self,
        credential: &str,
        listen: &ScrobbleListen,
    ) -> Result<SubmissionOutcome, SubmissionError> {
        self.calls
            .lock()
            .unwrap()
            .push((credential.to_owned(), listen.clone()));

        self.answer
            .lock()
            .unwrap()
            .clone()
            .expect("a queued sender answer")
    }
}

struct FakeDestinations {
    destinations: Vec<(Uuid, String)>,
    credential: Option<DestinationCredential>,
    missing_key: bool,
}

impl FakeDestinations {
    fn connected(destination_id: Uuid, token: &str) -> Self {
        Self {
            destinations: vec![(destination_id, "listenbrainz".to_owned())],
            credential: Some(DestinationCredential {
                destination_id,
                provider_id: DestinationProvider::ListenBrainz,
                username: Some("flynn".to_owned()),
                token: token.to_owned(),
            }),
            missing_key: false,
        }
    }

    fn disconnected() -> Self {
        Self {
            destinations: Vec::new(),
            credential: None,
            missing_key: false,
        }
    }
}

#[async_trait]
impl DeliveryDestinations for FakeDestinations {
    async fn enabled_destinations(
        &self,
        _user_id: Uuid,
    ) -> Result<Vec<(Uuid, String)>, DeliveryError> {
        Ok(self.destinations.clone())
    }

    async fn enabled_credential(
        &self,
        _user_id: Uuid,
        _provider_id: &str,
    ) -> Result<Option<DestinationCredential>, DeliveryError> {
        if self.missing_key {
            return Err(DeliveryError::Disabled);
        }

        Ok(self
            .credential
            .as_ref()
            .map(|credential| DestinationCredential {
                destination_id: credential.destination_id,
                provider_id: credential.provider_id,
                username: credential.username.clone(),
                token: credential.token.clone(),
            }))
    }
}

fn payload(recording_mbid: Option<&str>, release: bool) -> ListenPayload {
    let track = track(
        "Blue Monday",
        vec![credited(
            "New Order",
            Some("f1a1e1d1-0000-0000-0000-000000000001"),
        )],
        recording_mbid,
    );
    let album = release.then(|| album("Power, Corruption & Lies", Some("aaaa-bbbb")));

    ListenPayload::from_track(&track, album.as_ref()).unwrap()
}

fn service(
    store: std::sync::Arc<FakeStore>,
    payload: Option<ListenPayload>,
    sender: std::sync::Arc<FakeSender>,
    destinations: FakeDestinations,
) -> DeliveryService {
    DeliveryService::new(
        store,
        std::sync::Arc::new(FakeSource { payload }),
        sender,
        std::sync::Arc::new(destinations),
    )
}

#[test]
fn the_payload_carries_only_the_contract_members() {
    let listen = payload(Some("recording-1"), true).scrobble(1_700_000_000, SUBMISSION_CLIENT);

    assert_eq!(
        serde_json::to_value(&listen).unwrap(),
        json!({
            "listened_at": 1_700_000_000i64,
            "artist_name": "New Order",
            "track_name": "Blue Monday",
            "recording_mbid": "recording-1",
            "release_mbid": "aaaa-bbbb",
            "release_name": "Power, Corruption & Lies",
            "duration_ms": 240_000u64,
            "client": SUBMISSION_CLIENT,
        })
    );
}

#[test]
fn a_track_without_a_recording_mbid_yields_a_name_only_payload() {
    let listen = payload(None, false).scrobble(1_700_000_000, SUBMISSION_CLIENT);

    assert_eq!(
        serde_json::to_value(&listen).unwrap(),
        json!({
            "listened_at": 1_700_000_000i64,
            "artist_name": "New Order",
            "track_name": "Blue Monday",
            "duration_ms": 240_000u64,
            "client": SUBMISSION_CLIENT,
        })
    );
}

#[test]
fn an_absent_member_is_absent_rather_than_empty() {
    let value = serde_json::to_value(payload(None, false).scrobble(1, SUBMISSION_CLIENT)).unwrap();
    let members = value.as_object().unwrap();

    for absent in ["recording_mbid", "release_mbid", "release_name"] {
        assert!(!members.contains_key(absent), "{absent} was defaulted");
    }

    assert!(
        members["artist_name"]
            .as_str()
            .is_some_and(|name| !name.is_empty())
    );
    assert!(
        members["track_name"]
            .as_str()
            .is_some_and(|name| !name.is_empty())
    );
}

#[test]
fn a_zero_duration_is_absent_rather_than_zero() {
    let track = Track::new(
        TrackId::new(),
        TrackMetadata::new(
            "Blue Monday".to_owned(),
            vec![credited("New Order", None)],
            Some(0),
        )
        .unwrap(),
    );

    let payload = ListenPayload::from_track(&track, None).unwrap();
    let value = serde_json::to_value(payload.scrobble(1_700_000_000, SUBMISSION_CLIENT)).unwrap();

    assert!(payload.duration_ms.is_none());
    assert!(
        !value.as_object().unwrap().contains_key("duration_ms"),
        "a zero duration must be omitted, not sent as 0"
    );
}

#[test]
fn primary_credits_join_in_stored_order() {
    let track = track(
        "Ceremony",
        vec![
            credited("New Order", None),
            credited("Joy Division", Some("artist-2")),
        ],
        None,
    );

    let payload = ListenPayload::from_track(&track, None).unwrap();

    assert_eq!(payload.artist_name, "New Order, Joy Division");
    assert!(payload.recording_mbid.is_none());
}

#[test]
fn a_track_without_credited_artists_is_invalid() {
    let track = track("Untitled", Vec::new(), None);

    assert!(matches!(
        ListenPayload::from_track(&track, None),
        Err(DeliveryError::InvalidData)
    ));
}

#[test]
fn the_listen_identity_copies_the_ledger_key() {
    let principal = Uuid::now_v7();
    let listen_at = Utc::now();
    let submission = submission(principal, listen_at);

    let identity = ListenIdentity::from_submission(&submission);

    assert_eq!(identity.user_id, principal);
    assert_eq!(identity.origin, "native");
    assert_eq!(identity.track_id, *submission.track_id().as_uuid());
    assert_eq!(identity.listen_at, listen_at);
    assert_eq!(identity.session_key, "session-a");
}

#[test]
fn a_listen_without_a_session_uses_the_empty_session_key() {
    let submission = ListenSubmission::new(
        Uuid::now_v7(),
        ListenOrigin::SubsonicScrobble,
        TrackId::new(),
        Utc::now(),
        None,
        None,
    );

    assert_eq!(ListenIdentity::from_submission(&submission).session_key, "");
}

#[test]
fn a_new_row_records_the_provider_idempotence_limit() {
    let store = std::sync::Arc::new(FakeStore::default());
    let destinations = FakeDestinations::connected(Uuid::now_v7(), "token");
    let row = DeliveryRow::pending(
        &ListenIdentity::from_submission(&submission(Uuid::now_v7(), Utc::now())),
        Uuid::now_v7(),
        "listenbrainz".to_owned(),
    );

    assert_eq!(row.state, DeliveryState::Pending);
    assert_eq!(row.attempts, 0);
    assert_eq!(row.provider_idempotence, PROVIDER_IDEMPOTENCE);
    assert_eq!(row.provider_idempotence, "timestamp_only");
    assert!(store.rows.lock().unwrap().is_empty());
    assert_eq!(destinations.destinations.len(), 1);
}

#[tokio::test]
async fn a_delivered_listen_transitions_once() {
    let principal = Uuid::now_v7();
    let destination_id = Uuid::now_v7();
    let listen = ListenIdentity::from_submission(&submission(principal, Utc::now()));
    let seed = row(&listen, destination_id, DeliveryState::Pending);
    let store = std::sync::Arc::new(FakeStore::default());
    store.seed(seed.clone());
    let sender = std::sync::Arc::new(FakeSender::answering(Ok(SubmissionOutcome::Accepted)));
    let delivery = service(
        std::sync::Arc::clone(&store),
        Some(payload(Some("recording-1"), true)),
        std::sync::Arc::clone(&sender),
        FakeDestinations::connected(destination_id, "secret-token"),
    );

    assert_eq!(
        delivery.deliver(seed.id).await.unwrap(),
        DeliveryOutcome::Delivered
    );

    let stored = store.row(seed.id);
    assert_eq!(stored.state, DeliveryState::Delivered);
    assert_eq!(stored.attempts, 1);
    assert!(stored.last_error.is_none());

    let calls = sender.calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].0, "secret-token");
    assert_eq!(calls[0].1.listened_at, seed.listen_at.timestamp());
}

#[tokio::test]
async fn a_filtered_listen_is_ignored_with_the_provider_message() {
    let destination_id = Uuid::now_v7();
    let listen = ListenIdentity::from_submission(&submission(Uuid::now_v7(), Utc::now()));
    let seed = row(&listen, destination_id, DeliveryState::Pending);
    let store = std::sync::Arc::new(FakeStore::default());
    store.seed(seed.clone());
    let delivery = service(
        std::sync::Arc::clone(&store),
        Some(payload(None, false)),
        std::sync::Arc::new(FakeSender::answering(Ok(SubmissionOutcome::Filtered {
            message: "this listen was filtered".to_owned(),
        }))),
        FakeDestinations::connected(destination_id, "secret-token"),
    );

    assert_eq!(
        delivery.deliver(seed.id).await.unwrap(),
        DeliveryOutcome::Ignored
    );

    let stored = store.row(seed.id);
    assert_eq!(stored.state, DeliveryState::Ignored);
    assert_eq!(
        stored.last_error.as_deref(),
        Some("this listen was filtered")
    );
    assert_eq!(stored.last_error_class.as_deref(), Some("filtered"));
}

#[tokio::test]
async fn a_provider_outage_is_retried_until_the_budget_is_spent() {
    let destination_id = Uuid::now_v7();
    let listen = ListenIdentity::from_submission(&submission(Uuid::now_v7(), Utc::now()));
    let seed = row(&listen, destination_id, DeliveryState::Pending);
    let store = std::sync::Arc::new(FakeStore::default());
    store.seed(seed.clone());
    let delivery = service(
        std::sync::Arc::clone(&store),
        Some(payload(None, false)),
        std::sync::Arc::new(FakeSender::answering(Err(SubmissionError::Unavailable))),
        FakeDestinations::connected(destination_id, "secret-token"),
    );

    assert_eq!(
        delivery.deliver(seed.id).await.unwrap(),
        DeliveryOutcome::Retryable
    );

    let stored = store.row(seed.id);
    assert_eq!(stored.state, DeliveryState::Pending);
    assert_eq!(stored.attempts, 1);
    assert_eq!(
        stored.last_error_class.as_deref(),
        Some("provider_unavailable")
    );
}

#[tokio::test]
async fn a_spent_attempt_budget_exhausts_the_row_terminally() {
    let destination_id = Uuid::now_v7();
    let listen = ListenIdentity::from_submission(&submission(Uuid::now_v7(), Utc::now()));
    let mut seed = row(&listen, destination_id, DeliveryState::Pending);
    seed.attempts = 7;
    let store = std::sync::Arc::new(FakeStore::default());
    store.seed(seed.clone());
    let delivery = service(
        std::sync::Arc::clone(&store),
        Some(payload(None, false)),
        std::sync::Arc::new(FakeSender::answering(Err(SubmissionError::Unavailable))),
        FakeDestinations::connected(destination_id, "secret-token"),
    );

    assert_eq!(
        delivery.deliver(seed.id).await.unwrap(),
        DeliveryOutcome::Rejected
    );

    let stored = store.row(seed.id);
    assert_eq!(stored.state, DeliveryState::Exhausted);
    assert_eq!(stored.attempts, 8);
}

#[tokio::test]
async fn a_rate_limit_snoozes_without_consuming_an_attempt() {
    let destination_id = Uuid::now_v7();
    let listen = ListenIdentity::from_submission(&submission(Uuid::now_v7(), Utc::now()));
    let mut seed = row(&listen, destination_id, DeliveryState::Pending);
    seed.attempts = 3;
    let store = std::sync::Arc::new(FakeStore::default());
    store.seed(seed.clone());
    let sender = std::sync::Arc::new(FakeSender::answering(Err(SubmissionError::RateLimited {
        retry_after: Some(std::time::Duration::from_secs(30)),
    })));
    let delivery = service(
        std::sync::Arc::clone(&store),
        Some(payload(None, false)),
        std::sync::Arc::clone(&sender),
        FakeDestinations::connected(destination_id, "secret-token"),
    );

    match delivery.deliver(seed.id).await {
        Err(DeliveryError::RateLimited { retry_after }) => {
            assert_eq!(retry_after, Some(std::time::Duration::from_secs(30)));
        }
        other => panic!("expected a rate limit, got {other:?}"),
    }

    let stored = store.row(seed.id);
    assert_eq!(stored.state, DeliveryState::Pending);
    assert_eq!(stored.attempts, 3);
    assert_eq!(stored.last_error_class.as_deref(), Some("rate_limited"));
    assert_eq!(sender.calls().len(), 1);
}

#[tokio::test]
async fn a_rate_limit_past_the_deadline_terminates_the_loop() {
    let destination_id = Uuid::now_v7();
    let listen = ListenIdentity::from_submission(&submission(Uuid::now_v7(), Utc::now()));
    let mut seed = row(&listen, destination_id, DeliveryState::Pending);
    seed.created_at = Utc::now() - ChronoDuration::seconds(7_200);
    let store = std::sync::Arc::new(FakeStore::default());
    store.seed(seed.clone());
    let delivery = service(
        std::sync::Arc::clone(&store),
        Some(payload(None, false)),
        std::sync::Arc::new(FakeSender::answering(Err(SubmissionError::RateLimited {
            retry_after: None,
        }))),
        FakeDestinations::connected(destination_id, "secret-token"),
    );

    assert_eq!(
        delivery.deliver(seed.id).await.unwrap(),
        DeliveryOutcome::Rejected
    );

    let stored = store.row(seed.id);
    assert_eq!(stored.state, DeliveryState::Exhausted);
    assert_eq!(
        stored.last_error_class.as_deref(),
        Some("rate_limited_deadline")
    );
}

#[tokio::test]
async fn a_rejected_credential_is_terminal_and_records_its_class() {
    let destination_id = Uuid::now_v7();
    let listen = ListenIdentity::from_submission(&submission(Uuid::now_v7(), Utc::now()));
    let seed = row(&listen, destination_id, DeliveryState::Pending);
    let store = std::sync::Arc::new(FakeStore::default());
    store.seed(seed.clone());
    let delivery = service(
        std::sync::Arc::clone(&store),
        Some(payload(None, false)),
        std::sync::Arc::new(FakeSender::answering(Err(
            SubmissionError::CredentialRejected,
        ))),
        FakeDestinations::connected(destination_id, "secret-token"),
    );

    assert!(matches!(
        delivery.deliver(seed.id).await,
        Err(DeliveryError::CredentialRejected)
    ));

    let stored = store.row(seed.id);
    assert_eq!(stored.state, DeliveryState::Rejected);
    assert_eq!(
        stored.last_error_class.as_deref(),
        Some("credential_rejected")
    );
    assert_eq!(
        stored.last_error.as_deref(),
        Some("the destination rejected the stored credential")
    );
}

#[tokio::test]
async fn a_permanent_request_rejection_is_terminal() {
    let destination_id = Uuid::now_v7();
    let listen = ListenIdentity::from_submission(&submission(Uuid::now_v7(), Utc::now()));
    let seed = row(&listen, destination_id, DeliveryState::Pending);
    let store = std::sync::Arc::new(FakeStore::default());
    store.seed(seed.clone());
    let delivery = service(
        std::sync::Arc::clone(&store),
        Some(payload(None, false)),
        std::sync::Arc::new(FakeSender::answering(Err(SubmissionError::Rejected {
            message: "invalid json".to_owned(),
        }))),
        FakeDestinations::connected(destination_id, "secret-token"),
    );

    assert!(matches!(
        delivery.deliver(seed.id).await,
        Err(DeliveryError::InvalidData)
    ));

    let stored = store.row(seed.id);
    assert_eq!(stored.state, DeliveryState::Rejected);
    assert_eq!(stored.last_error_class.as_deref(), Some("request_rejected"));
    assert_eq!(stored.last_error.as_deref(), Some("invalid json"));
}

#[tokio::test]
async fn a_terminal_row_is_never_delivered_twice() {
    let destination_id = Uuid::now_v7();
    let listen = ListenIdentity::from_submission(&submission(Uuid::now_v7(), Utc::now()));
    let mut seed = row(&listen, destination_id, DeliveryState::Delivered);
    seed.attempts = 1;
    let store = std::sync::Arc::new(FakeStore::default());
    store.seed(seed.clone());
    let sender = std::sync::Arc::new(FakeSender::answering(Ok(SubmissionOutcome::Accepted)));
    let delivery = service(
        std::sync::Arc::clone(&store),
        Some(payload(None, false)),
        std::sync::Arc::clone(&sender),
        FakeDestinations::connected(destination_id, "secret-token"),
    );

    assert_eq!(
        delivery.deliver(seed.id).await.unwrap(),
        DeliveryOutcome::AlreadyTerminal
    );
    assert!(sender.calls().is_empty());
    assert_eq!(store.row(seed.id).attempts, 1);
}

#[tokio::test]
async fn a_disabled_destination_submits_nothing_and_keeps_the_row_pending() {
    let destination_id = Uuid::now_v7();
    let listen = ListenIdentity::from_submission(&submission(Uuid::now_v7(), Utc::now()));
    let seed = row(&listen, destination_id, DeliveryState::Pending);
    let store = std::sync::Arc::new(FakeStore::default());
    store.seed(seed.clone());
    let sender = std::sync::Arc::new(FakeSender::answering(Ok(SubmissionOutcome::Accepted)));
    let delivery = service(
        std::sync::Arc::clone(&store),
        Some(payload(None, false)),
        std::sync::Arc::clone(&sender),
        FakeDestinations::disconnected(),
    );

    assert!(matches!(
        delivery.deliver(seed.id).await,
        Err(DeliveryError::Disabled)
    ));

    let stored = store.row(seed.id);
    assert_eq!(stored.state, DeliveryState::Pending);
    assert_eq!(stored.attempts, 0);
    assert!(sender.calls().is_empty());
}

#[tokio::test]
async fn a_reconnected_destination_does_not_receive_an_old_row() {
    let destination_id = Uuid::now_v7();
    let listen = ListenIdentity::from_submission(&submission(Uuid::now_v7(), Utc::now()));
    let seed = row(&listen, destination_id, DeliveryState::Pending);
    let store = std::sync::Arc::new(FakeStore::default());
    store.seed(seed.clone());
    let sender = std::sync::Arc::new(FakeSender::answering(Ok(SubmissionOutcome::Accepted)));
    let delivery = service(
        std::sync::Arc::clone(&store),
        Some(payload(None, false)),
        std::sync::Arc::clone(&sender),
        FakeDestinations::connected(Uuid::now_v7(), "secret-token"),
    );

    assert!(matches!(
        delivery.deliver(seed.id).await,
        Err(DeliveryError::Disabled)
    ));
    assert!(sender.calls().is_empty());
    assert_eq!(store.row(seed.id).state, DeliveryState::Pending);
}

#[tokio::test]
async fn an_unconfigured_credential_store_is_reported() {
    let destination_id = Uuid::now_v7();
    let listen = ListenIdentity::from_submission(&submission(Uuid::now_v7(), Utc::now()));
    let seed = row(&listen, destination_id, DeliveryState::Pending);
    let store = std::sync::Arc::new(FakeStore::default());
    store.seed(seed.clone());
    let delivery = service(
        std::sync::Arc::clone(&store),
        Some(payload(None, false)),
        std::sync::Arc::new(FakeSender::answering(Ok(SubmissionOutcome::Accepted))),
        FakeDestinations {
            destinations: Vec::new(),
            credential: None,
            missing_key: true,
        },
    );

    assert!(matches!(
        delivery.deliver(seed.id).await,
        Err(DeliveryError::CredentialMissing)
    ));
    assert_eq!(store.row(seed.id).state, DeliveryState::Pending);
}

#[tokio::test]
async fn a_vanished_track_is_rejected_terminally() {
    let destination_id = Uuid::now_v7();
    let listen = ListenIdentity::from_submission(&submission(Uuid::now_v7(), Utc::now()));
    let seed = row(&listen, destination_id, DeliveryState::Pending);
    let store = std::sync::Arc::new(FakeStore::default());
    store.seed(seed.clone());
    let sender = std::sync::Arc::new(FakeSender::answering(Ok(SubmissionOutcome::Accepted)));
    let delivery = service(
        std::sync::Arc::clone(&store),
        None,
        std::sync::Arc::clone(&sender),
        FakeDestinations::connected(destination_id, "secret-token"),
    );

    assert_eq!(
        delivery.deliver(seed.id).await.unwrap(),
        DeliveryOutcome::Rejected
    );

    let stored = store.row(seed.id);
    assert_eq!(stored.state, DeliveryState::Rejected);
    assert_eq!(stored.last_error_class.as_deref(), Some("track_missing"));
    assert!(sender.calls().is_empty());
}

#[tokio::test]
async fn a_listen_before_the_provider_minimum_is_rejected() {
    let destination_id = Uuid::now_v7();
    let listen_at = chrono::DateTime::from_timestamp(LISTEN_MINIMUM_TS - 60, 0).unwrap();
    let listen = ListenIdentity::from_submission(&submission(Uuid::now_v7(), listen_at));
    let seed = row(&listen, destination_id, DeliveryState::Pending);
    let store = std::sync::Arc::new(FakeStore::default());
    store.seed(seed.clone());
    let sender = std::sync::Arc::new(FakeSender::answering(Ok(SubmissionOutcome::Accepted)));
    let delivery = service(
        std::sync::Arc::clone(&store),
        Some(payload(None, false)),
        std::sync::Arc::clone(&sender),
        FakeDestinations::connected(destination_id, "secret-token"),
    );

    assert_eq!(
        delivery.deliver(seed.id).await.unwrap(),
        DeliveryOutcome::Rejected
    );
    assert_eq!(
        store.row(seed.id).last_error_class.as_deref(),
        Some("before_provider_minimum")
    );
    assert!(sender.calls().is_empty());
}

#[tokio::test]
async fn the_status_read_is_scoped_and_filtered() {
    let principal = Uuid::now_v7();
    let other = Uuid::now_v7();
    let listen = ListenIdentity::from_submission(&submission(principal, Utc::now()));
    let store = std::sync::Arc::new(FakeStore::default());
    let mut first = row(&listen, Uuid::now_v7(), DeliveryState::Delivered);
    first.user_id = principal;
    let mut second = row(&listen, Uuid::now_v7(), DeliveryState::Pending);
    second.user_id = principal;
    let mut foreign = row(&listen, Uuid::now_v7(), DeliveryState::Delivered);
    foreign.user_id = other;
    store.seed(first);
    store.seed(second);
    store.seed(foreign);
    let delivery = service(
        std::sync::Arc::clone(&store),
        None,
        std::sync::Arc::new(FakeSender::answering(Ok(SubmissionOutcome::Accepted))),
        FakeDestinations::disconnected(),
    );

    let pending = delivery
        .status(principal, Some(DeliveryState::Pending), 50)
        .await
        .unwrap();

    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].state, DeliveryState::Pending);

    let all = delivery.status(principal, None, 50).await.unwrap();

    assert_eq!(all.len(), 2);
    assert!(delivery.status(other, None, 1).await.unwrap().len() == 1);
    assert_eq!(delivery.status(principal, None, 0).await.unwrap().len(), 0);
}

#[test]
fn the_provider_idempotence_limit_is_recorded_never_claimed_as_exactly_once() {
    assert_eq!(PROVIDER_IDEMPOTENCE, "timestamp_only");

    let status = DeliveryStatus {
        destination_id: Uuid::now_v7(),
        provider_id: "listenbrainz".to_owned(),
        state: DeliveryState::Delivered,
        attempts: 1,
        provider_idempotence: PROVIDER_IDEMPOTENCE.to_owned(),
        last_error: None,
        updated_at: Utc::now(),
    };
    let value = serde_json::to_value(&status).unwrap();

    assert_eq!(value["provider_idempotence"], json!("timestamp_only"));
    assert!(value.get("credential").is_none());
    assert!(value.get("token").is_none());
    assert!(!serde_json::to_string(&value).unwrap().contains("exactly"));
}

#[test]
fn the_status_projection_carries_no_credential_member() {
    let mut row = DeliveryRow::pending(
        &ListenIdentity::from_submission(&submission(Uuid::now_v7(), Utc::now())),
        Uuid::now_v7(),
        "listenbrainz".to_owned(),
    );
    row.last_error = Some("the destination rejected the stored credential".to_owned());

    let value = serde_json::to_value(DeliveryStatus {
        destination_id: row.destination_id,
        provider_id: row.provider_id.clone(),
        state: row.state,
        attempts: row.attempts,
        provider_idempotence: row.provider_idempotence.clone(),
        last_error: row.last_error.clone(),
        updated_at: row.updated_at,
    })
    .unwrap();

    let mut keys = value
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect::<Vec<_>>();
    keys.sort_unstable();

    assert_eq!(
        keys,
        vec![
            "attempts",
            "destination_id",
            "last_error",
            "provider_id",
            "provider_idempotence",
            "state",
            "updated_at",
        ]
    );
}

#[test]
fn the_delivery_states_round_trip() {
    for state in [
        DeliveryState::Pending,
        DeliveryState::Delivered,
        DeliveryState::Ignored,
        DeliveryState::Rejected,
        DeliveryState::Exhausted,
    ] {
        assert_eq!(DeliveryState::parse(state.as_str()), Some(state));
    }

    assert_eq!(DeliveryState::parse("unknown"), None);
    assert!(!DeliveryState::Pending.is_terminal());
    assert!(DeliveryState::Exhausted.is_terminal());
}

#[test]
fn the_delivery_job_payload_names_its_operation_and_row() {
    let listen = ListenIdentity::from_submission(&submission(Uuid::now_v7(), Utc::now()));
    let destination_id = Uuid::now_v7();
    let delivery_id = Uuid::now_v7();

    let payload = crate::jobs::listening_delivery::ListeningDeliveryV1::for_delivery(
        &listen,
        destination_id,
        delivery_id,
    );

    assert_eq!(payload.delivery_id, delivery_id);
    assert_eq!(
        payload.operation_id,
        crate::jobs::listening_delivery::ListeningDeliveryV1::for_delivery(
            &listen,
            destination_id,
            delivery_id
        )
        .operation_id
    );
    assert_ne!(
        payload.operation_id,
        crate::jobs::listening_delivery::ListeningDeliveryV1::for_delivery(
            &listen,
            Uuid::now_v7(),
            delivery_id
        )
        .operation_id
    );
}

#[test]
fn the_delivery_job_payload_has_no_credential_member() {
    let listen = ListenIdentity::from_submission(&submission(Uuid::now_v7(), Utc::now()));
    let payload = crate::jobs::listening_delivery::ListeningDeliveryV1::for_delivery(
        &listen,
        Uuid::now_v7(),
        Uuid::now_v7(),
    );
    let value = serde_json::to_value(&payload).unwrap();

    let mut keys = value
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect::<Vec<_>>();
    keys.sort_unstable();

    assert_eq!(keys, vec!["delivery_id", "operation_id"]);
}

#[test]
fn the_delivery_queue_and_kind_are_stable() {
    assert_eq!(
        crate::jobs::listening_delivery::LISTEN_DELIVERY_QUEUE,
        "whio.listening.delivery"
    );
    assert_eq!(
        crate::jobs::listening_delivery::LISTEN_DELIVERY_KIND,
        "whio.listening.deliver.v1"
    );
    assert_eq!(
        crate::jobs::listening_delivery::LISTEN_DELIVERY_MAX_ATTEMPTS,
        8
    );
}
