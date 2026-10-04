use crate::support;

use std::{
    net::SocketAddr,
    sync::{Arc, Mutex},
    time::Duration,
};

use async_trait::async_trait;
use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use base64::Engine;
use base64::engine::general_purpose::STANDARD_NO_PAD;
use serde_json::{Value, json};
use sqlx::PgPool;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};
use tower::ServiceExt;
use uuid::Uuid;
use whio_core::features::{
    identity::{
        destinations::{
            DestinationCredential, DestinationError, DestinationProvider, DestinationService,
            DestinationValidator, ValidatedDestination, ValidationError,
        },
        domain::{Principal, User, UserId, UserRole},
        secrets::CredentialKey,
        service::CredentialService,
    },
    music_library::{
        delivery::{
            DeliveryDestinations, DeliveryError, DeliveryOutcome, DeliveryService, ListenSender,
            ListenSource, ScrobbleListen, SubmissionError, SubmissionOutcome,
        },
        domain::{
            ListenExtent, ListenOrigin, ListenOutcome, ListenSubmission, ProviderId,
            SourceIdentity, SourceScope, Track, TrackMetadata,
        },
        test_support::{CatalogueWrite, DeliveryStore, ListenStore, PostgresTrackRepository},
    },
};

struct DestinationHost {
    address: SocketAddr,
    requests: Arc<Mutex<Vec<(Value, String)>>>,
}

impl DestinationHost {
    async fn start(status: u16, body: &'static str) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("a stub port");
        let address = listener.local_addr().expect("a stub address");
        let requests = Arc::new(Mutex::new(Vec::new()));
        let recorded = Arc::clone(&requests);

        tokio::spawn(async move {
            while let Ok((mut socket, _)) = listener.accept().await {
                let recorded = Arc::clone(&recorded);
                tokio::spawn(async move {
                    let mut buffer = Vec::new();
                    let mut chunk = [0u8; 4096];
                    let mut needed = None;
                    loop {
                        let read = socket.read(&mut chunk).await.unwrap_or(0);
                        if read == 0 {
                            break;
                        }
                        buffer.extend_from_slice(&chunk[..read]);
                        if needed.is_none() {
                            needed = header_end(&buffer)
                                .map(|end| (end, content_length(&buffer[..end]).unwrap_or(0)));
                        }
                        if let Some((end, length)) = needed
                            && buffer.len() >= end + length
                        {
                            break;
                        }
                    }

                    let end = header_end(&buffer).expect("a complete request head");
                    let head = String::from_utf8_lossy(&buffer[..end]).to_string();
                    let authorization = head
                        .lines()
                        .find_map(|line| {
                            let (name, value) = line.split_once(':')?;
                            name.eq_ignore_ascii_case("authorization")
                                .then(|| value.trim().to_owned())
                        })
                        .unwrap_or_default();
                    let payload: Value =
                        serde_json::from_slice(&buffer[end..]).expect("a json request body");
                    recorded.lock().unwrap().push((payload, authorization));

                    let response = format!(
                        "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\n\
                         Content-Length: {}\r\nRetry-After: 7\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    let _ = socket.write_all(response.as_bytes()).await;
                    let _ = socket.shutdown().await;
                });
            }
        });

        Self { address, requests }
    }

    fn requests(&self) -> Vec<(Value, String)> {
        self.requests.lock().unwrap().clone()
    }
}

fn header_end(buffer: &[u8]) -> Option<usize> {
    buffer
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|position| position + 4)
}

fn content_length(head: &[u8]) -> Option<usize> {
    String::from_utf8_lossy(head).lines().find_map(|line| {
        line.to_ascii_lowercase()
            .strip_prefix("content-length:")
            .and_then(|value| value.trim().parse().ok())
    })
}

struct HttpSender {
    client: reqwest::Client,
    address: SocketAddr,
}

impl HttpSender {
    fn new(address: SocketAddr) -> Self {
        Self {
            client: reqwest::Client::builder()
                .timeout(Duration::from_secs(5))
                .build()
                .expect("a test http client"),
            address,
        }
    }
}

