#![cfg(feature = "postgres-integration")]

mod support;

use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode},
    response::Response,
};
use base64::Engine;
use base64::engine::general_purpose::STANDARD_NO_PAD;
use chrono::Utc;
use serde_json::{Value, json};
use sqlx::PgPool;
use std::sync::Arc;
use tower::ServiceExt;
use uuid::Uuid;
use whio_core::{
    app::{AppState, router},
    features::{
        identity::{credentials, provision, secrets::CredentialKey, service::CredentialService},
        music_library::{
            availability::ProviderRegistry,
            catalogue::{LibraryService, SearchService},
            domain::{ProviderId, SourceIdentity, SourceScope, TrackId, TrackMetadata},
            history::ListenService,
            queue::RestoreQueueService,
            test_support::{LibraryRead, ListenStore, RestoreQueueStore},
        },
        playback::PlaybackActivityService,
    },
};

use support::StorePorts;

const FLYNN: &str = "flynn";
const SABINE: &str = "sabine";
const PASSWORD: &str = "integration-password";

fn credential_key() -> CredentialKey {
    CredentialKey::from_base64(&STANDARD_NO_PAD.encode([11u8; 32])).expect("valid test key")
}

fn local_source(external_id: &str) -> SourceIdentity {
    SourceIdentity::new(
        ProviderId::new("local".to_owned()).unwrap(),
        SourceScope::Named("/music".to_owned()),
        external_id.to_owned(),
    )
    .unwrap()
}

fn metadata(title: &str) -> TrackMetadata {
    TrackMetadata::from_names(title.to_owned(), vec!["Artist".to_owned()], Some(240_000)).unwrap()
}

struct Fixture {
    app: Router,
    pool: PgPool,
    ports: StorePorts,
    flynn: String,
    sabine: String,
}

async fn fixture(pool: PgPool) -> Fixture {
    let key = credential_key();
    let provisioning = provision(pool.clone());
    let mut auth = Vec::new();

    for (username, display_name) in [(FLYNN, "Flynn"), (SABINE, "Sabine")] {
        provisioning
            .create_user(username, display_name)
            .await
            .unwrap();
        let (_, secret) = provisioning
            .mint_app_password(&key, username, PASSWORD)
            .await
            .unwrap();
        auth.push(auth_query(username, &secret));
    }

    let credential: Arc<CredentialService> = Arc::new(credentials(pool.clone(), key));
    let ports = StorePorts::new(pool.clone());
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
    let listen = Arc::new(ListenService::new(
        Arc::clone(&ports.store) as Arc<dyn ListenStore>,
        Arc::clone(&ports.store) as Arc<dyn LibraryRead>,
    ));
    let activity = Arc::new(PlaybackActivityService::new(
        Arc::clone(&listen),
        Arc::clone(&library),
    ));
    let queue = Arc::new(RestoreQueueService::new(
        Arc::clone(&ports.store) as Arc<dyn RestoreQueueStore>
    ));
    let playback = ports.streaming(Vec::new());
    let app = router(
        AppState::new(credential, search, library, playback)
            .with_listen(listen)
            .with_playback_activity(activity)
            .with_queue(queue),
    );

    let mut auth = auth.into_iter();
    let flynn = auth.next().unwrap();
    let sabine = auth.next().unwrap();

    Fixture {
        app,
        pool,
        ports,
        flynn,
        sabine,
    }
}

fn auth_query(username: &str, password: &str) -> String {
    format!("?u={username}&v=1.16.1&c=integration-test&p={password}")
}

fn with_auth(uri: &str, auth: &str) -> String {
    let separator = if uri.contains('?') { '&' } else { '?' };

    format!("{uri}{separator}{}", auth.trim_start_matches('?'))
}

