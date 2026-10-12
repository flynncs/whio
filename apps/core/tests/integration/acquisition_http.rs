use std::{path::PathBuf, sync::Arc, time::Duration};

use async_trait::async_trait;
use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode},
    response::Response,
};
use serde_json::{Value, json};
use sqlx::PgPool;
use tower::ServiceExt;
use whio_core::features::acquisition::config::AcquisitionConfig;
use whio_core::features::acquisition::domain::RequestState;
use whio_core::features::acquisition::ports::{
    AcquisitionTarget, BackendHealth, CandidateLimit, CandidateSearch, ImportError, ImportJob,
    ImportOutcome, Importer, LibraryOwnership, LookupError, MbId, MbRecording, MbRelease,
    MusicBrainzLookup, OwnershipError, ReleaseCandidate, RemoteTransfer, SearchText, Slot,
    TransferError, TransferHandle, TransferProgress, TransferRequest,
};
use whio_core::features::acquisition::service::AcquisitionPorts;
use whio_core::features::music_library::catalogue::CompletionService;

use crate::support;

struct NoSearch;

#[async_trait]
impl CandidateSearch for NoSearch {
    async fn search(
        &self,
        _query: &SearchText,
        _limit: CandidateLimit,
    ) -> Result<Vec<ReleaseCandidate>, TransferError> {
        Ok(Vec::new())
    }
}

struct NoTransfer;

#[async_trait]
impl RemoteTransfer for NoTransfer {
    async fn start(
        &self,
        _requests: &[TransferRequest],
    ) -> Result<Vec<Slot<TransferHandle>>, TransferError> {
        Ok(Vec::new())
    }

    async fn status(
        &self,
        _handles: &[TransferHandle],
    ) -> Result<Vec<Slot<TransferProgress>>, TransferError> {
        Ok(Vec::new())
    }

    async fn cancel(&self, _handle: &TransferHandle, _remove: bool) -> Result<(), TransferError> {
        Ok(())
    }

    async fn health(&self) -> BackendHealth {
        BackendHealth::Ready
    }
}

struct NoLookup;

#[async_trait]
impl MusicBrainzLookup for NoLookup {
    async fn release_candidates(
        &self,
        _artist: &str,
        _title: &str,
    ) -> Result<Vec<MbRelease>, LookupError> {
        Ok(Vec::new())
    }

    async fn release(&self, _id: &MbId) -> Result<Option<MbRelease>, LookupError> {
        Ok(None)
    }

    async fn recording_candidates(
        &self,
        _artist: &str,
        _title: &str,
    ) -> Result<Vec<MbRecording>, LookupError> {
        Ok(Vec::new())
    }

    async fn recording(&self, _id: &MbId) -> Result<Option<MbRecording>, LookupError> {
        Ok(None)
    }
}

struct NoImporter;

#[async_trait]
impl Importer for NoImporter {
    async fn import(&self, _job: &ImportJob) -> Result<ImportOutcome, ImportError> {
        Ok(ImportOutcome { items: Vec::new() })
    }
}

struct NoOwnership;

#[async_trait]
impl LibraryOwnership for NoOwnership {
    async fn owns(&self, _target: &AcquisitionTarget) -> Result<bool, OwnershipError> {
        Ok(false)
    }
}

struct Fixture {
    app: Router,
    pool: PgPool,
    admin: String,
    member: String,
}

async fn fixture(pool: PgPool, enabled: bool) -> Fixture {
    awa::migrations::run(&pool).await.expect("the awa schema");

    let roles = support::credentials::credential_service_with_roles();
    let ports = support::StorePorts::new(pool.clone());
    let (search, library) = ports.bare_catalogue();
    let playback = ports.streaming(Vec::new());
    let completion = Arc::new(CompletionService::disabled(
        Arc::clone(&library),
        Arc::clone(&search),
    ));
    let service = enabled.then(|| {
        whio_core::features::acquisition::test_support::service(
            pool.clone(),
            AcquisitionPorts::new(
                Arc::new(NoSearch),
                Arc::new(NoTransfer),
                Arc::new(NoLookup),
                Arc::new(NoImporter),
                Arc::new(NoOwnership),
                completion,
            ),
            Arc::clone(&library),
            AcquisitionConfig {
                enabled: true,
                staging_dir: Some(PathBuf::from("/staging")),
                library_dir: Some(PathBuf::from("/staging")),
                candidate_budget: 3,
                race_width: 1,
                start_timeout: Duration::from_secs(600),
                transfer_timeout: Duration::from_secs(1800),
                poll: Duration::from_secs(10),
            },
        )
    });
    let app = whio_core::app::router(
        whio_core::app::AppState::new(roles.service.clone(), search, library, playback)
            .with_acquisition(service),
    );

    Fixture {
        app,
        pool,
        admin: format!(
            "u={}&v=1.16.1&c=integration-test&p={}",
            roles.admin, roles.admin_secret
        ),
        member: format!(
            "u={}&v=1.16.1&c=integration-test&p={}",
            roles.member, roles.member_secret
        ),
    }
}

