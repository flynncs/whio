use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

use crate::features::identity::destinations::{DestinationCredential, DestinationProvider};
use crate::features::music_library::delivery::{
    DeliveryDestinations, DeliveryError, DeliveryOutcome, DeliveryService, DeliveryState,
    ListenSender, ListenSource, ScrobbleListen, SubmissionError, SubmissionOutcome,
};
use crate::features::music_library::domain::{
    ListenExtent, ListenOrigin, ListenOutcome, ListenSessionId, ListenSubmission, ProviderId,
    SourceIdentity, SourceScope, Track, TrackId, TrackMetadata,
};
use crate::features::music_library::storage::contracts::{
    CatalogueWrite, DeliveryStore, ListenStore, TrackRepositoryError,
};
use crate::jobs::listening_delivery::LISTEN_DELIVERY_MAX_ATTEMPTS;

use super::PostgresTrackRepository;

#[derive(Default)]
struct RecordingSender {
    calls: Mutex<Vec<(String, ScrobbleListen)>>,
    answer: Mutex<Option<Result<SubmissionOutcome, SubmissionError>>>,
}

impl RecordingSender {
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
impl ListenSender for RecordingSender {
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

struct Directory {
    destinations: Vec<(Uuid, String)>,
    credential: Option<DestinationCredential>,
}

impl Directory {
    fn connected(destination_id: Uuid, token: &str) -> Self {
        Self {
            destinations: vec![(destination_id, "listenbrainz".to_owned())],
            credential: Some(DestinationCredential {
                destination_id,
                provider_id: DestinationProvider::ListenBrainz,
                username: Some("flynn".to_owned()),
                token: token.to_owned(),
            }),
        }
    }

    fn disconnected(destination_id: Uuid) -> Self {
        Self {
            destinations: vec![(destination_id, "listenbrainz".to_owned())],
            credential: None,
        }
    }