async fn send(app: &Router, method: &str, uri: &str, body: Option<&Value>) -> Response {
    let builder = Request::builder()
        .method(method)
        .uri(uri)
        .header("x-request-id", "request-123");
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

async fn track(ports: &StorePorts, external_id: &str) -> TrackId {
    ports
        .catalogue()
        .get_or_create(local_source(external_id), metadata(external_id))
        .await
        .unwrap()
        .id()
        .clone()
}

async fn user_id(pool: &PgPool, username: &str) -> Uuid {
    sqlx::query_scalar("SELECT id FROM users WHERE username = $1")
        .bind(username)
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn counter(pool: &PgPool, user: Uuid, track: &TrackId) -> Option<i32> {
    sqlx::query_scalar(
        "SELECT play_count FROM track_play_stats WHERE user_id = $1 AND track_id = $2",
    )
    .bind(user)
    .bind(track.as_uuid())
    .fetch_optional(pool)
    .await
    .unwrap()
}

async fn ledger_rows(pool: &PgPool, user: Uuid, track: &TrackId) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM listens WHERE user_id = $1 AND track_id = $2")
        .bind(user)
        .bind(track.as_uuid())
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn seed_counter(pool: &PgPool, user: Uuid, track: &TrackId, plays: i32) {
    sqlx::query(
        "INSERT INTO track_play_stats (user_id, track_id, play_count, last_played_at) \
         VALUES ($1, $2, $3, CURRENT_TIMESTAMP)",
    )
    .bind(user)
    .bind(track.as_uuid())
    .bind(plays)
    .execute(pool)
    .await
    .unwrap();
}

fn submission(
    track: &TrackId,
    origin: &str,
    at: &str,
    session: Option<&str>,
    extent: Option<(u64, u64, bool)>,
) -> Value {
    let mut body = json!({
        "track_id": track.to_string(),
        "origin": origin,
        "at": at,
    });

    if let Some(session) = session {
        body["session_id"] = json!(session);
    }

    if let Some((start_position_ms, listened_ms, qualified)) = extent {
        body["extent"] = json!({
            "start_position_ms": start_position_ms,
            "listened_ms": listened_ms,
            "qualified": qualified,
        });
    }

    body
}

async fn submit(app: &Router, auth: &str, body: &Value) -> Response {
    send(app, "POST", &with_auth("/api/v1/listens", auth), Some(body)).await
}

async fn history(app: &Router, auth: &str, parameters: &str) -> Response {
    let uri = if parameters.is_empty() {
        "/api/v1/history".to_owned()
    } else {
        format!("/api/v1/history?{parameters}")
    };

    send(app, "GET", &with_auth(&uri, auth), None).await
}

async fn read_activity(app: &Router, auth: &str, parameters: &str) -> Response {
    let uri = if parameters.is_empty() {
        "/api/v1/activity".to_owned()
    } else {
        format!("/api/v1/activity?{parameters}")
    };

    send(app, "GET", &with_auth(&uri, auth), None).await
}

async fn report(app: &Router, auth: &str, body: &Value) -> Response {
    send(
        app,
        "POST",
        &with_auth("/api/v1/activity", auth),
        Some(body),
    )
    .await
}

async fn read_queue(app: &Router, auth: &str) -> Response {
    send(app, "GET", &with_auth("/api/v1/queue", auth), None).await
}

fn queue_snapshot(body: &Value) -> Value {
    let entry_ids = body["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["entry_id"].clone())
        .collect::<Vec<_>>();

    json!({
        "revision": body["revision"],
        "current_index": body["current_index"],
        "position_ms": body["position_ms"],
        "changed_at": body["changed_at"],
        "entry_ids": entry_ids,
    })
}

#[sqlx::test]
async fn a_submission_records_its_extent(pool: PgPool) {
    let fixture = fixture(pool).await;
    let track = track(&fixture.ports, "extent.flac").await;
    let at = "2026-09-01T10:00:00Z";

    let recorded = submit(
        &fixture.app,
        &fixture.flynn,
        &submission(
            &track,
            "native",
            at,
            Some("session-extent"),
            Some((1_000, 240_000, true)),
        ),
    )
    .await;

    assert_eq!(recorded.status(), StatusCode::OK);
    let recorded = json_body(recorded).await;
    assert_eq!(recorded["outcome"], json!("recorded"));
    assert_eq!(recorded["item"]["track_id"], json!(track.to_string()));
    assert_eq!(recorded["item"]["origin"], json!("native"));
    assert_eq!(
        recorded["item"]["listen_at"],
        json!("2026-09-01T10:00:00+00:00")
    );
    assert_eq!(recorded["item"]["session_id"], json!("session-extent"));
    assert_eq!(
        recorded["item"]["extent"],
        json!({"start_position_ms": 1_000, "listened_ms": 240_000, "qualified": true})
    );

    let read = history(&fixture.app, &fixture.flynn, "").await;

    assert_eq!(read.status(), StatusCode::OK);
    assert_eq!(read.headers()["x-total-count"], "1");
    let read = json_body(read).await;
    assert_eq!(read["items"], json!([recorded["item"].clone()]));
    assert_eq!(read["total"], json!(1));
    assert_eq!(read["total_kind"], json!("all_time_plays"));
    assert_eq!(
        read["coverage"],
        json!({"since": "2026-09-01T10:00:00+00:00"})
    );
    assert_eq!(
        counter(&fixture.pool, user_id(&fixture.pool, FLYNN).await, &track).await,
        Some(1)
    );
}

#[sqlx::test]
async fn a_short_submission_is_recorded_and_does_not_count(pool: PgPool) {
    let fixture = fixture(pool).await;
    let track = track(&fixture.ports, "four-minutes.flac").await;

    let recorded = submit(
        &fixture.app,
        &fixture.flynn,
        &submission(
            &track,
            "native",
            "2026-09-02T10:00:00Z",
            Some("session-skip"),
            Some((0, 12_000, false)),
        ),
    )
    .await;

    assert_eq!(recorded.status(), StatusCode::OK);
    let recorded = json_body(recorded).await;
    assert_eq!(recorded["outcome"], json!("recorded"));
    assert_eq!(
        recorded["item"]["extent"],
        json!({"start_position_ms": 0, "listened_ms": 12_000, "qualified": false})
    );

    let read = history(&fixture.app, &fixture.flynn, "").await;

    assert_eq!(read.status(), StatusCode::OK);
    assert_eq!(read.headers()["x-total-count"], "0");
    let read = json_body(read).await;
    assert_eq!(read["items"].as_array().unwrap().len(), 1);
    assert_eq!(
        read["items"][0]["extent"],
        json!({"start_position_ms": 0, "listened_ms": 12_000, "qualified": false})
    );
    assert_eq!(read["total"], json!(0));
    let user = user_id(&fixture.pool, FLYNN).await;
    assert_eq!(ledger_rows(&fixture.pool, user, &track).await, 1);
    assert_eq!(counter(&fixture.pool, user, &track).await, None);

    let qualified = submit(
        &fixture.app,
        &fixture.flynn,
        &submission(
            &track,
            "native",
            "2026-09-02T11:00:00Z",
            Some("session-played"),
            Some((0, 240_000, true)),
        ),
    )
    .await;
    assert_eq!(qualified.status(), StatusCode::OK);

    let mixed = json_body(history(&fixture.app, &fixture.flynn, "").await).await;
    assert_eq!(mixed["items"].as_array().unwrap().len(), 2);
    assert_eq!(
        mixed["total"],
        json!(1),
        "one qualified listen and one skip: the total is the counter, not the two rows"
    );
    assert_eq!(ledger_rows(&fixture.pool, user, &track).await, 2);
    assert_eq!(counter(&fixture.pool, user, &track).await, Some(1));
}

#[sqlx::test]
async fn an_exact_repeat_submits_once(pool: PgPool) {
    let fixture = fixture(pool).await;
    let track = track(&fixture.ports, "repeat.flac").await;
    let body = submission(
        &track,
        "native",
        "2026-09-03T10:00:00Z",
        Some("session-repeat"),
        Some((0, 240_000, true)),
    );

    let first = submit(&fixture.app, &fixture.flynn, &body).await;
    let second = submit(&fixture.app, &fixture.flynn, &body).await;

    assert_eq!(first.status(), StatusCode::OK);
    assert_eq!(json_body(first).await["outcome"], json!("recorded"));
    assert_eq!(second.status(), StatusCode::OK);
    assert_eq!(json_body(second).await["outcome"], json!("duplicate"));

    let read = json_body(history(&fixture.app, &fixture.flynn, "").await).await;
    assert_eq!(read["items"].as_array().unwrap().len(), 1);
    assert_eq!(read["total"], json!(1));

    let user = user_id(&fixture.pool, FLYNN).await;
    assert_eq!(ledger_rows(&fixture.pool, user, &track).await, 1);
    assert_eq!(counter(&fixture.pool, user, &track).await, Some(1));
}

#[sqlx::test]
async fn a_report_never_writes_the_ledger(pool: PgPool) {
    let fixture = fixture(pool).await;
    let track = track(&fixture.ports, "report.flac").await;

    let reported = report(
        &fixture.app,
        &fixture.flynn,
        &json!({
            "client": "device-a",
            "track_id": track.to_string(),
            "state": "playing",
            "position_ms": 0,
            "at": Utc::now().to_rfc3339(),
        }),
    )
    .await;

    assert_eq!(reported.status(), StatusCode::OK);
    let reported = json_body(reported).await;
    assert_eq!(reported["client"], json!("device-a"));
    assert_eq!(reported["state"], json!("playing"));
    assert_eq!(reported["listened_ms"], json!(0));
    assert_eq!(reported["submitted"], json!(false));

    let live = read_activity(&fixture.app, &fixture.flynn, "").await;

    assert_eq!(live.status(), StatusCode::OK);
    let live = json_body(live).await;
    assert_eq!(live["items"].as_array().unwrap().len(), 1);
    assert_eq!(live["items"][0]["client"], json!("device-a"));
    assert_eq!(live["items"][0]["track_id"], json!(track.to_string()));

    let read = json_body(history(&fixture.app, &fixture.flynn, "").await).await;
    assert_eq!(read["items"], json!([]));
    assert_eq!(read["total"], json!(0));

    let user = user_id(&fixture.pool, FLYNN).await;
    assert_eq!(ledger_rows(&fixture.pool, user, &track).await, 0);
    assert_eq!(counter(&fixture.pool, user, &track).await, None);
}

#[sqlx::test]
async fn the_activity_read_is_not_the_queue(pool: PgPool) {
    let fixture = fixture(pool).await;
    let track = track(&fixture.ports, "queue.flac").await;

    let saved = send(
        &fixture.app,
        "PUT",
        &with_auth("/api/v1/queue", &fixture.flynn),
        Some(&json!({
            "entries": [track.to_string()],
            "current_index": 0,
            "position_ms": 4_200,
        })),
    )
    .await;
    assert_eq!(saved.status(), StatusCode::OK);
    let before = json_body(read_queue(&fixture.app, &fixture.flynn).await).await;
    assert_eq!(before["revision"], json!(1));

    let reported = report(
        &fixture.app,
        &fixture.flynn,
        &json!({
            "client": "device-b",
            "track_id": track.to_string(),
            "state": "playing",
            "position_ms": 1_000,
            "playback_rate": 1.0,
            "at": Utc::now().to_rfc3339(),
        }),
    )
    .await;
    assert_eq!(reported.status(), StatusCode::OK);

    let after = json_body(read_queue(&fixture.app, &fixture.flynn).await).await;
    assert_eq!(
        queue_snapshot(&after),
        queue_snapshot(&before),
        "a report leaves the restore queue alone"
    );

    let live = json_body(read_activity(&fixture.app, &fixture.flynn, "").await).await;
    let session = live["items"][0].clone();
    for identity in [
        "entry_id",
        "entries",
        "revision",
        "current_index",
        "occurrence",
    ] {
        assert!(
            session.get(identity).is_none(),
            "a live session carries no queue identity: {identity}"
        );
    }
    assert!(live.get("revision").is_none());
    assert_eq!(session["client"], json!("device-b"));
    assert!(!session["session_id"].as_str().unwrap().is_empty());
}

#[sqlx::test]
async fn coverage_is_reported(pool: PgPool) {
    let fixture = fixture(pool).await;
    let track = track(&fixture.ports, "marker.flac").await;
    let marker = "2026-09-10T00:00:00Z";

    let recorded = submit(
        &fixture.app,
        &fixture.flynn,
        &submission(
            &track,
            "native",
            marker,
            Some("session-marker"),
            Some((0, 240_000, true)),
        ),
    )
    .await;
    assert_eq!(recorded.status(), StatusCode::OK);

    let before = history(
        &fixture.app,
        &fixture.flynn,
        "since=2026-09-09T22:00:00Z&until=2026-09-09T23:00:00Z",
    )
    .await;

    assert_eq!(before.status(), StatusCode::OK);
    let before = json_body(before).await;
    assert_eq!(before["items"], json!([]));
    assert_eq!(before["total"], json!(0));
    assert_eq!(before["coverage"], json!({"since": null}));

    let covered_empty = history(
        &fixture.app,
        &fixture.flynn,
        "since=2026-09-10T01:00:00Z&until=2026-09-10T02:00:00Z",
    )
    .await;

    assert_eq!(covered_empty.status(), StatusCode::OK);
    let covered_empty = json_body(covered_empty).await;
    assert_eq!(covered_empty["items"], json!([]));
    assert_eq!(covered_empty["total"], json!(0));
    assert_eq!(
        covered_empty["coverage"],
        json!({"since": "2026-09-10T00:00:00+00:00"}),
        "an empty covered window is distinguishable from an uncovered one"
    );

    let covering = history(
        &fixture.app,
        &fixture.flynn,
        "since=2026-09-09T23:00:00Z&until=2026-09-10T01:00:00Z",
    )
    .await;

    let covering = json_body(covering).await;
    assert_eq!(covering["items"].as_array().unwrap().len(), 1);
    assert_eq!(covering["total"], json!(1));
    assert_eq!(
        covering["coverage"],
        json!({"since": "2026-09-10T00:00:00+00:00"})
    );
}

#[sqlx::test]
async fn an_unknown_parameter_is_rejected(pool: PgPool) {
    let fixture = fixture(pool).await;
    let track = track(&fixture.ports, "rejected.flac").await;
    let at = "2026-09-11T10:00:00Z";
    let body = submission(
        &track,
        "native",
        at,
        Some("session-rejected"),
        Some((0, 240_000, true)),
    );
    let report_body = json!({
        "client": "device-c",
        "track_id": track.to_string(),
        "state": "playing",
        "position_ms": 0,
        "at": at,
    });

    let tagged = format!("{}&bogus=1", fixture.flynn);

    let rejected = [
        history(&fixture.app, &fixture.flynn, "bogus=1").await,
        read_activity(&fixture.app, &fixture.flynn, "bogus=1").await,
        submit(&fixture.app, &tagged, &body).await,
        report(&fixture.app, &tagged, &report_body).await,
        history(&fixture.app, &fixture.flynn, "order=sideways").await,
        submit(
            &fixture.app,
            &fixture.flynn,
            &submission(&track, "nowhere", at, None, Some((0, 240_000, true))),
        )
        .await,
        report(
            &fixture.app,
            &fixture.flynn,
            &json!({
                "client": "device-c",
                "track_id": track.to_string(),
                "state": "playing",
                "position_ms": 0,
                "at": at,
                "ignore_scrobble": true,
            }),
        )
        .await,
        report(
            &fixture.app,
            &fixture.flynn,
            &json!({
                "client": "device-c",
                "track_id": track.to_string(),
                "state": "sideways",
                "position_ms": 0,
                "at": at,
            }),
        )
        .await,
    ];

    for response in rejected {
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(json_body(response).await["code"], json!("invalid_request"));
    }

    let unknown_track = TrackId::new();
    let unknown = submit(
        &fixture.app,
        &fixture.flynn,
        &submission(&unknown_track, "native", at, None, Some((0, 240_000, true))),
    )
    .await;

    assert_eq!(unknown.status(), StatusCode::BAD_REQUEST);
    assert_eq!(json_body(unknown).await["code"], json!("invalid_request"));
    assert_eq!(
        ledger_rows(&fixture.pool, user_id(&fixture.pool, FLYNN).await, &track).await,
        0,
        "a rejected request records nothing"
    );
}

#[sqlx::test]
async fn the_total_is_the_count_not_the_page(pool: PgPool) {
    let fixture = fixture(pool).await;
    let track = track(&fixture.ports, "paged.flac").await;
    let user = user_id(&fixture.pool, FLYNN).await;
    seed_counter(&fixture.pool, user, &track, 5).await;

    for (session, at) in [
        ("session-one", "2026-09-12T10:00:00Z"),
        ("session-two", "2026-09-12T11:00:00Z"),
        ("session-three", "2026-09-12T12:00:00Z"),
    ] {
        let recorded = submit(
            &fixture.app,
            &fixture.flynn,
            &submission(
                &track,
                "native",
                at,
                Some(session),
                Some((0, 240_000, true)),
            ),
        )
        .await;
        assert_eq!(recorded.status(), StatusCode::OK);
    }

    let newest = history(&fixture.app, &fixture.flynn, "limit=1&offset=0").await;

    assert_eq!(newest.status(), StatusCode::OK);
    assert_eq!(newest.headers()["x-total-count"], "8");
    let newest = json_body(newest).await;
    assert_eq!(newest["items"].as_array().unwrap().len(), 1);
    assert_eq!(newest["items"][0]["session_id"], json!("session-three"));
    assert_eq!(newest["total"], json!(8));
    assert_eq!(newest["total_kind"], json!("all_time_plays"));
    assert_eq!(counter(&fixture.pool, user, &track).await, Some(8));

    let deeper = history(&fixture.app, &fixture.flynn, "limit=1&offset=2").await;

    assert_eq!(deeper.status(), StatusCode::OK);
    assert_eq!(deeper.headers()["x-total-count"], "8");
    let deeper = json_body(deeper).await;
    assert_eq!(deeper["items"].as_array().unwrap().len(), 1);
    assert_eq!(deeper["items"][0]["session_id"], json!("session-one"));
    assert_eq!(deeper["total"], json!(8));

    let repeated = history(&fixture.app, &fixture.flynn, "limit=1&offset=2").await;
    assert_eq!(json_body(repeated).await, deeper, "paging is stable");

    let oldest = json_body(
        history(
            &fixture.app,
            &fixture.flynn,
            "limit=1&offset=0&order=oldest",
        )
        .await,
    )
    .await;
    assert_eq!(oldest["items"][0]["session_id"], json!("session-one"));

    let windowed = history(
        &fixture.app,
        &fixture.flynn,
        "limit=1&offset=0&since=2026-09-12T00:00:00Z",
    )
    .await;

    assert_eq!(windowed.status(), StatusCode::OK);
    assert_eq!(windowed.headers()["x-total-count"], "3");
    let windowed = json_body(windowed).await;
    assert_eq!(windowed["items"].as_array().unwrap().len(), 1);
    assert_eq!(windowed["total"], json!(3));
    assert_eq!(windowed["total_kind"], json!("windowed_plays"));
}

#[sqlx::test]
async fn a_second_principal_sees_only_its_own_history(pool: PgPool) {
    let fixture = fixture(pool).await;
    let track = track(&fixture.ports, "private.flac").await;

    let recorded = submit(
        &fixture.app,
        &fixture.flynn,
        &submission(
            &track,
            "native",
            "2026-09-13T10:00:00Z",
            Some("session-private"),
            Some((0, 240_000, true)),
        ),
    )
    .await;
    assert_eq!(recorded.status(), StatusCode::OK);

    let mine = json_body(history(&fixture.app, &fixture.flynn, "").await).await;
    assert_eq!(mine["items"].as_array().unwrap().len(), 1);
    assert_eq!(mine["total"], json!(1));

    let theirs = json_body(history(&fixture.app, &fixture.sabine, "").await).await;
    assert_eq!(theirs["items"], json!([]));
    assert_eq!(theirs["total"], json!(0));
    assert_eq!(theirs["coverage"], json!({"since": null}));
}
