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
    http::{Request, StatusCode, header},
    response::IntoResponse,
    routing::post,
};
use base64::Engine;
use base64::engine::general_purpose::STANDARD_NO_PAD;
use serde_json::{Value, json};
use sqlx::PgPool;
use tower::ServiceExt;
use uuid::Uuid;
use whio_core::{
    app::AppState,
    features::identity::{
        self,
        destinations::{
            DestinationProvider, DestinationService, DestinationValidator, ValidatedDestination,
            ValidationError,
        },
        domain::{User, UserId, UserRole},
        secrets::CredentialKey,
        service::CredentialService,
    },
    integrations::listenbrainz::{ListenBrainzAdapter, SUBMIT_PATH, VALIDATE_PATH},
    platform::pacing::CallLimiter,
};

const PASSWORD: &str = "whio-test-password";
const TOKEN: &str = "secret-destination-token";
const NONCE_LEN: usize = 12;

fn credential_key() -> CredentialKey {
    CredentialKey::from_base64(&STANDARD_NO_PAD.encode([11u8; 32])).expect("valid test key")
}

fn credential_service(users: &[(Uuid, &str)]) -> Arc<CredentialService> {
    let key = credential_key();
    let store = Arc::new(support::credentials::MemoryCredentialStore::new());

    for (principal, username) in users {
        store.insert_user(User {
            id: UserId::from_uuid(*principal),
            username: (*username).to_owned(),
            display_name: (*username).to_owned(),
            role: UserRole::Member,
        });
        store.insert_app_password(&key, PASSWORD, UserId::from_uuid(*principal));
    }

    Arc::new(CredentialService::new(store, key))
}

fn router(users: &[(Uuid, &str)], pool: &PgPool, destinations: Arc<DestinationService>) -> Router {
    let ports = support::StorePorts::new(pool.clone());
    let (search, library) = ports.bare_catalogue();
    let playback = ports.streaming(Vec::new());
    let state = AppState::new(credential_service(users), search, library, playback)
        .with_destinations(destinations);

    whio_core::app::router(state)
}

struct FakeValidator {
    calls: Mutex<Vec<(DestinationProvider, String)>>,
    answer: Mutex<Result<ValidatedDestination, ValidationError>>,
}