#[async_trait]
impl ListenSender for HttpSender {
    async fn submit(
        &self,
        credential: &str,
        listen: &ScrobbleListen,
    ) -> Result<SubmissionOutcome, SubmissionError> {
        let response = self
            .client
            .post(format!("http://{}/1/submit-listens", self.address))
            .header("Authorization", format!("Token {credential}"))
            .header("Content-Type", "application/json")
            .json(&listen)
            .send()
            .await
            .map_err(|_| SubmissionError::Unavailable)?;

        match response.status().as_u16() {
            200 => Ok(SubmissionOutcome::Accepted),
            400 => Err(SubmissionError::Rejected {
                message: response.text().await.unwrap_or_default(),
            }),
            401 => Err(SubmissionError::CredentialRejected),
            429 => Err(SubmissionError::RateLimited {
                retry_after: response
                    .headers()
                    .get("retry-after")
                    .and_then(|value| value.to_str().ok())
                    .and_then(|value| value.parse().ok())
                    .map(Duration::from_secs),
            }),
            _ => Err(SubmissionError::Unavailable),
        }
    }
}

#[derive(Clone)]
struct Directory {
    destination_id: Uuid,
    enabled: bool,
}

#[async_trait]
impl DeliveryDestinations for Directory {
    async fn enabled_destinations(
        &self,
        _user_id: Uuid,
    ) -> Result<Vec<(Uuid, String)>, DeliveryError> {
        Ok(vec![(self.destination_id, "listenbrainz".to_owned())])
    }