    fn empty() -> Self {
        Self {
            destinations: Vec::new(),
            credential: None,
        }
    }
}

#[async_trait]
impl DeliveryDestinations for Directory {
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

fn local_source(external_id: &str) -> SourceIdentity {
    SourceIdentity::new(
        ProviderId::new("local".to_owned()).unwrap(),
        SourceScope::Named("/music".to_owned()),
        external_id.to_owned(),
    )
    .unwrap()
}

fn metadata(title: &str, recording_mbid: Option<&str>) -> TrackMetadata {
    TrackMetadata::from_names(
        title.to_owned(),
        vec!["New Order".to_owned()],
        Some(240_000),
    )
    .unwrap()
    .with_recording_id(recording_mbid.map(str::to_owned))
    .unwrap()
}

async fn insert_user(pool: &PgPool, id: Uuid, username: &str) {
    sqlx::query("INSERT INTO users (id, username, display_name) VALUES ($1, $2, $3)")
        .bind(id)
        .bind(username)
        .bind(username)
        .execute(pool)
        .await
        .unwrap();
}

async fn create_track(
    repository: &PostgresTrackRepository,
    external_id: &str,
    recording_mbid: Option<&str>,
) -> Track {
    repository
        .get_or_create(
            local_source(external_id),
            metadata(external_id, recording_mbid),
        )
        .await
        .unwrap()
}

async fn attach_release(pool: &PgPool, track: &Track, release_mbid: &str) {
    let album_id = Uuid::now_v7();

    sqlx::query("INSERT INTO albums (id, title, musicbrainz_release_id) VALUES ($1, $2, $3)")
        .bind(album_id)
        .bind("Power, Corruption & Lies")
        .bind(release_mbid)
        .execute(pool)
        .await
        .unwrap();

    sqlx::query("INSERT INTO album_tracks (album_id, track_id, position) VALUES ($1, $2, 1)")
        .bind(album_id)
        .bind(track.id().as_uuid())
        .execute(pool)
        .await
        .unwrap();
}

fn submission(
    principal: Uuid,
    track: &Track,
    listen_at: DateTime<Utc>,
    session_id: Option<&str>,
) -> ListenSubmission {
    ListenSubmission::new(
        principal,
        ListenOrigin::Native,
        track.id().clone(),
        listen_at,
        session_id.map(|value| ListenSessionId::new(value.to_owned()).unwrap()),
        Some(ListenExtent::new(0, 240_000, true)),
    )
}

struct Harness {
    store: Arc<PostgresTrackRepository>,
    delivery: Arc<DeliveryService>,
    sender: Arc<RecordingSender>,
}

fn service_for(
    pool: &PgPool,
    destinations: Directory,
    answer: Result<SubmissionOutcome, SubmissionError>,
) -> Harness {
    let sender = Arc::new(RecordingSender::answering(answer));
    let reader = Arc::new(PostgresTrackRepository::new(pool.clone()));
    let delivery = Arc::new(DeliveryService::new(
        Arc::clone(&reader) as Arc<dyn DeliveryStore>,
        Arc::clone(&reader) as Arc<dyn ListenSource>,
        Arc::clone(&sender) as Arc<dyn ListenSender>,
        Arc::new(destinations),
    ));
    let store =
        Arc::new(PostgresTrackRepository::new(pool.clone()).with_delivery(Arc::clone(&delivery)));

    Harness {
        store,
        delivery,
        sender,
    }
}

async fn delivery_rows(pool: &PgPool, principal: Uuid) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM listen_deliveries WHERE user_id = $1")
        .bind(principal)
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn delivery_jobs(pool: &PgPool, kind: &str) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM awa.jobs WHERE kind = $1")
        .bind(kind)
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn listen_rows(pool: &PgPool, principal: Uuid) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM listens WHERE user_id = $1")
        .bind(principal)
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn delivery_state(pool: &PgPool, destination_id: Uuid) -> String {
    sqlx::query_scalar("SELECT state FROM listen_deliveries WHERE destination_id = $1")
        .bind(destination_id)
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn ledger_snapshot(pool: &PgPool, principal: Uuid) -> Value {
    let listens = sqlx::query_scalar::<_, Value>(
        "SELECT COALESCE(jsonb_agg(to_jsonb(listen) ORDER BY listen.track_id, listen.listen_at, \
          listen.origin, listen.session_key), '[]'::jsonb) FROM listens AS listen \
         WHERE listen.user_id = $1",
    )
    .bind(principal)
    .fetch_one(pool)
    .await
    .unwrap();
    let plays = sqlx::query_scalar::<_, Value>(
        "SELECT COALESCE(jsonb_agg(to_jsonb(stat) ORDER BY stat.track_id), '[]'::jsonb) \
         FROM track_play_stats AS stat WHERE stat.user_id = $1",
    )
    .bind(principal)
    .fetch_one(pool)
    .await
    .unwrap();
    let marks = sqlx::query_scalar::<_, Value>(
        "SELECT COALESCE(jsonb_agg(to_jsonb(mark)), '[]'::jsonb) FROM listen_marks AS mark \
         WHERE mark.user_id = $1",
    )
    .bind(principal)
    .fetch_one(pool)
    .await
    .unwrap();

    json!({ "listens": listens, "plays": plays, "marks": marks })
}

async fn prepare_awa_schema(pool: &PgPool) {
    awa::migrations::run(pool)
        .await
        .expect("the awa schema is applied for a delivery test");
}

#[sqlx::test]
async fn an_accepted_listen_records_one_row_and_one_job_per_destination(pool: PgPool) {
    prepare_awa_schema(&pool).await;
    let principal = Uuid::now_v7();
    insert_user(&pool, principal, "flynn").await;
    let track = create_track(
        &PostgresTrackRepository::new(pool.clone()),
        "one",
        Some("rec-1"),
    )
    .await;
    let first = Uuid::now_v7();
    let second = Uuid::now_v7();
    let destinations = Directory {
        destinations: vec![
            (first, "listenbrainz".to_owned()),
            (second, "listenbrainz".to_owned()),
        ],
        credential: None,
    };
    let harness = service_for(&pool, destinations, Ok(SubmissionOutcome::Accepted));
    let listen = submission(principal, &track, Utc::now(), Some("session-a"));

    assert_eq!(
        harness.store.submit_listen(&listen).await.unwrap(),
        ListenOutcome::Recorded
    );

    assert_eq!(delivery_rows(&pool, principal).await, 2);
    assert_eq!(
        delivery_jobs(&pool, crate::jobs::listening_delivery::LISTEN_DELIVERY_KIND).await,
        2
    );
    assert_eq!(listen_rows(&pool, principal).await, 1);
}

#[sqlx::test]
async fn a_forced_enqueue_failure_rolls_back_the_listen(pool: PgPool) {
    prepare_awa_schema(&pool).await;
    let principal = Uuid::now_v7();
    insert_user(&pool, principal, "flynn").await;
    let track = create_track(&PostgresTrackRepository::new(pool.clone()), "one", None).await;
    let destination_id = Uuid::now_v7();
    let harness = service_for(
        &pool,
        Directory::connected(destination_id, "token"),
        Ok(SubmissionOutcome::Accepted),
    );

    sqlx::query(
        "CREATE OR REPLACE FUNCTION whio_reject_delivery_job() RETURNS trigger AS $$ \
         BEGIN IF NEW.kind = 'whio.listening.deliver.v1' THEN \
           RAISE EXCEPTION 'forced delivery enqueue failure'; END IF; RETURN NEW; END $$ \
         LANGUAGE plpgsql",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "CREATE TRIGGER whio_reject_delivery_job BEFORE INSERT ON awa.jobs_hot \
         FOR EACH ROW EXECUTE FUNCTION whio_reject_delivery_job()",
    )
    .execute(&pool)
    .await
    .unwrap();

    let listen = submission(principal, &track, Utc::now(), None);
    let failed = harness.store.submit_listen(&listen).await;

    assert!(failed.is_err());
    assert_eq!(listen_rows(&pool, principal).await, 0);
    assert_eq!(delivery_rows(&pool, principal).await, 0);
    assert_eq!(
        delivery_jobs(&pool, crate::jobs::listening_delivery::LISTEN_DELIVERY_KIND).await,
        0
    );
}

#[sqlx::test]
async fn a_duplicate_listen_records_no_delivery_and_no_job(pool: PgPool) {
    prepare_awa_schema(&pool).await;
    let principal = Uuid::now_v7();
    insert_user(&pool, principal, "flynn").await;
    let track = create_track(&PostgresTrackRepository::new(pool.clone()), "one", None).await;
    let destination_id = Uuid::now_v7();
    let harness = service_for(
        &pool,
        Directory::connected(destination_id, "token"),
        Ok(SubmissionOutcome::Accepted),
    );
    let at = Utc::now();
    let listen = submission(principal, &track, at, Some("session-a"));

    assert_eq!(
        harness.store.submit_listen(&listen).await.unwrap(),
        ListenOutcome::Recorded
    );
    assert_eq!(
        harness.store.submit_listen(&listen).await.unwrap(),
        ListenOutcome::Duplicate
    );

    assert_eq!(listen_rows(&pool, principal).await, 1);
    assert_eq!(delivery_rows(&pool, principal).await, 1);
    assert_eq!(
        delivery_jobs(&pool, crate::jobs::listening_delivery::LISTEN_DELIVERY_KIND).await,
        1
    );
}

#[sqlx::test]
async fn a_listen_without_an_enabled_destination_is_still_accepted(pool: PgPool) {
    prepare_awa_schema(&pool).await;
    let principal = Uuid::now_v7();
    insert_user(&pool, principal, "flynn").await;
    let track = create_track(&PostgresTrackRepository::new(pool.clone()), "one", None).await;
    let harness = service_for(&pool, Directory::empty(), Ok(SubmissionOutcome::Accepted));

    assert_eq!(
        harness
            .store
            .submit_listen(&submission(principal, &track, Utc::now(), None))
            .await
            .unwrap(),
        ListenOutcome::Recorded
    );

    assert_eq!(listen_rows(&pool, principal).await, 1);
    assert_eq!(delivery_rows(&pool, principal).await, 0);
    assert_eq!(
        delivery_jobs(&pool, crate::jobs::listening_delivery::LISTEN_DELIVERY_KIND).await,
        0
    );
}

#[sqlx::test]
async fn a_successful_delivery_leaves_the_local_ledger_byte_identical(pool: PgPool) {
    prepare_awa_schema(&pool).await;
    let principal = Uuid::now_v7();
    insert_user(&pool, principal, "flynn").await;
    let track = create_track(
        &PostgresTrackRepository::new(pool.clone()),
        "one",
        Some("rec-1"),
    )
    .await;
    attach_release(&pool, &track, "release-1").await;
    let destination_id = Uuid::now_v7();
    let harness = service_for(
        &pool,
        Directory::connected(destination_id, "token"),
        Ok(SubmissionOutcome::Accepted),
    );

    harness
        .store
        .submit_listen(&submission(principal, &track, Utc::now(), None))
        .await
        .unwrap();
    let before = ledger_snapshot(&pool, principal).await;
    let row = sqlx::query_scalar::<_, Uuid>("SELECT id FROM listen_deliveries LIMIT 1")
        .fetch_one(&pool)
        .await
        .unwrap();

    assert_eq!(
        harness.delivery.deliver(row).await.unwrap(),
        DeliveryOutcome::Delivered
    );
    assert_eq!(ledger_snapshot(&pool, principal).await, before);
    assert_eq!(delivery_state(&pool, destination_id).await, "delivered");
    assert_eq!(
        harness.sender.calls()[0].1.recording_mbid.as_deref(),
        Some("rec-1")
    );
}

#[sqlx::test]
async fn a_failed_delivery_leaves_the_local_ledger_byte_identical(pool: PgPool) {
    prepare_awa_schema(&pool).await;
    let principal = Uuid::now_v7();
    insert_user(&pool, principal, "flynn").await;
    let track = create_track(&PostgresTrackRepository::new(pool.clone()), "one", None).await;
    let destination_id = Uuid::now_v7();
    let harness = service_for(
        &pool,
        Directory::connected(destination_id, "token"),
        Err(SubmissionError::Unavailable),
    );

    harness
        .store
        .submit_listen(&submission(principal, &track, Utc::now(), None))
        .await
        .unwrap();
    let before = ledger_snapshot(&pool, principal).await;
    let row = sqlx::query_scalar::<_, Uuid>("SELECT id FROM listen_deliveries LIMIT 1")
        .fetch_one(&pool)
        .await
        .unwrap();

    assert_eq!(
        harness.delivery.deliver(row).await.unwrap(),
        DeliveryOutcome::Retryable
    );
    assert_eq!(ledger_snapshot(&pool, principal).await, before);
    assert_eq!(delivery_state(&pool, destination_id).await, "pending");
}

#[sqlx::test]
async fn delivery_to_a_disabled_destination_leaves_the_local_ledger_byte_identical(pool: PgPool) {
    prepare_awa_schema(&pool).await;
    let principal = Uuid::now_v7();
    insert_user(&pool, principal, "flynn").await;
    let track = create_track(&PostgresTrackRepository::new(pool.clone()), "one", None).await;
    let destination_id = Uuid::now_v7();
    let harness = service_for(
        &pool,
        Directory::connected(destination_id, "token"),
        Ok(SubmissionOutcome::Accepted),
    );

    harness
        .store
        .submit_listen(&submission(principal, &track, Utc::now(), None))
        .await
        .unwrap();
    let before = ledger_snapshot(&pool, principal).await;
    let row = sqlx::query_scalar::<_, Uuid>("SELECT id FROM listen_deliveries LIMIT 1")
        .fetch_one(&pool)
        .await
        .unwrap();

    sqlx::query(
        "INSERT INTO destination_accounts \
         (id, user_id, provider_id, encrypted_secret, enabled, created_at, updated_at) \
         VALUES ($1, $2, 'listenbrainz', '\\x00', FALSE, CURRENT_TIMESTAMP, CURRENT_TIMESTAMP)",
    )
    .bind(destination_id)
    .bind(principal)
    .execute(&pool)
    .await
    .unwrap();

    let harness = service_for(
        &pool,
        Directory::disconnected(destination_id),
        Ok(SubmissionOutcome::Accepted),
    );

    assert!(matches!(
        harness.delivery.deliver(row).await,
        Err(DeliveryError::Disabled)
    ));
    assert_eq!(ledger_snapshot(&pool, principal).await, before);
    assert_eq!(delivery_state(&pool, destination_id).await, "pending");
    assert!(harness.sender.calls().is_empty());
}

#[sqlx::test]
async fn a_delivered_row_is_not_submitted_again(pool: PgPool) {
    prepare_awa_schema(&pool).await;
    let principal = Uuid::now_v7();
    insert_user(&pool, principal, "flynn").await;
    let track = create_track(&PostgresTrackRepository::new(pool.clone()), "one", None).await;
    let destination_id = Uuid::now_v7();
    let harness = service_for(
        &pool,
        Directory::connected(destination_id, "token"),
        Ok(SubmissionOutcome::Accepted),
    );

    harness
        .store
        .submit_listen(&submission(principal, &track, Utc::now(), None))
        .await
        .unwrap();
    let row = sqlx::query_scalar::<_, Uuid>("SELECT id FROM listen_deliveries LIMIT 1")
        .fetch_one(&pool)
        .await
        .unwrap();

    assert_eq!(
        harness.delivery.deliver(row).await.unwrap(),
        DeliveryOutcome::Delivered
    );
    assert_eq!(
        harness.delivery.deliver(row).await.unwrap(),
        DeliveryOutcome::AlreadyTerminal
    );
    assert_eq!(harness.sender.calls().len(), 1);
    assert_eq!(
        sqlx::query_scalar::<_, i32>("SELECT attempts FROM listen_deliveries WHERE id = $1")
            .bind(row)
            .fetch_one(&pool)
            .await
            .unwrap(),
        1
    );
}

#[sqlx::test]
async fn a_rate_limited_listen_keeps_its_attempt_count(pool: PgPool) {
    prepare_awa_schema(&pool).await;
    let principal = Uuid::now_v7();
    insert_user(&pool, principal, "flynn").await;
    let track = create_track(&PostgresTrackRepository::new(pool.clone()), "one", None).await;
    let destination_id = Uuid::now_v7();
    let harness = service_for(
        &pool,
        Directory::connected(destination_id, "token"),
        Err(SubmissionError::RateLimited {
            retry_after: Some(std::time::Duration::from_secs(5)),
        }),
    );

    harness
        .store
        .submit_listen(&submission(principal, &track, Utc::now(), None))
        .await
        .unwrap();
    let row = sqlx::query_scalar::<_, Uuid>("SELECT id FROM listen_deliveries LIMIT 1")
        .fetch_one(&pool)
        .await
        .unwrap();

    assert!(matches!(
        harness.delivery.deliver(row).await,
        Err(DeliveryError::RateLimited { .. })
    ));
    assert_eq!(
        sqlx::query_scalar::<_, i32>("SELECT attempts FROM listen_deliveries WHERE id = $1")
            .bind(row)
            .fetch_one(&pool)
            .await
            .unwrap(),
        0
    );
    assert_eq!(delivery_state(&pool, destination_id).await, "pending");
}

#[sqlx::test]
async fn the_delivery_status_read_is_scoped_to_the_principal(pool: PgPool) {
    prepare_awa_schema(&pool).await;
    let principal = Uuid::now_v7();
    let other = Uuid::now_v7();
    insert_user(&pool, principal, "flynn").await;
    insert_user(&pool, other, "sabine").await;
    let tracks = PostgresTrackRepository::new(pool.clone());
    let first = create_track(&tracks, "one", None).await;
    let second = create_track(&tracks, "two", None).await;
    let destination_id = Uuid::now_v7();
    let harness = service_for(
        &pool,
        Directory::connected(destination_id, "token"),
        Ok(SubmissionOutcome::Accepted),
    );

    harness
        .store
        .submit_listen(&submission(principal, &first, Utc::now(), None))
        .await
        .unwrap();
    harness
        .store
        .submit_listen(&submission(principal, &second, Utc::now(), None))
        .await
        .unwrap();
    harness
        .store
        .submit_listen(&submission(other, &first, Utc::now(), None))
        .await
        .unwrap();

    let mine = harness.delivery.status(principal, None, 50).await.unwrap();

    assert_eq!(mine.len(), 2);
    assert_eq!(
        harness
            .delivery
            .status(principal, Some(DeliveryState::Delivered), 50)
            .await
            .unwrap()
            .len(),
        0
    );
    assert_eq!(
        harness
            .delivery
            .status(principal, None, 1)
            .await
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        harness
            .delivery
            .status(other, None, 50)
            .await
            .unwrap()
            .len(),
        1
    );
    assert!(
        mine.iter()
            .all(|status| status.provider_idempotence == "timestamp_only")
    );
}

#[sqlx::test]
async fn a_pending_row_survives_a_worker_restart_with_one_effect(pool: PgPool) {
    prepare_awa_schema(&pool).await;
    let principal = Uuid::now_v7();
    insert_user(&pool, principal, "flynn").await;
    let track = create_track(&PostgresTrackRepository::new(pool.clone()), "one", None).await;
    let destination_id = Uuid::now_v7();
    let harness = service_for(
        &pool,
        Directory::connected(destination_id, "token"),
        Ok(SubmissionOutcome::Accepted),
    );

    harness
        .store
        .submit_listen(&submission(principal, &track, Utc::now(), None))
        .await
        .unwrap();
    let row = sqlx::query_scalar::<_, Uuid>("SELECT id FROM listen_deliveries LIMIT 1")
        .fetch_one(&pool)
        .await
        .unwrap();

    let restarted = service_for(
        &pool,
        Directory::connected(destination_id, "token"),
        Ok(SubmissionOutcome::Accepted),
    );

    assert_eq!(
        restarted.delivery.deliver(row).await.unwrap(),
        DeliveryOutcome::Delivered
    );
    assert_eq!(
        restarted.delivery.deliver(row).await.unwrap(),
        DeliveryOutcome::AlreadyTerminal
    );
    assert_eq!(restarted.sender.calls().len(), 1);
}

#[sqlx::test]
async fn the_listen_payload_reads_canonical_members(pool: PgPool) {
    prepare_awa_schema(&pool).await;
    let track = create_track(
        &PostgresTrackRepository::new(pool.clone()),
        "one",
        Some("rec-1"),
    )
    .await;
    attach_release(&pool, &track, "release-1").await;
    let store = PostgresTrackRepository::new(pool.clone());

    let payload = ListenSource::listen_payload(&store, *track.id().as_uuid())
        .await
        .unwrap()
        .unwrap();

    assert_eq!(payload.track_name, "one");
    assert_eq!(payload.artist_name, "New Order");
    assert_eq!(payload.recording_mbid.as_deref(), Some("rec-1"));
    assert_eq!(payload.release_mbid.as_deref(), Some("release-1"));
    assert_eq!(
        payload.release_name.as_deref(),
        Some("Power, Corruption & Lies")
    );
    assert_eq!(payload.duration_ms, Some(240_000));
    assert!(
        ListenSource::listen_payload(&store, Uuid::now_v7())
            .await
            .unwrap()
            .is_none()
    );
}

#[sqlx::test]
async fn the_local_reads_are_the_only_ledger_writers(pool: PgPool) {
    prepare_awa_schema(&pool).await;
    let principal = Uuid::now_v7();
    insert_user(&pool, principal, "flynn").await;
    let track = create_track(&PostgresTrackRepository::new(pool.clone()), "one", None).await;
    let harness = service_for(
        &pool,
        Directory::connected(Uuid::now_v7(), "token"),
        Err(SubmissionError::CredentialRejected),
    );

    harness
        .store
        .submit_listen(&submission(principal, &track, Utc::now(), None))
        .await
        .unwrap();
    let before = ledger_snapshot(&pool, principal).await;
    let row = sqlx::query_scalar::<_, Uuid>("SELECT id FROM listen_deliveries LIMIT 1")
        .fetch_one(&pool)
        .await
        .unwrap();

    assert!(matches!(
        harness.delivery.deliver(row).await,
        Err(DeliveryError::CredentialRejected)
    ));

    assert_eq!(ledger_snapshot(&pool, principal).await, before);
    assert_eq!(
        sqlx::query_scalar::<_, Option<String>>(
            "SELECT last_error_class FROM listen_deliveries WHERE id = $1"
        )
        .bind(row)
        .fetch_one(&pool)
        .await
        .unwrap()
        .as_deref(),
        Some("credential_rejected")
    );
    assert!(
        TrackRepositoryError::InvalidData
            .to_string()
            .contains("invalid")
    );
    assert_eq!(TrackId::from_uuid(*track.id().as_uuid()), *track.id());
}

async fn job_state(pool: &PgPool, delivery_id: Uuid) -> String {
    sqlx::query_scalar(
        "SELECT state::text FROM awa.jobs WHERE kind = 'whio.listening.deliver.v1' \
         AND args->>'delivery_id' = $1 ORDER BY id DESC LIMIT 1",
    )
    .bind(delivery_id.to_string())
    .fetch_one(pool)
    .await
    .unwrap()
}

async fn job_attempt(pool: &PgPool, delivery_id: Uuid) -> i16 {
    sqlx::query_scalar(
        "SELECT attempt FROM awa.jobs WHERE kind = 'whio.listening.deliver.v1' \
         AND args->>'delivery_id' = $1 ORDER BY id DESC LIMIT 1",
    )
    .bind(delivery_id.to_string())
    .fetch_one(pool)
    .await
    .unwrap()
}

async fn job_count(pool: &PgPool, delivery_id: Uuid) -> i64 {
    sqlx::query_scalar(
        "SELECT count(*) FROM awa.jobs WHERE kind = 'whio.listening.deliver.v1' \
         AND args->>'delivery_id' = $1",
    )
    .bind(delivery_id.to_string())
    .fetch_one(pool)
    .await
    .unwrap()
}

async fn first_delivery(pool: &PgPool) -> Uuid {
    sqlx::query_scalar("SELECT id FROM listen_deliveries LIMIT 1")
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn delivery_attempts(pool: &PgPool, delivery_id: Uuid) -> i32 {
    sqlx::query_scalar("SELECT attempts FROM listen_deliveries WHERE id = $1")
        .bind(delivery_id)
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn await_delivery_state(pool: &PgPool, delivery_id: Uuid, state: &str, within: Duration) {
    let deadline = tokio::time::Instant::now() + within;

    loop {
        let stored: String =
            sqlx::query_scalar("SELECT state FROM listen_deliveries WHERE id = $1")
                .bind(delivery_id)
                .fetch_one(pool)
                .await
                .unwrap();

        if stored == state {
            return;
        }

        assert!(
            tokio::time::Instant::now() < deadline,
            "delivery {delivery_id} never reached {state}, last saw {stored}"
        );

        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

async fn await_delivery_attempts(
    pool: &PgPool,
    delivery_id: Uuid,
    attempts: i32,
    within: Duration,
) {
    let deadline = tokio::time::Instant::now() + within;

    loop {
        if delivery_attempts(pool, delivery_id).await == attempts {
            return;
        }

        assert!(
            tokio::time::Instant::now() < deadline,
            "delivery {delivery_id} never reached {} attempts",
            attempts
        );

        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

async fn await_job_state(pool: &PgPool, delivery_id: Uuid, state: &str, within: Duration) {
    let deadline = tokio::time::Instant::now() + within;

    loop {
        let stored = job_state(pool, delivery_id).await;

        if stored == state {
            return;
        }

        assert!(
            tokio::time::Instant::now() < deadline,
            "job for delivery {delivery_id} never reached {state}, last saw {stored}"
        );

        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

async fn await_terminal_job(pool: &PgPool, delivery_id: Uuid, within: Duration) -> String {
    let deadline = tokio::time::Instant::now() + within;

    loop {
        let stored = job_state(pool, delivery_id).await;

        if matches!(stored.as_str(), "completed" | "failed" | "cancelled") {
            return stored;
        }

        assert!(
            tokio::time::Instant::now() < deadline,
            "job for delivery {delivery_id} never became terminal, last saw {stored}"
        );

        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

async fn start_delivery_worker(pool: &PgPool, delivery: Arc<DeliveryService>) -> awa::Client {
    let builder = awa::Client::builder(pool.clone()).state(delivery).queue(
        crate::jobs::listening_delivery::LISTEN_DELIVERY_QUEUE,
        awa::QueueConfig {
            max_workers: 1,
            poll_interval: Duration::from_millis(20),
            ..awa::QueueConfig::default()
        },
    );
    let client =
        crate::jobs::listening_delivery::register(builder, crate::jobs::RetryPolicy::default())
            .build()
            .expect("a listening delivery worker");
    client
        .start()
        .await
        .expect("the listening delivery worker starts");

    client
}

#[sqlx::test]
async fn a_job_enqueued_while_no_worker_runs_delivers_once_on_start(pool: PgPool) {
    prepare_awa_schema(&pool).await;
    let principal = Uuid::now_v7();
    insert_user(&pool, principal, "flynn").await;
    let track = create_track(&PostgresTrackRepository::new(pool.clone()), "one", None).await;
    let destination_id = Uuid::now_v7();
    let harness = service_for(
        &pool,
        Directory::connected(destination_id, "token"),
        Ok(SubmissionOutcome::Accepted),
    );

    harness
        .store
        .submit_listen(&submission(principal, &track, Utc::now(), None))
        .await
        .unwrap();
    let row = first_delivery(&pool).await;

    assert_eq!(delivery_state(&pool, destination_id).await, "pending");
    assert!(harness.sender.calls().is_empty());
    assert_eq!(job_count(&pool, row).await, 1);

    let worker = start_delivery_worker(&pool, Arc::clone(&harness.delivery)).await;

    await_delivery_state(&pool, row, "delivered", Duration::from_secs(30)).await;
    await_job_state(&pool, row, "completed", Duration::from_secs(30)).await;

    assert_eq!(harness.sender.calls().len(), 1);
    assert_eq!(delivery_attempts(&pool, row).await, 1);
    assert_eq!(job_count(&pool, row).await, 1);

    worker.shutdown(Duration::from_secs(10)).await;

    let restarted = start_delivery_worker(&pool, Arc::clone(&harness.delivery)).await;
    tokio::time::sleep(Duration::from_millis(300)).await;

    assert_eq!(harness.sender.calls().len(), 1);
    assert_eq!(delivery_attempts(&pool, row).await, 1);
    assert_eq!(job_count(&pool, row).await, 1);

    restarted.shutdown(Duration::from_secs(10)).await;
}

#[sqlx::test]
async fn a_transient_fault_retries_through_the_handler(pool: PgPool) {
    prepare_awa_schema(&pool).await;
    let principal = Uuid::now_v7();
    insert_user(&pool, principal, "flynn").await;
    let track = create_track(&PostgresTrackRepository::new(pool.clone()), "one", None).await;
    let destination_id = Uuid::now_v7();
    let harness = service_for(
        &pool,
        Directory::connected(destination_id, "token"),
        Err(SubmissionError::Unavailable),
    );

    harness
        .store
        .submit_listen(&submission(principal, &track, Utc::now(), None))
        .await
        .unwrap();
    let row = first_delivery(&pool).await;

    let worker = start_delivery_worker(&pool, Arc::clone(&harness.delivery)).await;

    await_delivery_attempts(&pool, row, 1, Duration::from_secs(30)).await;
    worker.shutdown(Duration::from_secs(10)).await;

    assert!(!harness.sender.calls().is_empty());
    assert_eq!(delivery_state(&pool, destination_id).await, "pending");
    assert!(delivery_attempts(&pool, row).await >= 1);
    assert_eq!(
        sqlx::query_scalar::<_, Option<String>>(
            "SELECT last_error_class FROM listen_deliveries WHERE id = $1"
        )
        .bind(row)
        .fetch_one(&pool)
        .await
        .unwrap()
        .as_deref(),
        Some("provider_unavailable")
    );
    assert!(
        matches!(
            job_state(&pool, row).await.as_str(),
            "available" | "scheduled" | "retryable" | "running"
        ),
        "a transient fault must leave the job claimable, not completed"
    );
}

#[sqlx::test]
async fn two_snoozes_through_the_handler_consume_no_attempt(pool: PgPool) {
    prepare_awa_schema(&pool).await;
    let principal = Uuid::now_v7();
    insert_user(&pool, principal, "flynn").await;
    let track = create_track(&PostgresTrackRepository::new(pool.clone()), "one", None).await;
    let destination_id = Uuid::now_v7();
    let harness = service_for(
        &pool,
        Directory::connected(destination_id, "token"),
        Err(SubmissionError::RateLimited {
            retry_after: Some(Duration::from_secs(1)),
        }),
    );

    harness
        .store
        .submit_listen(&submission(principal, &track, Utc::now(), None))
        .await
        .unwrap();
    let row = first_delivery(&pool).await;

    let worker = start_delivery_worker(&pool, Arc::clone(&harness.delivery)).await;

    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);

    loop {
        let calls = harness.sender.calls().len();
        let state = job_state(&pool, row).await;
        let attempt = job_attempt(&pool, row).await;

        if calls >= 2 && state != "running" && attempt == 0 {
            break;
        }

        assert!(
            tokio::time::Instant::now() < deadline,
            "two snoozes never settled without consuming an attempt: {calls} calls, state {state}, attempt {attempt}"
        );

        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    assert_eq!(delivery_attempts(&pool, row).await, 0);
    assert_eq!(delivery_state(&pool, destination_id).await, "pending");
    assert_eq!(job_count(&pool, row).await, 1);

    worker.shutdown(Duration::from_secs(10)).await;
}

#[sqlx::test]
async fn a_credential_rejection_is_terminal_through_the_handler(pool: PgPool) {
    prepare_awa_schema(&pool).await;
    let principal = Uuid::now_v7();
    insert_user(&pool, principal, "flynn").await;
    let track = create_track(&PostgresTrackRepository::new(pool.clone()), "one", None).await;
    let destination_id = Uuid::now_v7();
    let harness = service_for(
        &pool,
        Directory::connected(destination_id, "token"),
        Err(SubmissionError::CredentialRejected),
    );

    harness
        .store
        .submit_listen(&submission(principal, &track, Utc::now(), None))
        .await
        .unwrap();
    let row = first_delivery(&pool).await;

    let worker = start_delivery_worker(&pool, Arc::clone(&harness.delivery)).await;

    await_delivery_state(&pool, row, "rejected", Duration::from_secs(30)).await;
    await_job_state(&pool, row, "failed", Duration::from_secs(30)).await;

    assert_eq!(harness.sender.calls().len(), 1);
    assert_eq!(delivery_attempts(&pool, row).await, 1);
    assert_eq!(
        sqlx::query_scalar::<_, Option<String>>(
            "SELECT last_error_class FROM listen_deliveries WHERE id = $1"
        )
        .bind(row)
        .fetch_one(&pool)
        .await
        .unwrap()
        .as_deref(),
        Some("credential_rejected")
    );
    assert_eq!(job_state(&pool, row).await, "failed");
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM destination_accounts")
            .fetch_one(&pool)
            .await
            .unwrap(),
        0,
        "the library must not write the identity-owned destination table"
    );

    worker.shutdown(Duration::from_secs(10)).await;
}

#[sqlx::test]
async fn an_exhausted_row_is_terminal_through_the_handler(pool: PgPool) {
    prepare_awa_schema(&pool).await;
    let principal = Uuid::now_v7();
    insert_user(&pool, principal, "flynn").await;
    let track = create_track(&PostgresTrackRepository::new(pool.clone()), "one", None).await;
    let destination_id = Uuid::now_v7();
    let harness = service_for(
        &pool,
        Directory::connected(destination_id, "token"),
        Err(SubmissionError::Unavailable),
    );

    harness
        .store
        .submit_listen(&submission(principal, &track, Utc::now(), None))
        .await
        .unwrap();
    let row = first_delivery(&pool).await;

    sqlx::query("UPDATE listen_deliveries SET attempts = $2 WHERE id = $1")
        .bind(row)
        .bind(i32::from(LISTEN_DELIVERY_MAX_ATTEMPTS) - 1)
        .execute(&pool)
        .await
        .unwrap();

    let worker = start_delivery_worker(&pool, Arc::clone(&harness.delivery)).await;

    await_delivery_state(&pool, row, "exhausted", Duration::from_secs(30)).await;

    assert_eq!(harness.sender.calls().len(), 1);
    assert_eq!(
        delivery_attempts(&pool, row).await,
        i32::from(LISTEN_DELIVERY_MAX_ATTEMPTS)
    );
    await_terminal_job(&pool, row, Duration::from_secs(30)).await;

    worker.shutdown(Duration::from_secs(10)).await;
}

#[sqlx::test]
async fn an_empty_mbid_cannot_be_stored_and_a_zero_duration_is_omitted(pool: PgPool) {
    prepare_awa_schema(&pool).await;
    let track = create_track(
        &PostgresTrackRepository::new(pool.clone()),
        "one",
        Some("rec-1"),
    )
    .await;
    attach_release(&pool, &track, "release-1").await;
    let track_id = *track.id().as_uuid();
    let album_id = sqlx::query_scalar::<_, Uuid>(
        "SELECT album_id FROM album_tracks WHERE track_id = $1 LIMIT 1",
    )
    .bind(track_id)
    .fetch_one(&pool)
    .await
    .unwrap();

    let empty_recording =
        sqlx::query("UPDATE tracks SET musicbrainz_recording_id = '' WHERE id = $1")
            .bind(track_id)
            .execute(&pool)
            .await;
    assert!(
        empty_recording.is_err(),
        "the schema rejects an empty recording mbid"
    );

    let empty_release = sqlx::query("UPDATE albums SET musicbrainz_release_id = '' WHERE id = $1")
        .bind(album_id)
        .execute(&pool)
        .await;
    assert!(
        empty_release.is_err(),
        "the schema rejects an empty release mbid"
    );

    sqlx::query("UPDATE tracks SET duration_ms = 0 WHERE id = $1")
        .bind(track_id)
        .execute(&pool)
        .await
        .unwrap();

    let store = PostgresTrackRepository::new(pool.clone());
    let payload = ListenSource::listen_payload(&store, track_id)
        .await
        .unwrap()
        .unwrap();

    assert_eq!(payload.recording_mbid.as_deref(), Some("rec-1"));
    assert_eq!(payload.release_mbid.as_deref(), Some("release-1"));
    assert!(payload.duration_ms.is_none());

    let value = serde_json::to_value(payload.scrobble(1_700_000_000, "whio/test")).unwrap();
    let members = value.as_object().unwrap();

    assert!(
        !members.contains_key("duration_ms"),
        "a zero duration is omitted"
    );
}
