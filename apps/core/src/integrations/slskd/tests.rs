use std::{
    collections::VecDeque,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};

use axum::{
    Router,
    body::Body,
    extract::State,
    http::{Request, StatusCode},
    response::{IntoResponse, Response},
    routing::any,
};
use reqwest::Url;
use uuid::Uuid;

use crate::features::acquisition::ports::{
    BackendHealth, CandidateLimit, CandidateSearch, Rejection, RemoteTransfer, SearchText, Slot,
    TransferError, TransferHandle, TransferInput, TransferPhase, TransferRequest,
};
use crate::platform::config::{ConfigError, Vars};

use crate::platform::timer::Timer;
use crate::platform::timer::manual::ManualTimer;

use super::{Slskd, config};

#[derive(Clone, Debug)]
struct Recorded {
    method: String,
    path: String,
    query: String,
    body: String,
    api_key: Option<String>,
}

struct Route {
    method: &'static str,
    path: &'static str,
    handler: Box<dyn Fn(&Recorded) -> Reply + Send + Sync>,
}

struct Stub {
    routes: Vec<Route>,
    records: Arc<Mutex<Vec<Recorded>>>,
}

#[derive(Clone)]
struct Reply {
    status: StatusCode,
    body: String,
}

fn reply(status: StatusCode, body: impl Into<String>) -> Reply {
    Reply {
        status,
        body: body.into(),
    }
}

fn ok(body: impl Into<String>) -> Reply {
    reply(StatusCode::OK, body)
}

fn created(body: impl Into<String>) -> Reply {
    reply(StatusCode::CREATED, body)
}

fn route(
    method: &'static str,
    path: &'static str,
    handler: impl Fn(&Recorded) -> Reply + Send + Sync + 'static,
) -> Route {
    Route {
        method,
        path,
        handler: Box::new(handler),
    }
}

fn sequence(replies: Vec<Reply>) -> impl Fn(&Recorded) -> Reply + Send + Sync + 'static {
    let queue = Arc::new(Mutex::new(VecDeque::from(replies)));

    move |_| {
        queue
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| reply(StatusCode::INTERNAL_SERVER_ERROR, "exhausted"))
    }
}

async fn start(routes: Vec<Route>) -> (Url, Arc<Mutex<Vec<Recorded>>>) {
    let records = Arc::new(Mutex::new(Vec::new()));
    let stub = Arc::new(Stub {
        routes,
        records: Arc::clone(&records),
    });
    let app = Router::new()
        .fallback(any(respond))
        .with_state(Arc::clone(&stub));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();

    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    (Url::parse(&format!("http://{address}/")).unwrap(), records)
}

async fn respond(State(stub): State<Arc<Stub>>, request: Request<Body>) -> Response<Body> {
    let method = request.method().to_string();
    let uri = request.uri().clone();
    let api_key = request
        .headers()
        .get("x-api-key")
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    let body = axum::body::to_bytes(request.into_body(), usize::MAX)
        .await
        .unwrap_or_default();
    let recorded = Recorded {
        method,
        path: uri.path().to_owned(),
        query: uri.query().unwrap_or_default().to_owned(),
        body: String::from_utf8_lossy(&body).to_string(),
        api_key,
    };
    stub.records.lock().unwrap().push(recorded.clone());

    let matched = stub
        .routes
        .iter()
        .filter(|candidate| {
            candidate.method == recorded.method && recorded.path.contains(candidate.path)
        })
        .max_by_key(|candidate| candidate.path.len());

    if let Some(candidate) = matched {
        let reply = (candidate.handler)(&recorded);

        return (reply.status, reply.body).into_response();
    }

    (StatusCode::NOT_FOUND, "no route").into_response()
}

fn client(base: &Url) -> Slskd {
    Slskd::with_config(
        base.clone(),
        "test-key".to_owned(),
        Duration::from_secs(15),
        Duration::from_millis(50),
        Duration::from_millis(50),
        ManualTimer::new(),
    )
    .unwrap()
}