impl FakeValidator {
    fn accepting(username: &str) -> Arc<Self> {
        Arc::new(Self {
            calls: Mutex::new(Vec::new()),
            answer: Mutex::new(Ok(ValidatedDestination {
                provider_id: DestinationProvider::ListenBrainz,
                username: Some(username.to_owned()),
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

fn service(pool: &PgPool, validator: Arc<dyn DestinationValidator>) -> Arc<DestinationService> {
    Arc::new(identity::destinations(
        pool.clone(),
        Some(credential_key()),
        validator,
    ))
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

fn request(method: &str, path: &str, username: &str, body: &str) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(format!(
            "{path}{}u={username}&v=1.16.1&c=integration-test&p={PASSWORD}",
            if path.contains('?') { '&' } else { '?' }
        ))
        .header("content-type", "application/json")
        .header("x-request-id", "request-123")
        .body(Body::from(body.to_owned()))
        .unwrap()
}

fn connect_request(username: &str, provider_id: &str, token: &str) -> Request<Body> {
    request(
        "POST",
        "/api/v1/identity/destinations",
        username,
        &json!({ "provider_id": provider_id, "token": token }).to_string(),
    )
}

async fn send(app: &Router, request: Request<Body>) -> (StatusCode, Value) {
    let response = app
        .clone()
        .oneshot(request)
        .await
        .expect("the destination router answers");
    let status = response.status();
    let bytes = to_bytes(response.into_body(), 65_536)
        .await
        .expect("a response body");
    let value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).expect("a JSON response body")
    };

    (status, value)
}

async fn stored_secret(pool: &PgPool, user_id: Uuid) -> Vec<u8> {
    sqlx::query_scalar(
        "SELECT encrypted_secret FROM destination_accounts \
         WHERE user_id = $1 AND provider_id = 'listenbrainz'",
    )
    .bind(user_id)
    .fetch_one(pool)
    .await
    .unwrap()
}

async fn account_rows(pool: &PgPool) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM destination_accounts")
        .fetch_one(pool)
        .await
        .unwrap()
}

fn members(value: &Value) -> Vec<String> {
    let mut members = value
        .as_object()
        .expect("an object")
        .keys()
        .cloned()
        .collect::<Vec<_>>();
    members.sort_unstable();

    members
}

fn nonce(blob: &[u8]) -> &[u8] {
    &blob[1..1 + NONCE_LEN]
}

async fn controlled_destination() -> SocketAddr {
    let app = Router::new().route(VALIDATE_PATH, post(validate)).route(
        SUBMIT_PATH,
        post(|| async { (StatusCode::OK, r#"{"status":"ok"}"#) }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("a controlled destination port");
    let address = listener.local_addr().expect("a controlled address");

    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });

    address
}

async fn validate(request: axum::extract::Request) -> impl IntoResponse {
    let token = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();

    match token {
        "Token good-token" => (StatusCode::OK, r#"{"valid":true,"user_name":"flynnteh"}"#),
        "Token unauthorized-token" => (StatusCode::UNAUTHORIZED, "{}"),
        _ => (
            StatusCode::OK,
            r#"{"valid":false,"message":"invalid token"}"#,
        ),
    }
}

fn adapter(address: SocketAddr) -> Arc<dyn DestinationValidator> {
    Arc::new(
        ListenBrainzAdapter::with_config(
            url::Url::parse(&format!("http://{address}/")).expect("a controlled base URL"),
            Duration::from_secs(5),
            Duration::from_secs(5),
            Arc::new(CallLimiter::new(Duration::from_millis(1))),
        )
        .expect("a controlled adapter"),
    )
}

#[sqlx::test]
async fn the_connect_route_stores_ciphertext_and_reports_no_secret(pool: PgPool) {
    let principal = Uuid::now_v7();
    insert_user(&pool, principal, "flynn").await;
    let validator = FakeValidator::accepting("flynnteh");
    let destinations = service(
        &pool,
        Arc::clone(&validator) as Arc<dyn DestinationValidator>,
    );
    let app = router(&[(principal, "flynn")], &pool, Arc::clone(&destinations));

    let (status, body) = send(&app, connect_request("flynn", "listenbrainz", TOKEN)).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        members(&body),
        vec![
            "connected_at",
            "enabled",
            "last_verified_at",
            "provider_id",
            "revoked",
            "username",
        ]
    );
    assert_eq!(body["provider_id"], json!("listenbrainz"));
    assert_eq!(body["username"], json!("flynnteh"));
    assert_eq!(body["enabled"], json!(true));
    assert_eq!(body["revoked"], json!(false));
    assert!(!body.to_string().contains(TOKEN));

    let (status, listed) = send(
        &app,
        request("GET", "/api/v1/identity/destinations", "flynn", ""),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(listed.as_array().expect("a list").len(), 1);
    assert!(!listed.to_string().contains(TOKEN));

    let stored = stored_secret(&pool, principal).await;

    assert_ne!(stored, TOKEN.as_bytes());
    assert!(
        !stored
            .windows(TOKEN.len())
            .any(|window| window == TOKEN.as_bytes()),
        "the plaintext token must not appear in the stored blob"
    );
    assert_eq!(stored.len(), 1 + NONCE_LEN + TOKEN.len() + 16);
    assert_eq!(
        validator.calls(),
        vec![(DestinationProvider::ListenBrainz, TOKEN.to_owned())]
    );
}

#[sqlx::test]
async fn each_principal_owns_one_ciphertext_and_cannot_read_the_other(pool: PgPool) {
    let flynn = Uuid::now_v7();
    let sabine = Uuid::now_v7();
    insert_user(&pool, flynn, "flynn").await;
    insert_user(&pool, sabine, "sabine").await;
    let validator = FakeValidator::accepting("flynn");
    let destinations = service(
        &pool,
        Arc::clone(&validator) as Arc<dyn DestinationValidator>,
    );
    let app = router(
        &[(flynn, "flynn"), (sabine, "sabine")],
        &pool,
        Arc::clone(&destinations),
    );

    assert_eq!(
        send(&app, connect_request("flynn", "listenbrainz", TOKEN))
            .await
            .0,
        StatusCode::OK
    );
    validator.answer(Ok(ValidatedDestination {
        provider_id: DestinationProvider::ListenBrainz,
        username: Some("sabine".to_owned()),
    }));
    assert_eq!(
        send(&app, connect_request("sabine", "listenbrainz", TOKEN))
            .await
            .0,
        StatusCode::OK
    );

    let flynn_blob = stored_secret(&pool, flynn).await;
    let sabine_blob = stored_secret(&pool, sabine).await;

    assert_ne!(flynn_blob, sabine_blob);
    assert_ne!(nonce(&flynn_blob), nonce(&sabine_blob));
    assert_eq!(account_rows(&pool).await, 2);

    let (status, flynn_status) = send(
        &app,
        request("GET", "/api/v1/identity/destinations", "flynn", ""),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(flynn_status.as_array().expect("a list").len(), 1);
    assert_eq!(flynn_status[0]["username"], json!("flynn"));

    let (status, sabine_status) = send(
        &app,
        request("GET", "/api/v1/identity/destinations", "sabine", ""),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(sabine_status.as_array().expect("a list").len(), 1);
    assert_eq!(sabine_status[0]["username"], json!("sabine"));

    assert_eq!(
        destinations
            .enabled_credential(flynn, "listenbrainz")
            .await
            .unwrap()
            .map(|credential| credential.token),
        Some(TOKEN.to_owned())
    );
    assert_eq!(
        destinations
            .enabled_credential(sabine, "listenbrainz")
            .await
            .unwrap()
            .map(|credential| credential.token),
        Some(TOKEN.to_owned())
    );

    assert_eq!(
        send(
            &app,
            request(
                "POST",
                "/api/v1/identity/destinations/listenbrainz/disable",
                "flynn",
                "",
            ),
        )
        .await
        .0,
        StatusCode::OK
    );

    assert!(
        destinations
            .enabled_destinations(flynn)
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        destinations
            .enabled_destinations(sabine)
            .await
            .unwrap()
            .len(),
        1
    );
    assert!(
        destinations
            .enabled_credential(flynn, "listenbrainz")
            .await
            .unwrap()
            .is_none()
    );

    let (_, sabine_status) = send(
        &app,
        request("GET", "/api/v1/identity/destinations", "sabine", ""),
    )
    .await;
    assert_eq!(sabine_status[0]["enabled"], json!(true));
}

#[sqlx::test]
async fn a_two_hundred_valid_false_and_a_401_both_store_nothing(pool: PgPool) {
    let principal = Uuid::now_v7();
    insert_user(&pool, principal, "flynn").await;
    let address = controlled_destination().await;
    let destinations = service(&pool, adapter(address));
    let app = router(&[(principal, "flynn")], &pool, Arc::clone(&destinations));

    let (status, body) = send(&app, connect_request("flynn", "listenbrainz", "bad-token")).await;

    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["code"], json!("invalid_credential"));
    assert_eq!(
        body["message"],
        json!("The destination rejected the credential")
    );
    assert_eq!(body["request_id"], json!("request-123"));
    assert!(!body.to_string().contains("bad-token"));
    assert_eq!(account_rows(&pool).await, 0);

    let (status, body) = send(
        &app,
        connect_request("flynn", "listenbrainz", "unauthorized-token"),
    )
    .await;

    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["code"], json!("invalid_credential"));
    assert_eq!(account_rows(&pool).await, 0);

    let (status, body) = send(&app, connect_request("flynn", "listenbrainz", "good-token")).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["username"], json!("flynnteh"));
    let stored = stored_secret(&pool, principal).await;

    let (status, body) = send(&app, connect_request("flynn", "listenbrainz", "bad-token")).await;

    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["code"], json!("already_connected"));
    assert_eq!(account_rows(&pool).await, 1);
    assert_eq!(stored_secret(&pool, principal).await, stored);
    assert_eq!(
        destinations
            .enabled_credential(principal, "listenbrainz")
            .await
            .unwrap()
            .map(|credential| credential.token),
        Some("good-token".to_owned())
    );
}

#[sqlx::test]
async fn a_transient_validator_failure_answers_503_and_stores_nothing(pool: PgPool) {
    let principal = Uuid::now_v7();
    insert_user(&pool, principal, "flynn").await;
    let validator = FakeValidator::refusing(ValidationError::Unavailable);
    let destinations = service(
        &pool,
        Arc::clone(&validator) as Arc<dyn DestinationValidator>,
    );
    let app = router(&[(principal, "flynn")], &pool, Arc::clone(&destinations));

    let (status, body) = send(&app, connect_request("flynn", "listenbrainz", TOKEN)).await;

    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(body["code"], json!("destination_unavailable"));
    assert_eq!(
        body["message"],
        json!("The destination is temporarily unavailable")
    );
    assert_eq!(account_rows(&pool).await, 0);
    assert_eq!(validator.calls().len(), 1);
}

#[sqlx::test]
async fn disabling_re_enabling_and_revoking_change_the_credential_read(pool: PgPool) {
    let principal = Uuid::now_v7();
    insert_user(&pool, principal, "flynn").await;
    let validator = FakeValidator::accepting("flynnteh");
    let destinations = service(
        &pool,
        Arc::clone(&validator) as Arc<dyn DestinationValidator>,
    );
    let app = router(&[(principal, "flynn")], &pool, Arc::clone(&destinations));

    assert_eq!(
        send(&app, connect_request("flynn", "listenbrainz", TOKEN))
            .await
            .0,
        StatusCode::OK
    );

    let (status, body) = send(
        &app,
        request(
            "POST",
            "/api/v1/identity/destinations/listenbrainz/disable",
            "flynn",
            "",
        ),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["enabled"], json!(false));
    assert_eq!(body["revoked"], json!(false));
    assert!(
        destinations
            .enabled_destinations(principal)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        destinations
            .enabled_credential(principal, "listenbrainz")
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(account_rows(&pool).await, 1);

    let (status, body) = send(
        &app,
        request(
            "POST",
            "/api/v1/identity/destinations/listenbrainz/enable",
            "flynn",
            "",
        ),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["enabled"], json!(true));
    assert_eq!(
        destinations
            .enabled_destinations(principal)
            .await
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        destinations
            .enabled_credential(principal, "listenbrainz")
            .await
            .unwrap()
            .map(|credential| credential.token),
        Some(TOKEN.to_owned())
    );

    let (status, body) = send(
        &app,
        request(
            "DELETE",
            "/api/v1/identity/destinations/listenbrainz",
            "flynn",
            "",
        ),
    )
    .await;

    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(body, Value::Null);

    let (_, statuses) = send(
        &app,
        request("GET", "/api/v1/identity/destinations", "flynn", ""),
    )
    .await;

    assert_eq!(statuses[0]["revoked"], json!(true));
    assert_eq!(statuses[0]["enabled"], json!(true));
    assert!(
        destinations
            .enabled_credential(principal, "listenbrainz")
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        destinations
            .enabled_destinations(principal)
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(account_rows(&pool).await, 1);
}

#[sqlx::test]
async fn the_credential_read_round_trips_and_a_rewritten_row_id_fails_closed(pool: PgPool) {
    let principal = Uuid::now_v7();
    insert_user(&pool, principal, "flynn").await;
    let validator = FakeValidator::accepting("flynnteh");
    let destinations = service(
        &pool,
        Arc::clone(&validator) as Arc<dyn DestinationValidator>,
    );
    let app = router(&[(principal, "flynn")], &pool, Arc::clone(&destinations));

    assert_eq!(
        send(&app, connect_request("flynn", "listenbrainz", TOKEN))
            .await
            .0,
        StatusCode::OK
    );

    let credential = destinations
        .enabled_credential(principal, "listenbrainz")
        .await
        .unwrap()
        .expect("the stored credential");
    let destination_id = credential.destination_id;

    assert_eq!(credential.token, TOKEN);
    assert_eq!(credential.username.as_deref(), Some("flynnteh"));
    assert!(!format!("{credential:?}").contains(TOKEN));

    let rewritten = sqlx::query("UPDATE destination_accounts SET id = $1 WHERE id = $2")
        .bind(Uuid::now_v7())
        .bind(destination_id)
        .execute(&pool)
        .await
        .unwrap();

    assert_eq!(rewritten.rows_affected(), 1);

    let error = destinations
        .enabled_credential(principal, "listenbrainz")
        .await
        .expect_err("a rewritten row id must not decrypt");

    assert!(
        format!("{error:?}").contains("InvalidData"),
        "the aad mismatch must surface as invalid data, got {error:?}"
    );
}

#[sqlx::test]
async fn the_other_routes_map_their_statuses_and_never_call_the_validator(pool: PgPool) {
    let principal = Uuid::now_v7();
    insert_user(&pool, principal, "flynn").await;
    let validator = FakeValidator::accepting("flynnteh");
    let destinations = service(
        &pool,
        Arc::clone(&validator) as Arc<dyn DestinationValidator>,
    );
    let app = router(&[(principal, "flynn")], &pool, Arc::clone(&destinations));

    let (status, body) = send(
        &app,
        request(
            "POST",
            "/api/v1/identity/destinations/lastfm/enable",
            "flynn",
            "",
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["code"], json!("invalid_request"));

    for path in [
        "/api/v1/identity/destinations/listenbrainz/enable",
        "/api/v1/identity/destinations/listenbrainz/disable",
    ] {
        let (status, body) = send(&app, request("POST", path, "flynn", "")).await;

        assert_eq!(status, StatusCode::NOT_FOUND, "{path}");
        assert_eq!(body["code"], json!("not_found"), "{path}");
    }

    let (status, body) = send(
        &app,
        request(
            "DELETE",
            "/api/v1/identity/destinations/listenbrainz",
            "flynn",
            "",
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["code"], json!("not_found"));

    let (status, body) = send(
        &app,
        request(
            "GET",
            "/api/v1/identity/destinations?unknown=1",
            "flynn",
            "",
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["code"], json!("invalid_request"));

    assert_eq!(validator.calls().len(), 0);

    assert_eq!(
        send(&app, connect_request("flynn", "listenbrainz", TOKEN))
            .await
            .0,
        StatusCode::OK
    );
    assert_eq!(validator.calls().len(), 1);

    let (status, body) = send(
        &app,
        request("GET", "/api/v1/identity/destinations", "flynn", ""),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body.as_array().expect("a list").len(), 1);

    for path in [
        "/api/v1/identity/destinations/listenbrainz/disable",
        "/api/v1/identity/destinations/listenbrainz/enable",
    ] {
        assert_eq!(
            send(&app, request("POST", path, "flynn", "")).await.0,
            StatusCode::OK,
            "{path}"
        );
        assert_eq!(validator.calls().len(), 1, "{path}");
    }

    assert!(
        destinations
            .enabled_credential(principal, "listenbrainz")
            .await
            .unwrap()
            .is_some()
    );
    assert_eq!(validator.calls().len(), 1);

    assert_eq!(
        send(
            &app,
            request(
                "DELETE",
                "/api/v1/identity/destinations/listenbrainz",
                "flynn",
                "",
            ),
        )
        .await
        .0,
        StatusCode::NO_CONTENT
    );
    assert_eq!(validator.calls().len(), 1);

    assert_eq!(
        send(&app, connect_request("flynn", "listenbrainz", TOKEN))
            .await
            .0,
        StatusCode::OK
    );
    assert_eq!(validator.calls().len(), 2);

    let (status, body) = send(&app, connect_request("flynn", "listenbrainz", TOKEN)).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["code"], json!("already_connected"));
    assert_eq!(validator.calls().len(), 2);
}

#[sqlx::test]
async fn an_absent_credential_key_answers_503_without_a_detail_leak(pool: PgPool) {
    let app = support::app(pool.clone(), Vec::new());
    let expected = json!({
        "code": "destination_credentials_unavailable",
        "message": "Destination credentials are not configured",
        "request_id": "request-123",
    });

    let requests = vec![
        request(
            "GET",
            "/api/v1/identity/destinations",
            support::credentials::TEST_USERNAME,
            "",
        ),
        connect_request(support::credentials::TEST_USERNAME, "listenbrainz", TOKEN),
        request(
            "POST",
            "/api/v1/identity/destinations/listenbrainz/enable",
            support::credentials::TEST_USERNAME,
            "",
        ),
        request(
            "POST",
            "/api/v1/identity/destinations/listenbrainz/disable",
            support::credentials::TEST_USERNAME,
            "",
        ),
        request(
            "DELETE",
            "/api/v1/identity/destinations/listenbrainz",
            support::credentials::TEST_USERNAME,
            "",
        ),
    ];

    for request in requests {
        let (status, body) = send(&app, request).await;

        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(body, expected);
        assert_eq!(members(&body).len(), 3);
        let rendered = body.to_string();
        assert!(!rendered.contains("storage"), "{rendered}");
        assert!(!rendered.contains("Unavailable"), "{rendered}");
        assert!(!rendered.contains(TOKEN), "{rendered}");
    }
}
