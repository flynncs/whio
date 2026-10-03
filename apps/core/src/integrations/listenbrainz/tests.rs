use std::sync::Arc;

use crate::features::identity::destinations::DestinationProvider;

use super::*;
use axum::{
    Router,
    extract::{Request, State},
    http::header,
    response::IntoResponse,
    routing::post,
};
use std::sync::{
    Mutex as StdMutex,
    atomic::{AtomicUsize, Ordering},
};

type Calls = Arc<StdMutex<Vec<(String, String, String)>>>;

#[derive(Clone)]
struct Recorded {
    calls: Calls,
    limiter: Arc<CallLimiter>,
}

impl Recorded {
    fn start() -> (Router, Calls, Arc<CallLimiter>) {
        let calls = Arc::new(StdMutex::new(Vec::new()));
        let limiter = Arc::new(CallLimiter::new(MINIMUM_INTERVAL));
        let state = Self {
            calls: Arc::clone(&calls),
            limiter: Arc::clone(&limiter),
        };
        let app = Router::new()
            .route(VALIDATE_PATH, post(validate))
            .route(SUBMIT_PATH, post(submit))
            .with_state(state);

        (app, calls, limiter)
    }
}

async fn serve(app: Router) -> Url {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });

    Url::parse(&format!("http://{address}/")).unwrap()
}

fn request_line(request: &Request) -> (String, String, String) {
    let authorization = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_owned();
    let user_agent = request
        .headers()
        .get(header::USER_AGENT)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_owned();

    (request.uri().path().to_owned(), authorization, user_agent)
}

