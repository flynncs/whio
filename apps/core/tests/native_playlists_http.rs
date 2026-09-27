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
use serde_json::{Value, json};
use sqlx::PgPool;
use std::sync::Arc;
use tower::ServiceExt;
use uuid::Uuid;
use whio_core::{
    features::identity::secrets::CredentialKey,
    features::music_library::{
        catalogue::{LibraryService, SearchService},
        domain::{ProviderId, SourceIdentity, SourceScope, TrackId, TrackMetadata},
        playlists::PlaylistService,
        test_support::PlaylistStore,
    },
};

use support::{StorePorts, router_with_playlists};

struct Fixture {
    app: Router,
    owner: String,
    stranger: String,
    owner_id: String,
    stranger_id: String,
}

fn source(external_id: &str) -> SourceIdentity {
    SourceIdentity::new(
        ProviderId::new("local".to_owned()).unwrap(),
        SourceScope::Named("/music".to_owned()),
        external_id.to_owned(),
    )
    .unwrap()
}

fn metadata(title: &str) -> TrackMetadata {
    TrackMetadata::from_names(title.to_owned(), vec!["Artist".to_owned()], Some(1_000)).unwrap()
}

async fn fixture(pool: PgPool) -> Fixture {
    let key = CredentialKey::from_base64(&STANDARD_NO_PAD.encode([11u8; 32])).unwrap();
    let provisioning = whio_core::features::identity::provision(pool.clone());
    let owner_id = provisioning.create_user("flynn", "Flynn").await.unwrap();
    let stranger_id = provisioning.create_user("other", "Other").await.unwrap();
    let (_, owner_secret) = provisioning
        .mint_app_password(&key, "flynn", "integration")
        .await
        .unwrap();
    let (_, stranger_secret) = provisioning
        .mint_app_password(&key, "other", "integration")
        .await
        .unwrap();
    let credential = Arc::new(whio_core::features::identity::credentials(
        pool.clone(),
        key,
    ));
    let ports = StorePorts::new(pool);
    let library = Arc::new(LibraryService::new(
        ports.library(),
        ports.sources(),
        ports.state(),
    ));
    let search = Arc::new(SearchService::new(
        ports.library(),
        ports.catalogue(),
        ports.sources(),
        ports.state(),
    ));
    let playlists = Arc::new(PlaylistService::new(
        Arc::clone(&ports.store) as Arc<dyn PlaylistStore>,
        Arc::clone(&library),
    ));
    let playback = ports.streaming(vec![]);
    let app = router_with_playlists(credential, search, library, playlists, playback);

    Fixture {
        app,
        owner: auth("flynn", &owner_secret),
        stranger: auth("other", &stranger_secret),
        owner_id: owner_id.to_string(),
        stranger_id: stranger_id.to_string(),
    }
}

fn auth(username: &str, secret: &str) -> String {
    format!("?u={username}&v=1.16.1&c=integration-test&p={secret}")
}

fn with_auth(uri: &str, auth: &str) -> String {
    let separator = if uri.contains('?') { '&' } else { '?' };

    format!("{uri}{separator}{}", auth.trim_start_matches('?'))
}