fn with_auth(uri: &str, auth: &str) -> String {
    let separator = if uri.contains('?') { '&' } else { '?' };

    format!("{uri}{separator}{auth}")
}

async fn send(app: &Router, method: &str, uri: &str, auth: &str, body: Option<&Value>) -> Response {
    let builder = Request::builder()
        .method(method)
        .uri(with_auth(uri, auth))
        .header("x-request-id", support::TEST_REQUEST_ID);
    let request = match body {
        Some(body) => builder
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap(),
        None => builder.body(Body::empty()).unwrap(),
    };

    app.clone().oneshot(request).await.unwrap()
}

async fn json_body(response: Response) -> Value {
    let bytes = to_bytes(response.into_body(), 16 * 1024 * 1024)
        .await
        .unwrap();

    serde_json::from_slice(&bytes).unwrap()
}

const ORIGIN: &str = "11111111-1111-1111-1111-111111111111";

#[sqlx::test]
async fn a_non_admin_is_forbidden(pool: PgPool) {
    let fixture = fixture(pool, true).await;

    let response = send(
        &fixture.app,
        "POST",
        "/api/v1/acquisition/requests",
        &fixture.member,
        Some(&json!({"musicbrainz_release_id": ORIGIN})),
    )
    .await;

    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    assert_eq!(json_body(response).await["code"], "forbidden");
}

#[sqlx::test]
async fn exactly_one_origin_key_is_required(pool: PgPool) {
    let fixture = fixture(pool, true).await;

    for body in [
        json!({}),
        json!({"album_id": ORIGIN, "track_id": ORIGIN}),
        json!({"musicbrainz_release_id": "not-a-uuid"}),
    ] {
        let response = send(
            &fixture.app,
            "POST",
            "/api/v1/acquisition/requests",
            &fixture.admin,
            Some(&body),
        )
        .await;

        assert_eq!(
            response.status(),
            StatusCode::UNPROCESSABLE_ENTITY,
            "{body}"
        );
    }
}