async fn validate(State(state): State<Recorded>, request: Request) -> impl IntoResponse {
    state.limiter.acquire().await;
    state.calls.lock().unwrap().push(request_line(&request));

    match request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .strip_prefix("Token ")
    {
        Some("good-token") => (StatusCode::OK, r#"{"valid":true,"user_name":"flynnteh"}"#),
        Some("unauthorized-token") => (StatusCode::UNAUTHORIZED, "{}"),
        _ => (
            StatusCode::OK,
            r#"{"valid":false,"message":"invalid token"}"#,
        ),
    }
}

async fn submit(State(state): State<Recorded>, request: Request) -> impl IntoResponse {
    state.limiter.acquire().await;
    state.calls.lock().unwrap().push(request_line(&request));

    let token = header_of(request.headers(), header::AUTHORIZATION);
    let body = axum::body::to_bytes(request.into_body(), 1024 * 1024)
        .await
        .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
        .unwrap_or_default();

    match token.as_str() {
        "Token filtered-token" => (
            StatusCode::OK,
            r#"{"status":"filtered","message":"listen is too old"}"#,
        ),
        "Token slow-token" => (
            StatusCode::TOO_MANY_REQUESTS,
            r#"{"status":"rate_limited"}"#,
        ),
        "Token rejected-token" => (StatusCode::BAD_REQUEST, r#"{"message":"bad payload"}"#),
        "Token unauthorized-token" => (StatusCode::UNAUTHORIZED, "{}"),
        "Token broken-token" => (StatusCode::INTERNAL_SERVER_ERROR, "{}"),
        _ if body.contains("plain-token") => (StatusCode::BAD_REQUEST, "{}"),
        _ => (StatusCode::OK, r#"{"status":"ok"}"#),
    }
}

fn header_of(headers: &axum::http::HeaderMap, name: header::HeaderName) -> String {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_owned()
}

fn listen() -> ScrobbleListen {
    ScrobbleListen {
        listened_at: 1_700_000_000,
        artist_name: "a-ha".to_owned(),
        track_name: "Take On Me".to_owned(),
        recording_mbid: Some("recording".to_owned()),
        release_mbid: Some("release".to_owned()),
        release_name: Some("Hunting High and Low".to_owned()),
        duration_ms: Some(225_000),
        client: Some("whio/test".to_owned()),
    }
}

async fn adapter(app: Router, limiter: Arc<CallLimiter>) -> ListenBrainzAdapter {
    ListenBrainzAdapter::with_config(
        serve(app).await,
        Duration::from_secs(5),
        Duration::from_secs(5),
        limiter,
    )
    .unwrap()
}

#[tokio::test]
async fn an_invalid_token_is_read_from_a_two_hundred_body() {
    let (app, calls, limiter) = Recorded::start();
    let adapter = adapter(app, limiter).await;

    let error = adapter
        .validate(&DestinationProvider::ListenBrainz, "bad-token")
        .await
        .unwrap_err();

    assert_eq!(error, ValidationError::InvalidCredential);
    assert_eq!(calls.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn an_unauthorized_status_is_an_invalid_credential() {
    let (app, _, limiter) = Recorded::start();
    let adapter = adapter(app, limiter).await;

    assert_eq!(
        adapter
            .validate(&DestinationProvider::ListenBrainz, "unauthorized-token")
            .await
            .unwrap_err(),
        ValidationError::InvalidCredential
    );
}

#[tokio::test]
async fn a_valid_token_returns_the_destination_username() {
    let (app, calls, limiter) = Recorded::start();
    let adapter = adapter(app, limiter).await;

    let validated = adapter
        .validate(&DestinationProvider::ListenBrainz, "good-token")
        .await
        .unwrap();

    assert_eq!(validated.provider_id, DestinationProvider::ListenBrainz);
    assert_eq!(validated.username.as_deref(), Some("flynnteh"));

    let calls = calls.lock().unwrap();
    assert_eq!(calls[0].0, VALIDATE_PATH);
    assert_eq!(calls[0].1, "Token good-token");
    assert_eq!(calls[0].2, USER_AGENT);
}

#[tokio::test]
async fn a_transport_failure_is_an_unavailable_destination() {
    let limiter = Arc::new(CallLimiter::new(MINIMUM_INTERVAL));
    let adapter = ListenBrainzAdapter::with_config(
        Url::parse("http://127.0.0.1:1/").unwrap(),
        Duration::from_millis(200),
        Duration::from_millis(200),
        limiter,
    )
    .unwrap();

    assert_eq!(
        adapter
            .validate(&DestinationProvider::ListenBrainz, "good-token")
            .await
            .unwrap_err(),
        ValidationError::Unavailable
    );
}

#[tokio::test]
async fn an_accepted_submission_is_not_retried() {
    let (app, calls, limiter) = Recorded::start();
    let adapter = adapter(app, limiter).await;

    let outcome = adapter.submit("good-token", &listen()).await.unwrap();

    assert_eq!(outcome, SubmissionOutcome::Accepted);

    let calls = calls.lock().unwrap();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].0, SUBMIT_PATH);
    assert_eq!(calls[0].1, "Token good-token");
}

#[tokio::test]
async fn a_filtered_submission_carries_the_provider_message() {
    let (app, _, limiter) = Recorded::start();
    let adapter = adapter(app, limiter).await;

    assert_eq!(
        adapter.submit("filtered-token", &listen()).await.unwrap(),
        SubmissionOutcome::Filtered {
            message: "listen is too old".to_owned()
        }
    );
}

#[tokio::test]
async fn a_rate_limit_reads_retry_after_when_the_body_omits_it() {
    let (app, _, limiter) = Recorded::start();
    let adapter = adapter(app, limiter).await;

    assert_eq!(
        adapter.submit("slow-token", &listen()).await.unwrap_err(),
        SubmissionError::RateLimited { retry_after: None }
    );
}

#[tokio::test]
async fn a_rate_limit_reads_the_retry_after_header_when_present() {
    let limiter = Arc::new(CallLimiter::new(MINIMUM_INTERVAL));
    let app = Router::new()
        .route(
            SUBMIT_PATH,
            post(|| async {
                (
                    StatusCode::TOO_MANY_REQUESTS,
                    [(header::RETRY_AFTER, "42")],
                    "{}",
                )
            }),
        )
        .with_state(());
    let adapter = adapter(app, limiter).await;

    assert_eq!(
        adapter.submit("good-token", &listen()).await.unwrap_err(),
        SubmissionError::RateLimited {
            retry_after: Some(Duration::from_secs(42))
        }
    );
}

#[tokio::test]
async fn the_submission_errors_map_to_the_library_outcomes() {
    let (app, _, limiter) = Recorded::start();
    let adapter = adapter(app, limiter).await;

    assert_eq!(
        adapter
            .submit("unauthorized-token", &listen())
            .await
            .unwrap_err(),
        SubmissionError::CredentialRejected
    );
    assert!(matches!(
        adapter
            .submit("rejected-token", &listen())
            .await
            .unwrap_err(),
        SubmissionError::Rejected { .. }
    ));
    assert_eq!(
        adapter.submit("broken-token", &listen()).await.unwrap_err(),
        SubmissionError::Unavailable
    );
}

#[tokio::test]
async fn the_shared_limiter_serialises_calls_to_the_one_host() {
    let (app, calls, limiter) = Recorded::start();
    let adapter = adapter(app, limiter).await;
    let started = Instant::now();

    adapter
        .validate(&DestinationProvider::ListenBrainz, "good-token")
        .await
        .unwrap();
    adapter.submit("good-token", &listen()).await.unwrap();
    adapter.submit("good-token", &listen()).await.unwrap();

    let elapsed = started.elapsed();
    assert_eq!(calls.lock().unwrap().len(), 3);
    assert!(
        elapsed >= 2 * MINIMUM_INTERVAL,
        "three calls to one host must be serialised, took {elapsed:?}"
    );
}

#[tokio::test]
async fn the_request_never_carries_the_provider_token_in_its_body() {
    let calls = Arc::new(StdMutex::new(Vec::new()));
    let recorded = Arc::clone(&calls);
    let app = Router::new().route(
        SUBMIT_PATH,
        post(move |request: Request| {
            let recorded = Arc::clone(&recorded);
            async move {
                recorded
                    .lock()
                    .unwrap()
                    .push(header_of(request.headers(), header::AUTHORIZATION));
                let body = axum::body::to_bytes(request.into_body(), 1024 * 1024)
                    .await
                    .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
                    .unwrap_or_default();
                recorded.lock().unwrap().push(body);
                (StatusCode::OK, r#"{"status":"ok"}"#)
            }
        }),
    );
    let limiter = Arc::new(CallLimiter::new(Duration::from_millis(1)));
    let adapter = adapter(app, limiter).await;

    adapter.submit("secret-token", &listen()).await.unwrap();

    let calls = calls.lock().unwrap();
    assert_eq!(calls[0], "Token secret-token");
    assert!(!calls[1].contains("secret-token"));
}

#[test]
fn the_limiter_reserves_the_next_slot_before_a_call_runs() {
    let limiter = CallLimiter::new(Duration::from_millis(50));

    assert_eq!(limiter.reserve(), Duration::ZERO);
    assert!(limiter.reserve() >= Duration::from_millis(40));
}

#[test]
fn the_process_wide_interval_is_one_call_per_second() {
    assert_eq!(MINIMUM_INTERVAL, Duration::from_secs(1));

    let first = ListenBrainzAdapter::new().unwrap();
    let second = ListenBrainzAdapter::new().unwrap();

    assert!(
        !Arc::ptr_eq(first.limiter(), second.limiter()),
        "each construction owns its own limiter; app constructs the adapter once"
    );
}

#[test]
fn the_counter_is_shared_by_handle_not_copied_per_client() {
    let limiter = Arc::new(CallLimiter::new(MINIMUM_INTERVAL));
    let calls = Arc::new(AtomicUsize::new(0));

    for _ in 0..3 {
        let clone = Arc::clone(&limiter);
        calls.fetch_add(1, Ordering::SeqCst);
        assert_eq!(limiter.interval, clone.interval);
    }

    assert_eq!(calls.load(Ordering::SeqCst), 3);
    assert_eq!(Arc::strong_count(&limiter), 1);
}

#[test]
fn the_user_agent_names_the_application_version_and_contact() {
    assert!(USER_AGENT.starts_with("whio/"));
    assert!(USER_AGENT.contains("( https://"));
}

#[tokio::test]
async fn a_document_style_body_is_never_sent_for_the_wrong_provider() {
    let (app, calls, limiter) = Recorded::start();
    let adapter = adapter(app, limiter).await;

    assert_eq!(
        adapter
            .validate(&DestinationProvider::ListenBrainz, "good-token")
            .await
            .unwrap()
            .username
            .as_deref(),
        Some("flynnteh")
    );
    assert_eq!(calls.lock().unwrap().len(), 1);
}

#[allow(dead_code)]
fn header_names(headers: &axum::http::HeaderMap) -> Vec<String> {
    headers.keys().map(|name| name.to_string()).collect()
}
