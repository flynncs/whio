use crate::support;

use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode},
    response::Response,
};
use serde_json::{Value, json};
use sqlx::PgPool;
use std::sync::Arc;
use tower::ServiceExt;
use whio_core::features::music_library::{
    availability::ProviderRegistry,
    catalogue::{LibraryService, SearchService},
    domain::{ProviderId, SourceIdentity, SourceScope, TrackId, TrackMetadata},
    queue::{MAX_RESTORE_QUEUE_ENTRIES, RestoreQueueService},
    test_support::RestoreQueueStore,
};

use support::{StorePorts, credentials::database_credential_service, router_with_queue};

struct Fixture {
    app: Router,
    ports: StorePorts,
    auth: String,
}

fn local_source(external_id: &str) -> SourceIdentity {
    SourceIdentity::new(
        ProviderId::new("local".to_owned()).unwrap(),
        SourceScope::Named("/music".to_owned()),
        external_id.to_owned(),
    )
    .unwrap()
}

fn provider_source(external_id: &str) -> SourceIdentity {
    SourceIdentity::new(
        ProviderId::new("youtube_music".to_owned()).unwrap(),
        SourceScope::Global,
        external_id.to_owned(),
    )
    .unwrap()
}

fn metadata(title: &str) -> TrackMetadata {
    TrackMetadata::from_names(title.to_owned(), vec!["Artist".to_owned()], Some(1_000)).unwrap()
}

async fn fixture(pool: PgPool) -> Fixture {
    let (credential, secret) = database_credential_service(pool.clone()).await;
    let ports = StorePorts::new(pool);
    let library = Arc::new(
        LibraryService::new(ports.library(), ports.sources(), ports.state())
            .with_registry(ProviderRegistry::with_enabled(Vec::new())),
    );
    let search = Arc::new(SearchService::new(
        ports.library(),
        ports.catalogue(),
        ports.sources(),
        ports.state(),
    ));
    let queue = Arc::new(RestoreQueueService::new(
        Arc::clone(&ports.store) as Arc<dyn RestoreQueueStore>
    ));
    let playback = ports.streaming(Vec::new());
    let app = router_with_queue(credential, search, library, queue, playback);

    Fixture {
        app,
        ports,
        auth: format!("?u=flynn&v=1.16.1&c=integration-test&p={secret}"),
    }
}

fn with_auth(uri: &str, auth: &str) -> String {
    let separator = if uri.contains('?') { '&' } else { '?' };

    format!("{uri}{separator}{}", auth.trim_start_matches('?'))
}

async fn track(ports: &StorePorts, external_id: &str, title: &str) -> TrackId {
    ports
        .catalogue()
        .get_or_create(local_source(external_id), metadata(title))
        .await
        .unwrap()
        .id()
        .clone()
}

async fn provider_track(ports: &StorePorts, external_id: &str, title: &str) -> TrackId {
    ports
        .catalogue()
        .get_or_create(provider_source(external_id), metadata(title))
        .await
        .unwrap()
        .id()
        .clone()
}