#[sqlx::test]
async fn an_unknown_album_is_not_found(pool: PgPool) {
    let fixture = fixture(pool, true).await;

    let response = send(
        &fixture.app,
        "POST",
        "/api/v1/acquisition/requests",
        &fixture.admin,
        Some(&json!({"album_id": "22222222-2222-2222-2222-222222222222"})),
    )
    .await;

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[sqlx::test]
async fn a_new_request_is_created(pool: PgPool) {
    let fixture = fixture(pool, true).await;

    let response = send(
        &fixture.app,
        "POST",
        "/api/v1/acquisition/requests",
        &fixture.admin,
        Some(&json!({"musicbrainz_release_id": ORIGIN})),
    )
    .await;

    assert_eq!(response.status(), StatusCode::CREATED);
    let body = json_body(response).await;
    assert_eq!(body["state"], "pending");
    assert_eq!(body["origin"]["kind"], "release");
    assert_eq!(body["origin"]["id"], ORIGIN);
    assert_eq!(body["target"], Value::Null);
    assert_eq!(body["attempts"], 0);

    let again = send(
        &fixture.app,
        "POST",
        "/api/v1/acquisition/requests",
        &fixture.admin,
        Some(&json!({"musicbrainz_release_id": ORIGIN})),
    )
    .await;
    assert_eq!(again.status(), StatusCode::OK);
    assert_eq!(json_body(again).await["id"], body["id"]);
}

#[sqlx::test]
async fn listing_returns_the_latest_per_origin(pool: PgPool) {
    let fixture = fixture(pool, true).await;
    let created = send(
        &fixture.app,
        "POST",
        "/api/v1/acquisition/requests",
        &fixture.admin,
        Some(&json!({"musicbrainz_release_id": ORIGIN})),
    )
    .await;
    let first = json_body(created).await["id"].as_str().unwrap().to_owned();
    whio_core::features::acquisition::test_support::mark_terminal(
        fixture.pool.clone(),
        uuid::Uuid::parse_str(&first).unwrap(),
        RequestState::Exhausted,
    )
    .await
    .unwrap();

    let second = send(
        &fixture.app,
        "POST",
        "/api/v1/acquisition/requests",
        &fixture.admin,
        Some(&json!({"musicbrainz_release_id": ORIGIN})),
    )
    .await;
    let latest = json_body(second).await["id"].as_str().unwrap().to_owned();

    let response = send(
        &fixture.app,
        "GET",
        &format!("/api/v1/acquisition/requests?origin={ORIGIN}&origin=33333333-3333-3333-3333-333333333333"),
        &fixture.admin,
        None,
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    let body = json_body(response).await;
    let items = body.as_array().unwrap();
    assert_eq!(items.len(), 1, "origins without a request are absent");
    assert_eq!(items[0]["id"], latest);
    assert_ne!(items[0]["id"], first);
}

#[sqlx::test]
async fn disabled_acquisition_is_503(pool: PgPool) {
    let fixture = fixture(pool, false).await;

    let response = send(
        &fixture.app,
        "POST",
        "/api/v1/acquisition/requests",
        &fixture.admin,
        Some(&json!({"musicbrainz_release_id": ORIGIN})),
    )
    .await;

    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(json_body(response).await["code"], "acquisition_disabled");
}

#[sqlx::test]
async fn extra_null_keys_are_invalid(pool: PgPool) {
    let fixture = fixture(pool, true).await;

    for body in [
        json!({"album_id": null, "musicbrainz_release_id": ORIGIN}),
        json!({"album_id": null}),
        json!({"musicbrainz_release_id": null}),
    ] {
        let response = send(
            &fixture.app,
            "POST",
            "/api/v1/acquisition/requests",
            &fixture.admin,
            Some(&body),
        )
        .await;

        assert_eq!(
            response.status(),
            StatusCode::UNPROCESSABLE_ENTITY,
            "{body}"
        );
    }
}

#[sqlx::test]
async fn a_member_cannot_read_requests(pool: PgPool) {
    let fixture = fixture(pool, true).await;

    let list = send(
        &fixture.app,
        "GET",
        &format!("/api/v1/acquisition/requests?origin={ORIGIN}"),
        &fixture.member,
        None,
    )
    .await;
    let detail = send(
        &fixture.app,
        "GET",
        &format!("/api/v1/acquisition/requests/{ORIGIN}"),
        &fixture.member,
        None,
    )
    .await;

    assert_eq!(list.status(), StatusCode::FORBIDDEN);
    assert_eq!(detail.status(), StatusCode::FORBIDDEN);
}

#[sqlx::test]
async fn a_rejected_body_leaks_nothing_into_the_logs(pool: PgPool) {
    use tracing::instrument::WithSubscriber;
    use tracing_subscriber::layer::SubscriberExt;

    support::keep_callsites_interested();
    let fixture = fixture(pool, true).await;
    let capture = support::Capture::default();
    let subscriber = tracing_subscriber::registry()
        .with(
            tracing_subscriber::fmt::layer()
                .with_writer(capture.clone())
                .with_ansi(false),
        )
        .with(tracing_subscriber::EnvFilter::new("info"));
    let dispatch = tracing::Dispatch::new(subscriber);

    let response = async {
        send(
            &fixture.app,
            "POST",
            "/api/v1/acquisition/requests",
            &fixture.admin,
            Some(&json!({
                "musicbrainz_release_id": ORIGIN,
                "super_secret_field": "hunter2"
            })),
        )
        .await
    }
    .with_subscriber(dispatch)
    .await;

    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(
        response
            .headers()
            .get("x-request-id")
            .and_then(|value| value.to_str().ok()),
        Some(support::TEST_REQUEST_ID),
        "the selected request reached the request-id middleware"
    );
    let logs = String::from_utf8_lossy(&capture.0.lock().unwrap()).into_owned();
    let selected_event = logs
        .lines()
        .find(|line| line.contains("http request") && line.contains(support::TEST_REQUEST_ID));
    assert!(
        selected_event.is_some(),
        "the captured middleware event contains the selected validated request id: {logs}"
    );
    assert!(!logs.contains("super_secret_field"));
    assert!(!logs.contains("hunter2"));
}

#[sqlx::test]
async fn equivalent_uuid_spellings_find_the_same_request(pool: PgPool) {
    let fixture = fixture(pool, true).await;
    let upper = ORIGIN.to_uppercase();

    let created = send(
        &fixture.app,
        "POST",
        "/api/v1/acquisition/requests",
        &fixture.admin,
        Some(&json!({"musicbrainz_release_id": upper})),
    )
    .await;
    assert_eq!(created.status(), StatusCode::CREATED);

    let listed = send(
        &fixture.app,
        "GET",
        &format!("/api/v1/acquisition/requests?origin={upper}"),
        &fixture.admin,
        None,
    )
    .await;

    assert_eq!(listed.status(), StatusCode::OK);
    assert_eq!(json_body(listed).await.as_array().unwrap().len(), 1);
}