fn fast_client(base: &Url) -> (Slskd, Arc<ManualTimer>) {
    let timer = ManualTimer::new();
    let client = Slskd::with_config(
        base.clone(),
        "test-key".to_owned(),
        Duration::from_millis(20),
        Duration::from_millis(2),
        Duration::from_millis(20),
        Arc::clone(&timer) as Arc<dyn Timer>,
    )
    .unwrap();

    (client, timer)
}

fn query(text: &str) -> SearchText {
    SearchText::new(text).unwrap()
}

fn limit(value: u16) -> CandidateLimit {
    CandidateLimit::new(value).unwrap()
}

fn file_request(key: Uuid, peer: &str, path: &str, size: u64) -> TransferRequest {
    TransferRequest {
        key,
        input: TransferInput::SoulseekFile {
            peer: peer.to_owned(),
            path: path.to_owned(),
            size_bytes: size,
        },
    }
}

const RESPONSES: &str = r#"[
  {
    "username": "peer-a",
    "hasFreeUploadSlot": true,
    "queueLength": 2,
    "uploadSpeed": 1000,
    "files": [
      {"filename": "Music\\Album\\One.flac", "size": 10, "extension": "flac", "bitRate": 900, "sampleRate": 44100, "bitDepth": 16, "length": 200, "isLocked": false},
      {"filename": "Music\\Album\\Two.flac", "size": 11, "extension": "flac", "isLocked": false}
    ],
    "lockedFiles": [
      {"filename": "Music\\Album\\Secret.flac", "size": 12, "extension": "flac", "isLocked": true}
    ]
  },
  {
    "username": "peer-b",
    "files": [
      {"filename": "Other\\Three.flac", "size": 13, "extension": "flac", "isLocked": false}
    ]
  }
]"#;

#[test]
fn slskd_is_disabled_by_default() {
    assert!(config::from_vars(&Vars::new()).unwrap().is_none());
    let disabled = Vars::from([("WHIO_SLSKD_ENABLED".to_owned(), "false".to_owned())]);
    assert!(config::from_vars(&disabled).unwrap().is_none());
    let invalid = Vars::from([("WHIO_SLSKD_ENABLED".to_owned(), "maybe".to_owned())]);
    assert!(matches!(
        config::from_vars(&invalid),
        Err(ConfigError::Invalid { .. })
    ));
}

#[test]
fn an_enabled_slskd_requires_url_and_key() {
    let missing_url = Vars::from([("WHIO_SLSKD_ENABLED".to_owned(), "true".to_owned())]);
    assert!(matches!(
        config::from_vars(&missing_url),
        Err(ConfigError::Missing { .. })
    ));

    let missing_key = Vars::from([
        ("WHIO_SLSKD_ENABLED".to_owned(), "true".to_owned()),
        ("WHIO_SLSKD_URL".to_owned(), "http://slskd.test".to_owned()),
    ]);
    assert!(matches!(
        config::from_vars(&missing_key),
        Err(ConfigError::Missing { .. })
    ));

    let enabled = Vars::from([
        ("WHIO_SLSKD_ENABLED".to_owned(), "true".to_owned()),
        ("WHIO_SLSKD_URL".to_owned(), "http://slskd.test".to_owned()),
        ("WHIO_SLSKD_API_KEY".to_owned(), "secret".to_owned()),
    ]);
    let config = config::from_vars(&enabled).unwrap().unwrap();
    assert_eq!(config.url.as_str(), "http://slskd.test/");
    assert_eq!(config.api_key, "secret");
}

#[test]
fn the_search_timeout_is_bounded() {
    let base = |seconds: &str| {
        Vars::from([
            ("WHIO_SLSKD_ENABLED".to_owned(), "true".to_owned()),
            ("WHIO_SLSKD_URL".to_owned(), "http://slskd.test".to_owned()),
            ("WHIO_SLSKD_API_KEY".to_owned(), "secret".to_owned()),
            (
                "WHIO_SLSKD_SEARCH_TIMEOUT_SECONDS".to_owned(),
                seconds.to_owned(),
            ),
        ])
    };

    assert_eq!(
        config::from_vars(&base("1"))
            .unwrap()
            .unwrap()
            .search_timeout,
        Duration::from_secs(1)
    );
    assert_eq!(
        config::from_vars(&base("120"))
            .unwrap()
            .unwrap()
            .search_timeout,
        Duration::from_secs(120)
    );
    assert!(config::from_vars(&base("0")).is_err());
    assert!(config::from_vars(&base("121")).is_err());
    assert!(config::from_vars(&base("nope")).is_err());
}