async fn send(app: &Router, method: &str, uri: &str, body: Option<&Value>) -> Response {
    let builder = Request::builder()
        .method(method)
        .uri(uri)
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

fn entry_ids(body: &Value) -> Vec<String> {
    body["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["entry_id"].as_str().unwrap().to_owned())
        .collect()
}

fn positions(body: &Value) -> Vec<u64> {
    body["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["position"].as_u64().unwrap())
        .collect()
}

fn track_ids(body: &Value) -> Vec<String> {
    body["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["track"]["id"].as_str().unwrap().to_owned())
        .collect()
}

async fn save(app: &Router, auth: &str, body: &Value) -> Response {
    send(app, "PUT", &with_auth("/api/v1/queue", auth), Some(body)).await
}

async fn read(app: &Router, auth: &str, query: &str) -> Response {
    let uri = if query.is_empty() {
        "/api/v1/queue".to_owned()
    } else {
        format!("/api/v1/queue?{query}")
    };

    send(app, "GET", &with_auth(&uri, auth), None).await
}

#[sqlx::test]
async fn a_saved_queue_round_trips(pool: PgPool) {
    let fixture = fixture(pool).await;
    let amber = track(&fixture.ports, "path-amber", "amber").await;
    let bravo = track(&fixture.ports, "path-bravo", "bravo").await;
    let saved = save(
        &fixture.app,
        &fixture.auth,
        &json!({
            "entries": [amber.to_string(), bravo.to_string(), amber.to_string()],
            "current_index": 2,
            "position_ms": 4200,
        }),
    )
    .await;

    assert_eq!(saved.status(), StatusCode::OK);
    let saved_body = json_body(saved).await;
    assert_eq!(saved_body["total"], json!(3));
    assert_eq!(saved_body["revision"], json!(1));
    assert_eq!(saved_body["current_index"], json!(2));
    assert_eq!(saved_body["position_ms"], json!(4200));
    assert_eq!(
        track_ids(&saved_body),
        vec![amber.to_string(), bravo.to_string(), amber.to_string()]
    );
    assert_eq!(positions(&saved_body), vec![0, 1, 2]);
    let saved_entry_ids = entry_ids(&saved_body);
    assert_eq!(saved_entry_ids.len(), 3);
    assert_eq!(
        saved_entry_ids
            .iter()
            .collect::<std::collections::HashSet<_>>()
            .len(),
        3,
        "each occurrence keeps its own identity"
    );
    assert_eq!(saved_body["items"][0]["track"]["title"], json!("amber"));
    assert_eq!(
        saved_body["items"][0]["track"]["availability"],
        json!("owned")
    );

    let read_back = read(&fixture.app, &fixture.auth, "").await;
    assert_eq!(read_back.status(), StatusCode::OK);
    let read_body = json_body(read_back).await;
    assert_eq!(
        read_body, saved_body,
        "the read returns the snapshot the save reported, ids and revision included"
    );
}

#[sqlx::test]
async fn an_absent_queue_is_not_an_empty_one(pool: PgPool) {
    let fixture = fixture(pool).await;
    let absent = read(&fixture.app, &fixture.auth, "").await;

    assert_eq!(absent.status(), StatusCode::NOT_FOUND);
    let absent_body = json_body(absent).await;
    assert_eq!(absent_body["code"], json!("not_found"));

    let saved = save(
        &fixture.app,
        &fixture.auth,
        &json!({"entries": [], "position_ms": 0}),
    )
    .await;

    assert_eq!(saved.status(), StatusCode::OK);
    let saved_body = json_body(saved).await;
    assert_eq!(saved_body["items"], json!([]));
    assert_eq!(saved_body["total"], json!(0));
    assert_eq!(saved_body["position_ms"], json!(0));
    assert!(
        saved_body.get("current_index").is_none(),
        "an empty queue names no index"
    );

    let empty = read(&fixture.app, &fixture.auth, "").await;
    assert_eq!(empty.status(), StatusCode::OK);
    let empty_body = json_body(empty).await;
    assert_eq!(empty_body["items"], json!([]));
    assert_eq!(empty_body["total"], json!(0));
    assert_eq!(empty_body["revision"], json!(1));

    let cleared = send(
        &fixture.app,
        "DELETE",
        &with_auth("/api/v1/queue", &fixture.auth),
        None,
    )
    .await;
    assert_eq!(cleared.status(), StatusCode::NO_CONTENT);

    let absent_again = read(&fixture.app, &fixture.auth, "").await;
    assert_eq!(absent_again.status(), StatusCode::NOT_FOUND);
    assert_eq!(json_body(absent_again).await["code"], json!("not_found"));
}

#[sqlx::test]
async fn a_save_replaces_rather_than_merges(pool: PgPool) {
    let fixture = fixture(pool).await;
    let amber = track(&fixture.ports, "path-amber", "amber").await;
    let bravo = track(&fixture.ports, "path-bravo", "bravo").await;
    let first = save(
        &fixture.app,
        &fixture.auth,
        &json!({
            "entries": [amber.to_string(), bravo.to_string(), amber.to_string()],
            "current_index": 2,
            "position_ms": 100,
        }),
    )
    .await;

    assert_eq!(first.status(), StatusCode::OK);
    let first_body = json_body(first).await;
    assert_eq!(first_body["total"], json!(3));

    let second = save(
        &fixture.app,
        &fixture.auth,
        &json!({
            "entries": [bravo.to_string()],
            "current_index": 0,
            "position_ms": 50,
        }),
    )
    .await;

    assert_eq!(second.status(), StatusCode::OK);
    let second_body = json_body(second).await;
    assert_eq!(second_body["total"], json!(1));
    assert_eq!(positions(&second_body), vec![0]);
    assert_eq!(track_ids(&second_body), vec![bravo.to_string()]);
    assert_eq!(second_body["position_ms"], json!(50));
    assert_eq!(second_body["revision"], json!(2));

    let read_back = read(&fixture.app, &fixture.auth, "").await;
    let read_body = json_body(read_back).await;
    assert_eq!(
        read_body, second_body,
        "nothing from the first snapshot survives the replacement"
    );
    let replaced = entry_ids(&read_body);
    assert!(
        replaced
            .iter()
            .all(|entry_id| !entry_ids(&first_body).contains(entry_id)),
        "the replacement carries new occurrences"
    );
}

#[sqlx::test]
async fn an_over_bound_save_is_rejected_and_changes_nothing(pool: PgPool) {
    let fixture = fixture(pool).await;
    let amber = track(&fixture.ports, "path-amber", "amber").await;
    let kept = save(
        &fixture.app,
        &fixture.auth,
        &json!({
            "entries": [amber.to_string()],
            "current_index": 0,
            "position_ms": 7,
        }),
    )
    .await;

    assert_eq!(kept.status(), StatusCode::OK);
    let kept_body = json_body(kept).await;

    let over = vec![amber.to_string(); MAX_RESTORE_QUEUE_ENTRIES + 1];
    let rejected = save(
        &fixture.app,
        &fixture.auth,
        &json!({
            "entries": over,
            "current_index": 0,
            "position_ms": 0,
        }),
    )
    .await;

    assert_eq!(rejected.status(), StatusCode::BAD_REQUEST);
    let rejected_body = json_body(rejected).await;
    assert_eq!(rejected_body["code"], json!("invalid_request"));
    assert_eq!(
        rejected_body["message"],
        json!("The restore queue holds at most 1000 entries"),
        "the client error names the bound it must respect, not an internal identifier"
    );

    let read_back = read(&fixture.app, &fixture.auth, "").await;
    assert_eq!(read_back.status(), StatusCode::OK);
    let read_body = json_body(read_back).await;
    assert_eq!(
        read_body, kept_body,
        "the rejected save leaves the earlier snapshot untouched"
    );
}

#[sqlx::test]
async fn a_stale_revision_conflicts(pool: PgPool) {
    let fixture = fixture(pool).await;
    let amber = track(&fixture.ports, "path-amber", "amber").await;
    let bravo = track(&fixture.ports, "path-bravo", "bravo").await;
    let first = save(
        &fixture.app,
        &fixture.auth,
        &json!({
            "entries": [amber.to_string()],
            "current_index": 0,
            "position_ms": 10,
        }),
    )
    .await;

    assert_eq!(first.status(), StatusCode::OK);
    let current = json_body(first).await;
    assert_eq!(current["revision"], json!(1));

    let stale = save(
        &fixture.app,
        &fixture.auth,
        &json!({
            "entries": [bravo.to_string()],
            "current_index": 0,
            "position_ms": 20,
            "revision": 999,
        }),
    )
    .await;

    assert_eq!(stale.status(), StatusCode::CONFLICT);
    assert_eq!(json_body(stale).await["code"], json!("conflict"));
    let after_stale = json_body(read(&fixture.app, &fixture.auth, "").await).await;
    assert_eq!(
        after_stale, current,
        "a stale revision changes nothing, including the revision"
    );

    let applied = save(
        &fixture.app,
        &fixture.auth,
        &json!({
            "entries": [bravo.to_string()],
            "current_index": 0,
            "position_ms": 20,
            "revision": 1,
        }),
    )
    .await;

    assert_eq!(applied.status(), StatusCode::OK);
    let applied_body = json_body(applied).await;
    assert_eq!(applied_body["revision"], json!(2));
    assert_eq!(track_ids(&applied_body), vec![bravo.to_string()]);

    let last_write_wins = save(
        &fixture.app,
        &fixture.auth,
        &json!({
            "entries": [amber.to_string(), bravo.to_string()],
            "current_index": 1,
            "position_ms": 30,
        }),
    )
    .await;

    assert_eq!(last_write_wins.status(), StatusCode::OK);
    let last_body = json_body(last_write_wins).await;
    assert_eq!(last_body["revision"], json!(3));
    assert_eq!(
        track_ids(&last_body),
        vec![amber.to_string(), bravo.to_string()]
    );
}

#[sqlx::test]
async fn an_unavailable_track_keeps_its_entry(pool: PgPool) {
    let fixture = fixture(pool).await;
    let owned = track(&fixture.ports, "path-amber", "amber").await;
    let remote = provider_track(&fixture.ports, "vid-1", "remote").await;
    let saved = save(
        &fixture.app,
        &fixture.auth,
        &json!({
            "entries": [owned.to_string(), remote.to_string()],
            "current_index": 1,
            "position_ms": 0,
        }),
    )
    .await;

    assert_eq!(saved.status(), StatusCode::OK);
    let body = json_body(saved).await;
    assert_eq!(body["total"], json!(2));
    assert_eq!(
        track_ids(&body),
        vec![owned.to_string(), remote.to_string()]
    );
    assert_eq!(positions(&body), vec![0, 1]);
    assert_eq!(body["items"][0]["track"]["availability"], json!("owned"));
    assert_eq!(body["items"][0]["track"]["ownership"], json!("owned"));
    assert_eq!(body["items"][1]["track"]["id"], json!(remote.to_string()));
    assert_eq!(
        body["items"][1]["track"]["availability"],
        json!("unavailable"),
        "an entry whose track is unavailable keeps its availability rather than being dropped"
    );
    assert_eq!(body["items"][1]["track"]["ownership"], json!("provider"));
    assert!(
        body["items"][1]["entry_id"]
            .as_str()
            .is_some_and(|id| !id.is_empty())
    );

    let read_back = json_body(read(&fixture.app, &fixture.auth, "").await).await;
    assert_eq!(read_back, body);
}

#[sqlx::test]
async fn an_unknown_parameter_is_rejected(pool: PgPool) {
    let fixture = fixture(pool).await;
    let amber = track(&fixture.ports, "path-amber", "amber").await;
    let valid = json!({
        "entries": [amber.to_string()],
        "current_index": 0,
        "position_ms": 0,
    });
    let cases = [
        ("GET", "/api/v1/queue?limit=0", None),
        ("GET", "/api/v1/queue?offset=first", None),
        ("GET", "/api/v1/queue?seed=1", None),
        ("PUT", "/api/v1/queue?limit=1", Some(&valid)),
        ("PUT", "/api/v1/queue?seed=1", Some(&valid)),
        ("DELETE", "/api/v1/queue?offset=1", None),
        (
            "PUT",
            "/api/v1/queue",
            Some(&json!({
                "entries": ["not-a-track"],
                "current_index": 0,
                "position_ms": 0,
            })),
        ),
        (
            "PUT",
            "/api/v1/queue",
            Some(&json!({
                "entries": [amber.to_string()],
                "current_index": 0,
                "position_ms": 0,
                "index": 3,
            })),
        ),
        (
            "PUT",
            "/api/v1/queue",
            Some(&json!({
                "entries": [amber.to_string()],
                "current_index": 0,
            })),
        ),
        (
            "PUT",
            "/api/v1/queue",
            Some(&json!({
                "entries": [amber.to_string()],
                "current_index": 0,
                "position_ms": i64::MAX as u64 + 1,
            })),
        ),
    ];

    for (method, uri, body) in cases {
        let response = send(&fixture.app, method, &with_auth(uri, &fixture.auth), body).await;
        assert_eq!(
            response.status(),
            StatusCode::BAD_REQUEST,
            "{method} {uri} is rejected"
        );
        let body = json_body(response).await;
        assert_eq!(body["code"], json!("invalid_request"), "{method} {uri}");
        assert_eq!(
            body["message"],
            json!("The request is invalid"),
            "{method} {uri}"
        );
        assert_eq!(body["request_id"], json!(support::TEST_REQUEST_ID));
    }
}

#[sqlx::test]
async fn the_total_is_the_count_not_the_page(pool: PgPool) {
    let fixture = fixture(pool).await;
    let amber = track(&fixture.ports, "path-amber", "amber").await;
    let saved = save(
        &fixture.app,
        &fixture.auth,
        &json!({
            "entries": vec![amber.to_string(); 60],
            "current_index": 0,
            "position_ms": 0,
        }),
    )
    .await;

    assert_eq!(saved.status(), StatusCode::OK);
    let saved_body = json_body(saved).await;
    assert_eq!(saved_body["total"], json!(60));
    assert_eq!(saved_body["items"].as_array().unwrap().len(), 60);

    let page = read(&fixture.app, &fixture.auth, "").await;
    assert_eq!(page.status(), StatusCode::OK);
    assert_eq!(page.headers()["x-total-count"], "60");
    let page_body = json_body(page).await;
    assert_eq!(page_body["total"], json!(60));
    assert_eq!(page_body["items"].as_array().unwrap().len(), 50);

    let tail = read(&fixture.app, &fixture.auth, "limit=10&offset=55").await;
    assert_eq!(tail.status(), StatusCode::OK);
    assert_eq!(tail.headers()["x-total-count"], "60");
    let tail_body = json_body(tail).await;
    assert_eq!(tail_body["total"], json!(60));
    assert_eq!(positions(&tail_body), vec![55, 56, 57, 58, 59]);
    assert_eq!(tail_body["revision"], json!(1));
    assert_eq!(tail_body["current_index"], json!(0));

    let past_end = read(&fixture.app, &fixture.auth, "offset=60").await;
    assert_eq!(past_end.status(), StatusCode::OK);
    assert_eq!(past_end.headers()["x-total-count"], "60");
    let past_end_body = json_body(past_end).await;
    assert_eq!(past_end_body["total"], json!(60));
    assert_eq!(past_end_body["items"], json!([]));
}