async fn track(ports: &StorePorts, external_id: &str, title: &str) -> TrackId {
    ports
        .catalogue()
        .get_or_create(source(external_id), metadata(title))
        .await
        .unwrap()
        .id()
        .clone()
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

async fn body_of(response: Response) -> Value {
    let bytes = to_bytes(response.into_body(), 16 * 1024 * 1024)
        .await
        .unwrap();

    serde_json::from_slice(&bytes).unwrap()
}

async fn create(app: &Router, auth: &str, name: &str, comment: Option<&str>) -> Value {
    let body = match comment {
        Some(comment) => json!({ "name": name, "comment": comment }),
        None => json!({ "name": name }),
    };
    let response = send(
        app,
        "POST",
        &with_auth("/api/v1/playlists", auth),
        Some(&body),
    )
    .await;
    assert_eq!(response.status(), StatusCode::CREATED);

    body_of(response).await
}

async fn edit(app: &Router, auth: &str, id: &str, body: Value) -> Response {
    send(
        app,
        "PATCH",
        &with_auth(&format!("/api/v1/playlists/{id}"), auth),
        Some(&body),
    )
    .await
}

async fn detail(app: &Router, auth: &str, id: &str) -> Response {
    send(
        app,
        "GET",
        &with_auth(&format!("/api/v1/playlists/{id}"), auth),
        None,
    )
    .await
}

async fn detail_body(app: &Router, auth: &str, id: &str) -> Value {
    let response = detail(app, auth, id).await;
    assert_eq!(response.status(), StatusCode::OK);

    body_of(response).await
}

async fn move_entry(app: &Router, auth: &str, id: &str, entry_id: &str, body: Value) -> Response {
    send(
        app,
        "POST",
        &with_auth(
            &format!("/api/v1/playlists/{id}/entries/{entry_id}/move"),
            auth,
        ),
        Some(&body),
    )
    .await
}

fn entry_ids(body: &Value) -> Vec<String> {
    body["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry["entry_id"].as_str().unwrap().to_owned())
        .collect()
}

fn entry_titles(body: &Value) -> Vec<String> {
    body["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry["track"]["title"].as_str().unwrap().to_owned())
        .collect()
}

fn entry_positions(body: &Value) -> Vec<u64> {
    body["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry["position"].as_u64().unwrap())
        .collect()
}

async fn add_tracks(app: &Router, auth: &str, id: &str, tracks: &[&TrackId]) -> Value {
    let additions = tracks
        .iter()
        .map(|track| track.to_string())
        .collect::<Vec<_>>();
    let response = edit(app, auth, id, json!({ "additions": additions })).await;
    assert_eq!(response.status(), StatusCode::OK);

    body_of(response).await
}

#[sqlx::test]
async fn native_playlist_list_covers_owned_and_public(pool: PgPool) {
    let fixture = fixture(pool).await;
    let owned = create(&fixture.app, &fixture.owner, "Mine", None).await;
    let shared = create(&fixture.app, &fixture.stranger, "Shared", None).await;
    let hidden = create(&fixture.app, &fixture.stranger, "Hidden", None).await;
    let response = edit(
        &fixture.app,
        &fixture.stranger,
        shared["id"].as_str().unwrap(),
        json!({ "public": true }),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);

    let response = send(
        &fixture.app,
        "GET",
        &with_auth("/api/v1/playlists", &fixture.owner),
        None,
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers().get("x-total-count").unwrap(),
        "2",
        "the header carries the matching total, not the page length"
    );
    let body = body_of(response).await;
    assert_eq!(body["total"], json!(2));
    let ids = body["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["id"].as_str().unwrap().to_owned())
        .collect::<Vec<_>>();
    assert!(ids.contains(&owned["id"].as_str().unwrap().to_owned()));
    assert!(ids.contains(&shared["id"].as_str().unwrap().to_owned()));
    assert!(
        !ids.contains(&hidden["id"].as_str().unwrap().to_owned()),
        "another principal's private playlist is invisible"
    );
    let listed = body["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["id"] == shared["id"])
        .unwrap();
    assert_eq!(listed["owner"]["id"], json!(fixture.stranger_id));
    assert_eq!(listed["owner"]["username"], json!("other"));

    let response = send(
        &fixture.app,
        "GET",
        &with_auth("/api/v1/playlists?limit=1&offset=1", &fixture.owner),
        None,
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers().get("x-total-count").unwrap(), "2");
    let page = body_of(response).await;
    assert_eq!(page["items"].as_array().unwrap().len(), 1);
    assert_eq!(page["total"], json!(2));

    let response = send(
        &fixture.app,
        "GET",
        &with_auth("/api/v1/playlists?order=name", &fixture.owner),
        None,
    )
    .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(body_of(response).await["code"], json!("invalid_request"));
}

#[sqlx::test]
async fn native_playlist_detail_carries_entries_and_owner(pool: PgPool) {
    let fixture = fixture(pool.clone()).await;
    let ports = StorePorts::new(pool.clone());
    let first = track(&ports, "path-first", "First").await;
    let second = track(&ports, "path-second", "Second").await;
    let created = create(&fixture.app, &fixture.owner, "Trip", None).await;
    let id = created["id"].as_str().unwrap().to_owned();
    let added = add_tracks(
        &fixture.app,
        &fixture.owner,
        &id,
        &[&first, &second, &first],
    )
    .await;
    assert_eq!(added["entry_count"], json!(3));

    let body = detail_body(&fixture.app, &fixture.owner, &id).await;
    assert_eq!(entry_titles(&body), vec!["First", "Second", "First"]);
    assert_eq!(entry_positions(&body), vec![0, 1, 2]);
    let ids = entry_ids(&body);
    assert_eq!(
        ids.iter().collect::<std::collections::HashSet<_>>().len(),
        3,
        "a repeated track is two distinct occurrences"
    );
    assert_eq!(body["owner"]["id"], json!(fixture.owner_id));
    assert_eq!(body["owner"]["username"], json!("flynn"));
    assert_eq!(body["editable"], json!(true));
    assert_eq!(body["entry_count"], json!(3));
    assert_eq!(body["revision"], json!(1));

    let again = detail_body(&fixture.app, &fixture.owner, &id).await;
    assert_eq!(entry_ids(&again), ids, "entry ids are stable across reads");

    let response = edit(&fixture.app, &fixture.owner, &id, json!({ "public": true })).await;
    assert_eq!(response.status(), StatusCode::OK);
    let seen = detail_body(&fixture.app, &fixture.stranger, &id).await;
    assert_eq!(
        seen["owner"]["id"],
        json!(fixture.owner_id),
        "a non-owner reading a public playlist sees the real owner"
    );
    assert_eq!(seen["owner"]["username"], json!("flynn"));
    assert_eq!(seen["editable"], json!(false));
    assert_eq!(entry_titles(&seen), vec!["First", "Second", "First"]);
}

#[sqlx::test]
async fn native_playlist_write_conflicts_on_a_stale_revision(pool: PgPool) {
    let fixture = fixture(pool).await;
    let created = create(&fixture.app, &fixture.owner, "Trip", None).await;
    let id = created["id"].as_str().unwrap().to_owned();
    let read = created["revision"].as_i64().unwrap();

    let response = edit(
        &fixture.app,
        &fixture.owner,
        &id,
        json!({ "revision": read, "name": "Trip A" }),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let applied = body_of(response).await;
    assert_eq!(applied["revision"], json!(read + 1));

    let response = edit(
        &fixture.app,
        &fixture.owner,
        &id,
        json!({ "revision": read, "name": "Stale" }),
    )
    .await;
    assert_eq!(
        response.status(),
        StatusCode::CONFLICT,
        "a write carrying the revision the caller read before another edit is a conflict"
    );
    let conflict = body_of(response).await;
    assert_eq!(conflict["code"], json!("conflict"));
    assert_eq!(conflict["request_id"], json!("request-123"));
    let unchanged = detail_body(&fixture.app, &fixture.owner, &id).await;
    assert_eq!(
        unchanged["name"],
        json!("Trip A"),
        "the stale write changed nothing"
    );
    assert_eq!(unchanged["revision"], json!(read + 1));

    let response = edit(
        &fixture.app,
        &fixture.owner,
        &id,
        json!({ "name": "Trip B" }),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let last = body_of(response).await;
    assert_eq!(
        last["revision"],
        json!(read + 2),
        "a write carrying no revision is last-write-wins and reports the resulting revision"
    );
    assert_eq!(last["name"], json!("Trip B"));
}

#[sqlx::test]
async fn native_playlist_edit_distinguishes_omission_from_clearing(pool: PgPool) {
    let fixture = fixture(pool).await;
    let created = create(&fixture.app, &fixture.owner, "Trip", Some("hello")).await;
    let id = created["id"].as_str().unwrap().to_owned();
    assert_eq!(created["comment"], json!("hello"));
    assert_eq!(created["public"], json!(false));

    let response = edit(
        &fixture.app,
        &fixture.owner,
        &id,
        json!({ "name": "Trip Two" }),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let omitted = body_of(response).await;
    assert_eq!(
        omitted["comment"],
        json!("hello"),
        "an omitted comment is left alone"
    );
    assert_eq!(omitted["name"], json!("Trip Two"));

    let response = edit(&fixture.app, &fixture.owner, &id, json!({ "comment": "" })).await;
    assert_eq!(response.status(), StatusCode::OK);
    let cleared = body_of(response).await;
    assert_eq!(
        cleared["comment"],
        Value::Null,
        "an empty comment clears it"
    );
    assert_eq!(cleared["name"], json!("Trip Two"));

    let response = edit(
        &fixture.app,
        &fixture.owner,
        &id,
        json!({ "comment": "back" }),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let restored = body_of(response).await;
    assert_eq!(restored["comment"], json!("back"));

    let response = edit(&fixture.app, &fixture.owner, &id, json!({ "public": true })).await;
    assert_eq!(response.status(), StatusCode::OK);
    let public = body_of(response).await;
    assert_eq!(public["public"], json!(true));
    assert_eq!(public["comment"], json!("back"));

    let response = edit(
        &fixture.app,
        &fixture.owner,
        &id,
        json!({ "name": "Trip Three" }),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let kept = body_of(response).await;
    assert_eq!(
        kept["public"],
        json!(true),
        "an omitted public flag is left alone"
    );
    assert_eq!(kept["comment"], json!("back"));

    let response = edit(
        &fixture.app,
        &fixture.owner,
        &id,
        json!({ "public": false }),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(body_of(response).await["public"], json!(false));

    let response = edit(&fixture.app, &fixture.owner, &id, json!({ "name": "" })).await;
    assert_eq!(
        response.status(),
        StatusCode::BAD_REQUEST,
        "an empty name is a client error, not a silent clear"
    );
    assert_eq!(body_of(response).await["code"], json!("invalid_request"));
    let unchanged = detail_body(&fixture.app, &fixture.owner, &id).await;
    assert_eq!(unchanged["name"], json!("Trip Three"));
    assert_eq!(unchanged["comment"], json!("back"));
    assert_eq!(unchanged["public"], json!(false));
}

#[sqlx::test]
async fn native_playlist_move_is_zero_based_and_contiguous(pool: PgPool) {
    let fixture = fixture(pool.clone()).await;
    let ports = StorePorts::new(pool.clone());
    let first = track(&ports, "path-first", "First").await;
    let second = track(&ports, "path-second", "Second").await;
    let third = track(&ports, "path-third", "Third").await;
    let fourth = track(&ports, "path-fourth", "Fourth").await;
    let created = create(&fixture.app, &fixture.owner, "Trip", None).await;
    let id = created["id"].as_str().unwrap().to_owned();
    add_tracks(
        &fixture.app,
        &fixture.owner,
        &id,
        &[&first, &second, &third, &fourth],
    )
    .await;
    let body = detail_body(&fixture.app, &fixture.owner, &id).await;
    let ids = entry_ids(&body);
    assert_eq!(
        entry_titles(&body),
        vec!["First", "Second", "Third", "Fourth"]
    );

    let response = move_entry(
        &fixture.app,
        &fixture.owner,
        &id,
        &ids[0],
        json!({ "before": 4 }),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let moved = body_of(response).await;
    let body = detail_body(&fixture.app, &fixture.owner, &id).await;
    assert_eq!(
        entry_titles(&body),
        vec!["Second", "Third", "Fourth", "First"],
        "the first entry moves to the end"
    );
    assert_eq!(entry_positions(&body), vec![0, 1, 2, 3]);
    assert_eq!(entry_ids(&body)[3], ids[0], "the moved entry keeps its id");
    assert_eq!(moved["revision"], json!(body["revision"]));

    let response = move_entry(
        &fixture.app,
        &fixture.owner,
        &id,
        &ids[3],
        json!({ "before": 0 }),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = detail_body(&fixture.app, &fixture.owner, &id).await;
    assert_eq!(
        entry_titles(&body),
        vec!["Fourth", "Second", "Third", "First"],
        "the last entry moves to the front"
    );
    assert_eq!(entry_positions(&body), vec![0, 1, 2, 3]);

    let response = move_entry(
        &fixture.app,
        &fixture.owner,
        &id,
        &ids[1],
        json!({ "before": 3 }),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = detail_body(&fixture.app, &fixture.owner, &id).await;
    assert_eq!(
        entry_titles(&body),
        vec!["Fourth", "Third", "Second", "First"],
        "a middle entry moves down before the pre-lift target"
    );
    assert_eq!(entry_positions(&body), vec![0, 1, 2, 3]);

    let response = move_entry(
        &fixture.app,
        &fixture.owner,
        &id,
        &ids[1],
        json!({ "before": 1 }),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = detail_body(&fixture.app, &fixture.owner, &id).await;
    assert_eq!(
        entry_titles(&body),
        vec!["Fourth", "Second", "Third", "First"],
        "the same middle entry moves back up"
    );
    assert_eq!(entry_positions(&body), vec![0, 1, 2, 3]);
}

#[sqlx::test]
async fn native_playlist_edit_is_atomic(pool: PgPool) {
    let fixture = fixture(pool.clone()).await;
    let ports = StorePorts::new(pool.clone());
    let first = track(&ports, "path-first", "First").await;
    let created = create(&fixture.app, &fixture.owner, "Trip", None).await;
    let id = created["id"].as_str().unwrap().to_owned();
    let revision = add_tracks(&fixture.app, &fixture.owner, &id, &[&first]).await["revision"]
        .as_i64()
        .unwrap();
    let body = detail_body(&fixture.app, &fixture.owner, &id).await;
    let entries = entry_ids(&body);

    let response = edit(
        &fixture.app,
        &fixture.owner,
        &id,
        json!({
            "name": "Changed",
            "removals": entries,
            "additions": [Uuid::now_v7().to_string()],
        }),
    )
    .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(body_of(response).await["code"], json!("invalid_request"));

    let unchanged = detail_body(&fixture.app, &fixture.owner, &id).await;
    assert_eq!(
        unchanged["name"],
        json!("Trip"),
        "the metadata edit rolled back"
    );
    assert_eq!(unchanged["revision"], json!(revision));
    assert_eq!(entry_ids(&unchanged), entries);
    assert_eq!(entry_titles(&unchanged), vec!["First"]);
}

#[sqlx::test]
async fn native_playlist_rejects_a_non_owner_write(pool: PgPool) {
    let fixture = fixture(pool.clone()).await;
    let ports = StorePorts::new(pool.clone());
    let first = track(&ports, "path-first", "First").await;
    let created = create(&fixture.app, &fixture.owner, "Mine", None).await;
    let id = created["id"].as_str().unwrap().to_owned();
    add_tracks(&fixture.app, &fixture.owner, &id, &[&first]).await;
    let entries = entry_ids(&detail_body(&fixture.app, &fixture.owner, &id).await);
    let hidden = create(&fixture.app, &fixture.stranger, "Hidden", None).await;
    let hidden_id = hidden["id"].as_str().unwrap().to_owned();
    let response = edit(&fixture.app, &fixture.owner, &id, json!({ "public": true })).await;
    assert_eq!(response.status(), StatusCode::OK);

    let response = edit(
        &fixture.app,
        &fixture.stranger,
        &id,
        json!({ "name": "Hijacked" }),
    )
    .await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    assert_eq!(body_of(response).await["code"], json!("forbidden"));

    let response = send(
        &fixture.app,
        "DELETE",
        &with_auth(&format!("/api/v1/playlists/{id}"), &fixture.stranger),
        None,
    )
    .await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);

    let response = move_entry(
        &fixture.app,
        &fixture.stranger,
        &id,
        &entries[0],
        json!({ "before": 1 }),
    )
    .await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);

    let response = edit(
        &fixture.app,
        &fixture.owner,
        &hidden_id,
        json!({ "name": "Hijacked" }),
    )
    .await;
    assert_eq!(
        response.status(),
        StatusCode::NOT_FOUND,
        "another principal's private playlist is not even visible"
    );

    let response = detail(&fixture.app, &fixture.owner, &hidden_id).await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);

    let response = detail(&fixture.app, &fixture.stranger, &id).await;
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "a public playlist is readable by a non-owner"
    );

    let response = edit(
        &fixture.app,
        &fixture.owner,
        &id,
        json!({ "name": "Still Mine" }),
    )
    .await;
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "public does not stop the owner editing"
    );
    let kept = detail_body(&fixture.app, &fixture.owner, &id).await;
    assert_eq!(kept["name"], json!("Still Mine"));
    assert_eq!(entry_titles(&kept), vec!["First"]);
    assert_eq!(entry_ids(&kept), entries);
}