#[tokio::test]
async fn the_search_timeout_is_sent_in_milliseconds() {
    let (base, records) = start(vec![
        route("POST", "/api/v0/searches", |_| created("{}")),
        route("GET", "/searches/", |_| ok(r#"{"isComplete":true}"#)),
        route("GET", "/responses", |_| ok("[]")),
        route("DELETE", "/searches/", |_| {
            reply(StatusCode::NO_CONTENT, "")
        }),
    ])
    .await;

    let _ = client(&base)
        .search(&query("a b"), limit(10))
        .await
        .unwrap();
    let records = records.lock().unwrap();
    let post = records
        .iter()
        .find(|record| record.method == "POST")
        .unwrap();

    assert!(post.body.contains(r#""searchTimeout":15000"#));
    assert!(post.body.contains(r#""fileLimit":10"#));
    assert_eq!(
        post.api_key.as_deref(),
        Some("test-key"),
        "the API key goes in the X-API-Key header"
    );
}

#[tokio::test]
async fn files_group_by_peer_and_directory() {
    let (base, _) = start(vec![
        route("POST", "/api/v0/searches", |_| created("{}")),
        route("GET", "/searches/", |_| ok(r#"{"isComplete":true}"#)),
        route("GET", "/responses", |_| ok(RESPONSES)),
        route("DELETE", "/searches/", |_| {
            reply(StatusCode::NO_CONTENT, "")
        }),
    ])
    .await;

    let candidates = client(&base)
        .search(&query("a b"), limit(500))
        .await
        .unwrap();

    assert_eq!(candidates.len(), 2);
    assert_eq!(candidates[0].source, r"peer-a:Music\Album");
    assert_eq!(candidates[0].user(), Some("peer-a"));
    assert_eq!(candidates[1].user(), Some("peer-b"));
    assert_eq!(candidates[0].files.len(), 2);
    assert_eq!(candidates[0].files[0].name, "One.flac");
    assert_eq!(candidates[0].peer.queue_length, Some(2));
    assert_eq!(candidates[0].peer.upload_speed, Some(1000));
    assert_eq!(candidates[0].peer.has_free_slot, Some(true));
    assert_eq!(candidates[1].source, r"peer-b:Other");
}

#[tokio::test]
async fn locked_files_are_not_candidates() {
    let (base, _) = start(vec![
        route("POST", "/api/v0/searches", |_| created("{}")),
        route("GET", "/searches/", |_| ok(r#"{"isComplete":true}"#)),
        route("GET", "/responses", |_| ok(RESPONSES)),
        route("DELETE", "/searches/", |_| {
            reply(StatusCode::NO_CONTENT, "")
        }),
    ])
    .await;

    let candidates = client(&base)
        .search(&query("a b"), limit(500))
        .await
        .unwrap();
    let names = candidates
        .iter()
        .flat_map(|candidate| candidate.files.iter())
        .map(|file| file.name.as_str())
        .collect::<Vec<_>>();

    assert_eq!(names, vec!["One.flac", "Two.flac", "Three.flac"]);
    assert!(!names.contains(&"Secret.flac"));
}

#[tokio::test]
async fn a_search_past_its_deadline_returns_what_arrived() {
    let (base, _) = start(vec![
        route("POST", "/api/v0/searches", |_| created("{}")),
        route("GET", "/searches/", |_| ok(r#"{"isComplete":false}"#)),
        route("GET", "/responses", |_| ok(RESPONSES)),
        route("DELETE", "/searches/", |_| {
            reply(StatusCode::NO_CONTENT, "")
        }),
    ])
    .await;
    let (client, timer) = fast_client(&base);
    let search = tokio::spawn(async move { client.search(&query("a b"), limit(500)).await });

    timer.await_waiter().await;
    timer.advance(Duration::from_millis(100));

    let candidates = search.await.unwrap().unwrap();
    assert_eq!(candidates.len(), 2);
}

#[tokio::test]
async fn delivered_is_set_only_when_reported_complete() {
    let id = Uuid::now_v7();
    let (base, _) = start(vec![
        route("GET", "/batches/", move |_| {
            ok(r#"{"username":"peer-a","transfers":[{"id":"11111111-1111-1111-1111-111111111111","filename":"Music\\Album\\Track.flac","size":10,"state":"Completed, Succeeded","bytesTransferred":10}]}"#)
        }),
        route("GET", "/api/v0/server", |_| ok(r#"{"isLoggedIn":true}"#)),
    ])
    .await;
    let adapter = client(&base);

    let complete = adapter
        .status(&[TransferHandle::new(format!(
            "{id}\u{1f}peer-a\u{1f}Music\\Album\\Track.flac"
        ))])
        .await
        .unwrap();
    match &complete[0] {
        Slot::Accepted(progress) => {
            assert_eq!(progress.phase, TransferPhase::ReportedComplete);
            assert_eq!(progress.delivered, Some(PathBuf::from("Album/Track.flac")));
            assert_eq!(progress.bytes_done, Some(10));
        }
        other => panic!("unexpected slot: {other:?}"),
    }

    let (base, _) = start(vec![route("GET", "/batches/", |_| {
        ok(r#"{"username":"peer-a","transfers":[{"id":"11111111-1111-1111-1111-111111111111","filename":"Music\\Album\\Track.flac","size":10,"state":"InProgress","bytesTransferred":4}]}"#)
    })])
    .await;
    let running = client(&base)
        .status(&[TransferHandle::new(format!(
            "{id}\u{1f}peer-a\u{1f}Music\\Album\\Track.flac"
        ))])
        .await
        .unwrap();
    match &running[0] {
        Slot::Accepted(progress) => {
            assert_eq!(progress.phase, TransferPhase::Active);
            assert_eq!(progress.delivered, None);
        }
        other => panic!("unexpected slot: {other:?}"),
    }
}

#[tokio::test]
async fn one_batch_per_request_keyed_by_the_request_key() {
    let key = Uuid::now_v7();
    let (base, records) = start(vec![route("POST", "/batches", |_| created("{}"))]).await;

    let slots = client(&base)
        .start(&[file_request(key, "peer-a", r"Music\Album\One.flac", 10)])
        .await
        .unwrap();

    assert!(matches!(slots.as_slice(), [Slot::Accepted(_)]));
    let records = records.lock().unwrap();
    let post = records.first().unwrap();
    assert!(post.body.contains(&key.to_string()));
    assert!(post.body.contains(r#""username":"peer-a""#));
    assert!(post.body.contains(r#""filename":"Music\\Album\\One.flac""#));
    assert!(post.body.contains(r#""size":10"#));
}

#[tokio::test]
async fn a_conflicting_batch_for_the_same_file_is_accepted() {
    let key = Uuid::now_v7();
    let (base, _) = start(vec![
        route("POST", "/batches", |_| {
            reply(StatusCode::CONFLICT, "a batch with that id exists")
        }),
        route("GET", "/batches/", |_| {
            ok(r#"{"username":"peer-a","transfers":[{"id":"11111111-1111-1111-1111-111111111111","filename":"Music\\Album\\One.flac","size":10,"state":"Queued"}]}"#)
        }),
    ])
    .await;

    let slots = client(&base)
        .start(&[file_request(key, "peer-a", r"Music\Album\One.flac", 10)])
        .await
        .unwrap();

    assert!(matches!(slots.as_slice(), [Slot::Accepted(_)]));
}

#[tokio::test]
async fn a_conflicting_batch_for_another_file_is_malformed() {
    let key = Uuid::now_v7();
    let (base, _) = start(vec![
        route("POST", "/batches", |_| {
            reply(StatusCode::CONFLICT, "a batch with that id exists")
        }),
        route("GET", "/batches/", |_| {
            ok(r#"{"username":"peer-a","transfers":[{"id":"11111111-1111-1111-1111-111111111111","filename":"Music\\Album\\Other.flac","size":10,"state":"Queued"}]}"#)
        }),
    ])
    .await;

    assert_eq!(
        client(&base)
            .start(&[file_request(key, "peer-a", r"Music\Album\One.flac", 10)])
            .await,
        Err(TransferError::Malformed)
    );
}

#[tokio::test]
async fn a_partial_failure_rejects_only_that_slot() {
    let first = file_request(Uuid::now_v7(), "peer-a", r"Music\Album\One.flac", 10);
    let second = file_request(Uuid::now_v7(), "peer-b", r"Other\Two.flac", 11);
    let failing = "One.flac".to_owned();
    let (base, _) = start(vec![route("POST", "/batches", move |record| {
        if record.body.contains(&failing) {
            reply(
                StatusCode::MULTI_STATUS,
                r#"{"batch":null,"failures":[{"filename":"Music\\Album\\One.flac","message":"already in progress"}]}"#,
            )
        } else {
            created("{}")
        }
    })])
    .await;

    let slots = client(&base).start(&[first, second]).await.unwrap();

    assert_eq!(slots.len(), 2);
    assert!(matches!(slots[0], Slot::Rejected(Rejection::Refused)));
    assert!(matches!(slots[1], Slot::Accepted(_)));
}

#[tokio::test]
async fn an_outage_mid_call_fails_the_call() {
    let first = file_request(Uuid::now_v7(), "peer-a", r"Music\Album\One.flac", 10);
    let second = file_request(Uuid::now_v7(), "peer-b", r"Other\Two.flac", 11);
    let (base, _) = start(vec![route(
        "POST",
        "/batches",
        sequence(vec![
            created("{}"),
            reply(StatusCode::SERVICE_UNAVAILABLE, ""),
        ]),
    )])
    .await;

    assert_eq!(
        client(&base).start(&[first, second]).await,
        Err(TransferError::Unavailable)
    );
}

#[tokio::test]
async fn every_phase_rule_maps_and_a_missing_batch_is_missing() {
    let id = Uuid::now_v7();
    let cases = [
        ("Completed, Succeeded", TransferPhase::ReportedComplete),
        ("Completed, Cancelled", TransferPhase::Cancelled),
        ("Completed, TimedOut", TransferPhase::Failed),
        ("Errored", TransferPhase::Failed),
        ("Rejected", TransferPhase::Failed),
        ("Aborted", TransferPhase::Failed),
        ("InProgress", TransferPhase::Active),
        ("Initializing", TransferPhase::Queued),
        ("Requested", TransferPhase::Queued),
        ("Queued, Remotely", TransferPhase::Queued),
        ("None", TransferPhase::Queued),
    ];

    for (state, expected) in cases {
        let body = format!(
            r#"{{"username":"peer-a","transfers":[{{"id":"11111111-1111-1111-1111-111111111111","filename":"Music\\Album\\Track.flac","size":10,"state":"{state}","bytesTransferred":3}}]}}"#
        );
        let (base, _) = start(vec![route("GET", "/batches/", move |_| ok(body.clone()))]).await;
        let slots = client(&base)
            .status(&[TransferHandle::new(format!(
                "{id}\u{1f}peer-a\u{1f}Music\\Album\\Track.flac"
            ))])
            .await
            .unwrap();

        match &slots[0] {
            Slot::Accepted(progress) => assert_eq!(progress.phase, expected, "state {state}"),
            other => panic!("unexpected slot for {state}: {other:?}"),
        }
    }

    let (base, _) = start(vec![route("GET", "/batches/", |_| {
        reply(StatusCode::NOT_FOUND, "")
    })])
    .await;
    assert!(matches!(
        client(&base)
            .status(&[TransferHandle::new(format!(
                "{id}\u{1f}peer-a\u{1f}Music\\Album\\Track.flac"
            ))])
            .await
            .unwrap()
            .as_slice(),
        [Slot::Missing]
    ));
}

#[tokio::test]
async fn cancel_accepts_204_and_404() {
    let id = Uuid::now_v7();
    let batch = r#"{"username":"peer-a","transfers":[{"id":"11111111-1111-1111-1111-111111111111","filename":"Music\\Album\\Track.flac","size":10,"state":"Queued"}]}"#;
    let (base, records) = start(vec![
        route("GET", "/batches/", move |_| ok(batch.to_owned())),
        route("DELETE", "/downloads/", |_| {
            reply(StatusCode::NO_CONTENT, "")
        }),
    ])
    .await;

    let handle = TransferHandle::new(format!("{id}\u{1f}peer-a\u{1f}Music\\Album\\Track.flac"));
    client(&base).cancel(&handle, true).await.unwrap();

    let (path, query) = {
        let records = records.lock().unwrap();
        let delete = records
            .iter()
            .find(|record| record.method == "DELETE")
            .unwrap();

        (delete.path.clone(), delete.query.clone())
    };
    assert!(
        path.contains("/api/v0/transfers/downloads/peer-a/11111111-1111-1111-1111-111111111111")
    );
    assert_eq!(query, "remove=true");

    let (base, _) = start(vec![
        route("GET", "/batches/", move |_| ok(batch.to_owned())),
        route("DELETE", "/downloads/", |_| {
            reply(StatusCode::NOT_FOUND, "")
        }),
    ])
    .await;
    client(&base).cancel(&handle, false).await.unwrap();
}

#[tokio::test]
async fn health_reports_each_variant() {
    let cases = [
        (ok(r#"{"isLoggedIn":true}"#), BackendHealth::Ready),
        (ok(r#"{"isLoggedIn":false}"#), BackendHealth::NotLoggedIn),
        (
            reply(StatusCode::UNAUTHORIZED, ""),
            BackendHealth::CredentialsRejected,
        ),
        (
            reply(StatusCode::SERVICE_UNAVAILABLE, ""),
            BackendHealth::Unavailable,
        ),
    ];

    for (reply, expected) in cases {
        let (base, _) = start(vec![route("GET", "/api/v0/server", move |_| reply.clone())]).await;

        assert_eq!(client(&base).health().await, expected);
    }
}

#[tokio::test]
async fn credentials_rejected_on_401_and_403() {
    for status in [StatusCode::UNAUTHORIZED, StatusCode::FORBIDDEN] {
        let (base, _) = start(vec![
            route("POST", "/api/v0/searches", move |_| reply(status, "")),
            route("GET", "/api/v0/server", move |_| reply(status, "")),
        ])
        .await;
        let client = client(&base);

        assert_eq!(
            client.search(&query("a b"), limit(10)).await,
            Err(TransferError::CredentialsRejected)
        );
        assert_eq!(client.health().await, BackendHealth::CredentialsRejected);
    }
}

#[tokio::test]
async fn the_api_key_never_appears_in_an_error() {
    let (base, records) = start(vec![route("POST", "/api/v0/searches", |_| {
        reply(StatusCode::INTERNAL_SERVER_ERROR, "upstream failure")
    })])
    .await;
    let secret = "super-secret-key";
    let client = Slskd::with_config(
        base,
        secret.to_owned(),
        Duration::from_secs(15),
        Duration::from_millis(50),
        Duration::from_millis(50),
        ManualTimer::new(),
    )
    .unwrap();

    let error = client.search(&query("a b"), limit(10)).await.unwrap_err();

    assert!(!format!("{error}").contains(secret));
    assert!(!format!("{error:?}").contains(secret));
    assert_eq!(
        records.lock().unwrap()[0].api_key.as_deref(),
        Some(secret),
        "the key is only ever sent in the header"
    );
}

#[tokio::test]
async fn candidates_are_truncated_to_the_limit() {
    let (base, _) = start(vec![
        route("POST", "/api/v0/searches", |_| created("{}")),
        route("GET", "/searches/", |_| ok(r#"{"isComplete":true}"#)),
        route("GET", "/responses", |_| ok(RESPONSES)),
        route("DELETE", "/searches/", |_| {
            reply(StatusCode::NO_CONTENT, "")
        }),
    ])
    .await;

    let candidates = client(&base).search(&query("a b"), limit(1)).await.unwrap();

    assert_eq!(candidates.len(), 1, "the grouped list is cut to the limit");
}

#[tokio::test]
async fn an_incomplete_search_polls_until_complete() {
    let calls = Arc::new(Mutex::new(0u32));
    let counter = Arc::clone(&calls);
    let (base, _) = start(vec![
        route("POST", "/api/v0/searches", |_| created("{}")),
        route("GET", "/searches/", move |_| {
            let mut count = counter.lock().unwrap();
            *count += 1;

            if *count < 2 {
                ok(r#"{"isComplete":false}"#)
            } else {
                ok(r#"{"isComplete":true}"#)
            }
        }),
        route("GET", "/responses", |_| ok(RESPONSES)),
        route("DELETE", "/searches/", |_| {
            reply(StatusCode::NO_CONTENT, "")
        }),
    ])
    .await;

    let (client, timer) = fast_client(&base);
    let search = tokio::spawn(async move { client.search(&query("a b"), limit(500)).await });

    timer.await_waiter().await;
    timer.advance(Duration::from_millis(2));

    let candidates = search.await.unwrap().unwrap();
    assert_eq!(candidates.len(), 2);
    assert!(*calls.lock().unwrap() >= 2, "the search is polled");
}

#[tokio::test]
async fn a_multi_file_batch_is_malformed() {
    let id = Uuid::now_v7();
    let (base, _) = start(vec![route("GET", "/batches/", |_| {
        ok(r#"{"username":"peer-a","transfers":[{"id":"11111111-1111-1111-1111-111111111111","filename":"Music\\Album\\Track.flac","size":10,"state":"Queued"},{"id":"22222222-2222-2222-2222-222222222222","filename":"Music\\Album\\Other.flac","size":11,"state":"Queued"}]}"#)
    })])
    .await;
    let handle = TransferHandle::new(format!("{id}\u{1f}peer-a\u{1f}Music\\Album\\Track.flac"));

    assert_eq!(
        client(&base).status(&[handle]).await,
        Err(TransferError::Malformed)
    );
}

#[tokio::test]
async fn a_wrong_peer_batch_is_malformed() {
    let id = Uuid::now_v7();
    let (base, _) = start(vec![route("GET", "/batches/", |_| {
        ok(r#"{"username":"peer-b","transfers":[{"id":"11111111-1111-1111-1111-111111111111","filename":"Music\\Album\\Track.flac","size":10,"state":"Queued"}]}"#)
    })])
    .await;
    let handle = TransferHandle::new(format!("{id}\u{1f}peer-a\u{1f}Music\\Album\\Track.flac"));

    assert_eq!(
        client(&base).status(&[handle]).await,
        Err(TransferError::Malformed)
    );
}

#[tokio::test]
async fn a_batch_with_a_matching_first_entry_is_still_malformed() {
    let id = Uuid::now_v7();
    let (base, _) = start(vec![route("GET", "/batches/", |_| {
        ok(r#"{"username":"peer-a","transfers":[{"id":"11111111-1111-1111-1111-111111111111","filename":"Music\\Album\\Track.flac","size":10,"state":"Queued"},{"id":"22222222-2222-2222-2222-222222222222","filename":"Music\\Album\\Other.flac","size":11,"state":"Queued"}]}"#)
    })])
    .await;
    let handle = TransferHandle::new(format!("{id}\u{1f}peer-a\u{1f}Music\\Album\\Track.flac"));

    assert_eq!(
        client(&base).status(&[handle]).await,
        Err(TransferError::Malformed),
        "the adapter never picks the first entry of a multi-file batch"
    );
}

#[tokio::test]
async fn a_failed_search_delete_preserves_the_result() {
    let (base, _) = start(vec![
        route("POST", "/api/v0/searches", |_| created("{}")),
        route("GET", "/searches/", |_| ok(r#"{"isComplete":true}"#)),
        route("GET", "/responses", |_| ok(RESPONSES)),
        route("DELETE", "/searches/", |_| {
            reply(StatusCode::BAD_REQUEST, "cannot delete")
        }),
    ])
    .await;

    let candidates = client(&base)
        .search(&query("a b"), limit(500))
        .await
        .unwrap();

    assert_eq!(
        candidates.len(),
        2,
        "a delete failure keeps the search result"
    );
}