    async fn enabled_credential(
        &self,
        _user_id: Uuid,
        _provider_id: &str,
    ) -> Result<Option<DestinationCredential>, DeliveryError> {
        if !self.enabled {
            return Ok(None);
        }

        Ok(Some(DestinationCredential {
            destination_id: self.destination_id,
            provider_id: DestinationProvider::ListenBrainz,
            username: Some("flynn".to_owned()),
            token: "test-token".to_owned(),
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
    title: &str,
    recording: Option<&str>,
) -> Track {
    repository
        .get_or_create(local_source(title), metadata(title, recording))
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
    listen_at: chrono::DateTime<chrono::Utc>,
) -> ListenSubmission {
    ListenSubmission::new(
        principal,
        ListenOrigin::Native,
        track.id().clone(),
        listen_at,
        None,
        Some(ListenExtent::new(0, 240_000, true)),
    )
}

fn service(
    pool: &PgPool,
    host: &DestinationHost,
    destination_id: Uuid,
    enabled: bool,
) -> (Arc<PostgresTrackRepository>, Arc<DeliveryService>) {
    let reader = Arc::new(PostgresTrackRepository::new(pool.clone()));
    let delivery = Arc::new(DeliveryService::new(
        Arc::clone(&reader) as Arc<dyn DeliveryStore>,
        Arc::clone(&reader) as Arc<dyn ListenSource>,
        Arc::new(HttpSender::new(host.address)),
        Arc::new(Directory {
            destination_id,
            enabled,
        }),
    ));
    let store =
        Arc::new(PostgresTrackRepository::new(pool.clone()).with_delivery(Arc::clone(&delivery)));

    (store, delivery)
}

async fn pending_row(pool: &PgPool, principal: Uuid) -> Uuid {
    sqlx::query_scalar("SELECT id FROM listen_deliveries WHERE user_id = $1")
        .bind(principal)
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn delivery_state(pool: &PgPool, delivery_id: Uuid) -> String {
    sqlx::query_scalar("SELECT state FROM listen_deliveries WHERE id = $1")
        .bind(delivery_id)
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn attempt_count(pool: &PgPool, delivery_id: Uuid) -> i32 {
    sqlx::query_scalar("SELECT attempts FROM listen_deliveries WHERE id = $1")
        .bind(delivery_id)
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn prepare_awa_schema(pool: &PgPool) {
    awa::migrations::run(pool)
        .await
        .expect("the awa schema is applied for a delivery test");
}

const STATUS_PASSWORD: &str = "whio-status-password";
const DESTINATION_TOKEN: &str = "test-token";

struct AcceptingValidator;

#[async_trait]
impl DestinationValidator for AcceptingValidator {
    async fn validate(
        &self,
        _provider_id: &DestinationProvider,
        _token: &str,
    ) -> Result<ValidatedDestination, ValidationError> {
        Ok(ValidatedDestination {
            provider_id: DestinationProvider::ListenBrainz,
            username: Some("flynn".to_owned()),
        })
    }
}

struct IdentityDestinations {
    destinations: Arc<DestinationService>,
    user_id: Uuid,
}

fn identity_failure(error: DestinationError) -> DeliveryError {
    match error {
        DestinationError::Disabled => DeliveryError::Unavailable,
        DestinationError::Unavailable => DeliveryError::ProviderUnavailable,
        DestinationError::InvalidCredential => DeliveryError::CredentialRejected,
        DestinationError::Unsupported
        | DestinationError::AlreadyConnected
        | DestinationError::NotFound
        | DestinationError::Repository(_) => DeliveryError::Disabled,
    }
}

#[async_trait]
impl DeliveryDestinations for IdentityDestinations {
    async fn enabled_destinations(
        &self,
        _user_id: Uuid,
    ) -> Result<Vec<(Uuid, String)>, DeliveryError> {
        self.destinations
            .enabled_destinations(self.user_id)
            .await
            .map_err(identity_failure)
    }

    async fn enabled_credential(
        &self,
        _user_id: Uuid,
        provider_id: &str,
    ) -> Result<Option<DestinationCredential>, DeliveryError> {
        self.destinations
            .enabled_credential(self.user_id, provider_id)
            .await
            .map_err(identity_failure)
    }
}

fn status_router(principal: Uuid, delivery: Arc<DeliveryService>, pool: &PgPool) -> Router {
    let key = CredentialKey::from_base64(&STANDARD_NO_PAD.encode([11u8; 32])).expect("a test key");
    let store = Arc::new(support::credentials::MemoryCredentialStore::new());
    store.insert_user(User {
        id: UserId::from_uuid(principal),
        username: "flynn".to_owned(),
        display_name: "Flynn".to_owned(),
        role: UserRole::Member,
    });
    store.insert_app_password(&key, STATUS_PASSWORD, UserId::from_uuid(principal));
    let credential = Arc::new(CredentialService::new(store, key));
    let ports = support::StorePorts::new(pool.clone());
    let (search, library) = ports.bare_catalogue();
    let playback = ports.streaming(Vec::new());
    let state = whio_core::app::AppState::new(credential, search, library, playback)
        .with_delivery(delivery);

    whio_core::app::router(state)
}

fn status_request(path: &str) -> Request<Body> {
    Request::builder()
        .method("GET")
        .uri(format!(
            "{path}{}u=flynn&v=1.16.1&c=integration-test&p={STATUS_PASSWORD}",
            if path.contains('?') { '&' } else { '?' }
        ))
        .header("x-request-id", "request-123")
        .body(Body::empty())
        .unwrap()
}

async fn status_response(app: &Router, request: Request<Body>) -> (StatusCode, Value) {
    let response = app
        .clone()
        .oneshot(request)
        .await
        .expect("the status route answers");
    let status = response.status();
    let bytes = to_bytes(response.into_body(), 65_536)
        .await
        .expect("a response body");
    let value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or_else(|error| {
            panic!(
                "status {status} answered a non-JSON body: {error}: {:?}",
                String::from_utf8_lossy(&bytes)
            )
        })
    };

    (status, value)
}

#[sqlx::test]
async fn the_controlled_host_receives_the_exact_contract_body(pool: PgPool) {
    prepare_awa_schema(&pool).await;
    let host = DestinationHost::start(200, r#"{"status":"ok"}"#).await;
    let principal = Uuid::now_v7();
    insert_user(&pool, principal, "flynn").await;
    let track = create_track(
        &PostgresTrackRepository::new(pool.clone()),
        "Blue Monday",
        Some("rec-1"),
    )
    .await;
    attach_release(&pool, &track, "release-1").await;
    let destination_id = Uuid::now_v7();
    let (store, delivery) = service(&pool, &host, destination_id, true);
    let listen_at = chrono::Utc::now();

    store
        .submit_listen(&submission(principal, &track, listen_at))
        .await
        .unwrap();
    let row = pending_row(&pool, principal).await;

    assert_eq!(
        delivery.deliver(row).await.unwrap(),
        DeliveryOutcome::Delivered
    );

    let requests = host.requests();

    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].1, "Token test-token");
    assert_eq!(
        requests[0].0,
        json!({
            "listened_at": listen_at.timestamp(),
            "artist_name": "New Order",
            "track_name": "Blue Monday",
            "recording_mbid": "rec-1",
            "release_mbid": "release-1",
            "release_name": "Power, Corruption & Lies",
            "duration_ms": 240_000u64,
            "client": whio_core::features::music_library::delivery::SUBMISSION_CLIENT,
        })
    );
    assert!(!requests[0].0.to_string().contains("test-token"));
    assert_eq!(delivery_state(&pool, row).await, "delivered");
}

#[sqlx::test]
async fn a_name_only_track_yields_a_valid_payload_over_http(pool: PgPool) {
    prepare_awa_schema(&pool).await;
    let host = DestinationHost::start(200, r#"{"status":"ok"}"#).await;
    let principal = Uuid::now_v7();
    insert_user(&pool, principal, "flynn").await;
    let track = create_track(
        &PostgresTrackRepository::new(pool.clone()),
        "Ceremony",
        None,
    )
    .await;
    let destination_id = Uuid::now_v7();
    let (store, delivery) = service(&pool, &host, destination_id, true);

    store
        .submit_listen(&submission(principal, &track, chrono::Utc::now()))
        .await
        .unwrap();
    let row = pending_row(&pool, principal).await;

    assert_eq!(
        delivery.deliver(row).await.unwrap(),
        DeliveryOutcome::Delivered
    );

    let body = &host.requests()[0].0;

    assert_eq!(body["track_name"], json!("Ceremony"));
    assert_eq!(body["artist_name"], json!("New Order"));
    assert!(body.get("recording_mbid").is_none());
    assert!(body.get("release_mbid").is_none());
    assert!(body.get("release_name").is_none());
}

#[sqlx::test]
async fn a_rejected_credential_over_http_is_terminal(pool: PgPool) {
    prepare_awa_schema(&pool).await;
    let host = DestinationHost::start(401, r#"{"error":"invalid token"}"#).await;
    let principal = Uuid::now_v7();
    insert_user(&pool, principal, "flynn").await;
    let track = create_track(
        &PostgresTrackRepository::new(pool.clone()),
        "Blue Monday",
        None,
    )
    .await;
    let destination_id = Uuid::now_v7();
    let (store, delivery) = service(&pool, &host, destination_id, true);

    store
        .submit_listen(&submission(principal, &track, chrono::Utc::now()))
        .await
        .unwrap();
    let row = pending_row(&pool, principal).await;

    assert!(matches!(
        delivery.deliver(row).await,
        Err(DeliveryError::CredentialRejected)
    ));
    assert_eq!(delivery_state(&pool, row).await, "rejected");
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
    assert_eq!(host.requests().len(), 1);
}

#[sqlx::test]
async fn a_rate_limit_over_http_snoozes_without_consuming_an_attempt(pool: PgPool) {
    prepare_awa_schema(&pool).await;
    let host = DestinationHost::start(429, r#"{"error":"slow down"}"#).await;
    let principal = Uuid::now_v7();
    insert_user(&pool, principal, "flynn").await;
    let track = create_track(
        &PostgresTrackRepository::new(pool.clone()),
        "Blue Monday",
        None,
    )
    .await;
    let destination_id = Uuid::now_v7();
    let (store, delivery) = service(&pool, &host, destination_id, true);

    store
        .submit_listen(&submission(principal, &track, chrono::Utc::now()))
        .await
        .unwrap();
    let row = pending_row(&pool, principal).await;

    match delivery.deliver(row).await {
        Err(DeliveryError::RateLimited { retry_after }) => {
            assert_eq!(retry_after, Some(Duration::from_secs(7)));
        }
        other => panic!("expected a rate limit, got {other:?}"),
    }

    assert_eq!(attempt_count(&pool, row).await, 0);
    assert_eq!(delivery_state(&pool, row).await, "pending");
}

#[sqlx::test]
async fn a_disabled_destination_submits_nothing_over_http(pool: PgPool) {
    prepare_awa_schema(&pool).await;
    let host = DestinationHost::start(200, r#"{"status":"ok"}"#).await;
    let principal = Uuid::now_v7();
    insert_user(&pool, principal, "flynn").await;
    let track = create_track(
        &PostgresTrackRepository::new(pool.clone()),
        "Blue Monday",
        None,
    )
    .await;
    let destination_id = Uuid::now_v7();
    let (store, delivery) = service(&pool, &host, destination_id, true);

    store
        .submit_listen(&submission(principal, &track, chrono::Utc::now()))
        .await
        .unwrap();
    let row = pending_row(&pool, principal).await;

    let (_, disabled) = service(&pool, &host, destination_id, false);

    assert!(matches!(
        disabled.deliver(row).await,
        Err(DeliveryError::Disabled)
    ));
    assert!(host.requests().is_empty());
    assert_eq!(delivery_state(&pool, row).await, "pending");
    assert_eq!(
        delivery.deliver(row).await.unwrap(),
        DeliveryOutcome::Delivered
    );
}

#[sqlx::test]
async fn a_pending_row_delivers_once_after_a_restart(pool: PgPool) {
    prepare_awa_schema(&pool).await;
    let host = DestinationHost::start(200, r#"{"status":"ok"}"#).await;
    let principal = Uuid::now_v7();
    insert_user(&pool, principal, "flynn").await;
    let track = create_track(
        &PostgresTrackRepository::new(pool.clone()),
        "Blue Monday",
        None,
    )
    .await;
    let destination_id = Uuid::now_v7();
    let (store, delivery) = service(&pool, &host, destination_id, true);

    store
        .submit_listen(&submission(principal, &track, chrono::Utc::now()))
        .await
        .unwrap();
    let row = pending_row(&pool, principal).await;

    let (_, restarted) = service(&pool, &host, destination_id, true);

    assert_eq!(
        restarted.deliver(row).await.unwrap(),
        DeliveryOutcome::Delivered
    );
    assert_eq!(
        restarted.deliver(row).await.unwrap(),
        DeliveryOutcome::AlreadyTerminal
    );
    assert_eq!(
        delivery.deliver(row).await.unwrap(),
        DeliveryOutcome::AlreadyTerminal
    );
    assert_eq!(host.requests().len(), 1);
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM awa.jobs WHERE kind = 'whio.listening.deliver.v1' AND \
             args->>'delivery_id' = $1"
        )
        .bind(row.to_string())
        .fetch_one(&pool)
        .await
        .unwrap(),
        1
    );
}

#[sqlx::test]
async fn a_duplicate_listen_submits_once_over_http(pool: PgPool) {
    prepare_awa_schema(&pool).await;
    let host = DestinationHost::start(200, r#"{"status":"ok"}"#).await;
    let principal = Uuid::now_v7();
    insert_user(&pool, principal, "flynn").await;
    let track = create_track(
        &PostgresTrackRepository::new(pool.clone()),
        "Blue Monday",
        None,
    )
    .await;
    let destination_id = Uuid::now_v7();
    let (store, delivery) = service(&pool, &host, destination_id, true);
    let listen = submission(principal, &track, chrono::Utc::now());

    assert_eq!(
        store.submit_listen(&listen).await.unwrap(),
        ListenOutcome::Recorded
    );
    assert_eq!(
        store.submit_listen(&listen).await.unwrap(),
        ListenOutcome::Duplicate
    );

    let row = pending_row(&pool, principal).await;

    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM listen_deliveries")
            .fetch_one(&pool)
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        delivery.deliver(row).await.unwrap(),
        DeliveryOutcome::Delivered
    );
    assert_eq!(host.requests().len(), 1);
}

#[sqlx::test]
async fn the_ledger_is_unchanged_by_a_delivered_listen(pool: PgPool) {
    prepare_awa_schema(&pool).await;
    let host = DestinationHost::start(200, r#"{"status":"ok"}"#).await;
    let principal = Uuid::now_v7();
    insert_user(&pool, principal, "flynn").await;
    let track = create_track(
        &PostgresTrackRepository::new(pool.clone()),
        "Blue Monday",
        None,
    )
    .await;
    let destination_id = Uuid::now_v7();
    let (store, delivery) = service(&pool, &host, destination_id, true);

    store
        .submit_listen(&submission(principal, &track, chrono::Utc::now()))
        .await
        .unwrap();

    let before = sqlx::query_scalar::<_, Value>(
        "SELECT jsonb_build_object( \
           'listens', (SELECT count(*) FROM listens WHERE user_id = $1), \
           'plays', (SELECT COALESCE(SUM(play_count), 0) FROM track_play_stats WHERE user_id = $1), \
           'marks', (SELECT started_at FROM listen_marks WHERE user_id = $1))",
    )
    .bind(principal)
    .fetch_one(&pool)
    .await
    .unwrap();

    let row = pending_row(&pool, principal).await;

    assert_eq!(
        delivery.deliver(row).await.unwrap(),
        DeliveryOutcome::Delivered
    );

    let after = sqlx::query_scalar::<_, Value>(
        "SELECT jsonb_build_object( \
           'listens', (SELECT count(*) FROM listens WHERE user_id = $1), \
           'plays', (SELECT COALESCE(SUM(play_count), 0) FROM track_play_stats WHERE user_id = $1), \
           'marks', (SELECT started_at FROM listen_marks WHERE user_id = $1))",
    )
    .bind(principal)
    .fetch_one(&pool)
    .await
    .unwrap();

    assert_eq!(after, before);
    assert_eq!(after["plays"], json!(1));
}

#[sqlx::test]
async fn the_status_route_projection_is_scoped_and_credential_free(pool: PgPool) {
    prepare_awa_schema(&pool).await;
    let host = DestinationHost::start(200, r#"{"status":"ok"}"#).await;
    let principal = Uuid::now_v7();
    insert_user(&pool, principal, "flynn").await;
    let track = create_track(
        &PostgresTrackRepository::new(pool.clone()),
        "Blue Monday",
        None,
    )
    .await;
    let destination_id = Uuid::now_v7();
    let (store, delivery) = service(&pool, &host, destination_id, true);

    store
        .submit_listen(&submission(principal, &track, chrono::Utc::now()))
        .await
        .unwrap();
    let row = pending_row(&pool, principal).await;

    assert_eq!(
        delivery.deliver(row).await.unwrap(),
        DeliveryOutcome::Delivered
    );

    let statuses = delivery.status(principal, None, 50).await.unwrap();

    assert_eq!(statuses.len(), 1);

    let value = serde_json::to_value(&statuses[0]).unwrap();

    assert_eq!(value["state"], json!("delivered"));
    assert_eq!(value["provider_idempotence"], json!("timestamp_only"));
    assert!(value.get("token").is_none());
    assert!(value.get("credential").is_none());
    assert!(
        delivery
            .status(Uuid::now_v7(), None, 50)
            .await
            .unwrap()
            .is_empty()
    );
}

#[sqlx::test]
async fn a_transient_503_is_retryable_through_deliver(pool: PgPool) {
    prepare_awa_schema(&pool).await;
    let host = DestinationHost::start(503, r#"{"error":"later"}"#).await;
    let principal = Uuid::now_v7();
    insert_user(&pool, principal, "flynn").await;
    let track = create_track(
        &PostgresTrackRepository::new(pool.clone()),
        "Blue Monday",
        None,
    )
    .await;
    let destination_id = Uuid::now_v7();
    let (store, delivery) = service(&pool, &host, destination_id, true);

    store
        .submit_listen(&submission(principal, &track, chrono::Utc::now()))
        .await
        .unwrap();
    let row = pending_row(&pool, principal).await;

    assert_eq!(
        delivery.deliver(row).await.unwrap(),
        DeliveryOutcome::Retryable
    );
    assert_eq!(host.requests().len(), 1);
    assert_eq!(delivery_state(&pool, row).await, "pending");
    assert_eq!(attempt_count(&pool, row).await, 1);
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
}

#[sqlx::test]
async fn the_status_route_is_scoped_filtered_and_credential_free(pool: PgPool) {
    prepare_awa_schema(&pool).await;
    let host = DestinationHost::start(200, r#"{"status":"ok"}"#).await;
    let principal = Uuid::now_v7();
    let other = Uuid::now_v7();
    insert_user(&pool, principal, "flynn").await;
    insert_user(&pool, other, "sabine").await;
    let tracks = PostgresTrackRepository::new(pool.clone());
    let first = create_track(&tracks, "Blue Monday", None).await;
    let second = create_track(&tracks, "Ceremony", None).await;
    let destination_id = Uuid::now_v7();
    let (store, delivery) = service(&pool, &host, destination_id, true);
    let (other_store, _) = service(&pool, &host, Uuid::now_v7(), true);

    store
        .submit_listen(&submission(principal, &first, chrono::Utc::now()))
        .await
        .unwrap();
    store
        .submit_listen(&submission(principal, &second, chrono::Utc::now()))
        .await
        .unwrap();
    other_store
        .submit_listen(&submission(other, &first, chrono::Utc::now()))
        .await
        .unwrap();

    let app = status_router(principal, Arc::clone(&delivery), &pool);

    let (status, body) =
        status_response(&app, status_request("/api/v1/library/listen-deliveries")).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["total"], json!(2));
    let items = body["items"].as_array().expect("a page");
    assert_eq!(items.len(), 2);

    for item in items {
        assert_eq!(item["destination_id"], json!(destination_id.to_string()));
        assert_eq!(item["provider_id"], json!("listenbrainz"));
        assert_eq!(item["provider_idempotence"], json!("timestamp_only"));
        assert_eq!(item["state"], json!("pending"));
        assert!(item.get("token").is_none());
        assert!(item.get("credential").is_none());
        assert!(item.get("encrypted_secret").is_none());
    }

    let (status, body) = status_response(
        &app,
        status_request("/api/v1/library/listen-deliveries?state=pending&limit=1"),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["total"], json!(1));
    assert_eq!(body["items"].as_array().expect("a page").len(), 1);

    let (status, body) = status_response(
        &app,
        status_request("/api/v1/library/listen-deliveries?state=delivered"),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["total"], json!(0));
    assert_eq!(body["items"].as_array().expect("a page").len(), 0);

    let (status, body) = status_response(
        &app,
        status_request("/api/v1/library/listen-deliveries?state=unknown"),
    )
    .await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["code"], json!("invalid_request"));

    let (status, body) = status_response(
        &app,
        status_request("/api/v1/library/listen-deliveries?unknown=1"),
    )
    .await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["code"], json!("invalid_request"));

    let (status, body) = status_response(
        &app,
        status_request("/api/v1/library/listen-deliveries?limit=0"),
    )
    .await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["code"], json!("invalid_request"));
}

#[sqlx::test]
async fn a_rejected_row_is_reported_through_the_status_route(pool: PgPool) {
    prepare_awa_schema(&pool).await;
    let host = DestinationHost::start(401, r#"{"error":"invalid token"}"#).await;
    let principal = Uuid::now_v7();
    insert_user(&pool, principal, "flynn").await;
    let track = create_track(
        &PostgresTrackRepository::new(pool.clone()),
        "Blue Monday",
        None,
    )
    .await;
    let destination_id = Uuid::now_v7();
    let (store, delivery) = service(&pool, &host, destination_id, true);

    store
        .submit_listen(&submission(principal, &track, chrono::Utc::now()))
        .await
        .unwrap();
    let row = pending_row(&pool, principal).await;

    assert!(matches!(
        delivery.deliver(row).await,
        Err(DeliveryError::CredentialRejected)
    ));

    let app = status_router(principal, Arc::clone(&delivery), &pool);
    let (status, body) = status_response(
        &app,
        status_request("/api/v1/library/listen-deliveries?state=rejected"),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["total"], json!(1));

    let item = &body["items"][0];
    assert_eq!(item["state"], json!("rejected"));
    assert_eq!(
        item["last_error"],
        json!("the destination rejected the stored credential")
    );

    let mut members = item
        .as_object()
        .expect("a status object")
        .keys()
        .map(String::as_str)
        .collect::<Vec<_>>();
    members.sort_unstable();

    assert_eq!(
        members,
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

#[sqlx::test]
async fn a_credential_rejection_leaves_the_identity_destination_untouched(pool: PgPool) {
    prepare_awa_schema(&pool).await;
    let host = DestinationHost::start(401, r#"{"error":"invalid token"}"#).await;
    let principal = Uuid::now_v7();
    insert_user(&pool, principal, "flynn").await;
    let track = create_track(
        &PostgresTrackRepository::new(pool.clone()),
        "Blue Monday",
        None,
    )
    .await;
    let key = CredentialKey::from_base64(&STANDARD_NO_PAD.encode([11u8; 32])).expect("a test key");
    let destinations = Arc::new(whio_core::features::identity::destinations(
        pool.clone(),
        Some(key),
        Arc::new(AcceptingValidator),
    ));
    let caller = Principal {
        user_id: UserId::from_uuid(principal),
        role: UserRole::Member,
    };

    destinations
        .connect(&caller, "listenbrainz", DESTINATION_TOKEN)
        .await
        .expect("the destination connects");
    let destination_id = destinations.enabled_destinations(principal).await.unwrap()[0].0;

    let reader = Arc::new(PostgresTrackRepository::new(pool.clone()));
    let delivery = Arc::new(DeliveryService::new(
        Arc::clone(&reader) as Arc<dyn DeliveryStore>,
        Arc::clone(&reader) as Arc<dyn ListenSource>,
        Arc::new(HttpSender::new(host.address)),
        Arc::new(IdentityDestinations {
            destinations: Arc::clone(&destinations),
            user_id: principal,
        }),
    ));
    let store =
        Arc::new(PostgresTrackRepository::new(pool.clone()).with_delivery(Arc::clone(&delivery)));

    store
        .submit_listen(&submission(principal, &track, chrono::Utc::now()))
        .await
        .unwrap();
    let row = pending_row(&pool, principal).await;

    assert_eq!(host.requests().len(), 0);

    assert!(matches!(
        delivery.deliver(row).await,
        Err(DeliveryError::CredentialRejected)
    ));
    assert_eq!(host.requests().len(), 1);
    assert_eq!(delivery_state(&pool, row).await, "rejected");

    let (enabled, revoked) = sqlx::query_as::<_, (bool, Option<chrono::DateTime<chrono::Utc>>)>(
        "SELECT enabled, revoked_at FROM destination_accounts WHERE id = $1",
    )
    .bind(destination_id)
    .fetch_one(&pool)
    .await
    .unwrap();

    assert!(enabled);
    assert!(revoked.is_none());
    assert_eq!(
        destinations
            .enabled_destinations(principal)
            .await
            .unwrap()
            .len(),
        1
    );
    assert!(
        destinations
            .enabled_credential(principal, "listenbrainz")
            .await
            .unwrap()
            .is_some()
    );
}
