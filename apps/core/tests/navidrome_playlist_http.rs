#![cfg(feature = "postgres-integration")]

mod support;

use std::sync::Arc;

use axum::{
    Router,
    body::{Body, to_bytes},
    http::{HeaderMap, Request, StatusCode},
    response::Response,
};
use base64::Engine;
use base64::engine::general_purpose::STANDARD_NO_PAD;
use serde_json::{Value, json};
use sqlx::PgPool;
use tower::ServiceExt;
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

const API: &str = "/compat/navidrome/api/playlist";
const NATIVE: &str = "/api/v1/playlists";

struct Fixture {
    app: Router,
    owner: String,
    stranger: String,
    owner_auth: String,
    owner_id: String,
    stranger_id: String,
    ports: StorePorts,
}

fn credential_key() -> CredentialKey {
    CredentialKey::from_base64(&STANDARD_NO_PAD.encode([11u8; 32])).unwrap()
}

fn source(external_id: &str) -> SourceIdentity {
    SourceIdentity::new(
        ProviderId::new("local".to_owned()).unwrap(),
        SourceScope::Named("/music".to_owned()),
        external_id.to_owned(),
    )
    .unwrap()
}

async fn fixture(pool: PgPool) -> Fixture {
    let key = credential_key();
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
    let owner = login(&app, "flynn", &owner_secret).await;
    let stranger = login(&app, "other", &stranger_secret).await;

    Fixture {
        app,
        owner,
        stranger,
        owner_auth: format!("?u=flynn&v=1.16.1&c=integration-test&p={owner_secret}"),
        owner_id: owner_id.to_string(),
        stranger_id: stranger_id.to_string(),
        ports,
    }
}

async fn login(app: &Router, username: &str, password: &str) -> String {
    let request = Request::builder()
        .method("POST")
        .uri("/compat/navidrome/auth/login")
        .header("x-request-id", "request-123")
        .header("content-type", "application/json")
        .body(Body::from(
            json!({"username": username, "password": password}).to_string(),
        ))
        .unwrap();
    let response = app.clone().oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), 8192).await.unwrap();
    let body: Value = serde_json::from_slice(&body).unwrap();

    body["token"].as_str().unwrap().to_owned()
}

async fn send(
    app: &Router,
    method: &str,
    path: &str,
    token: &str,
    body: Option<&Value>,
) -> Response {
    let builder = Request::builder()
        .method(method)
        .uri(path)
        .header("x-request-id", "request-123")
        .header("x-nd-authorization", format!("Bearer {token}"));
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
    let bytes = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();

    serde_json::from_slice(&bytes).unwrap()
}

async fn ok_body(response: Response) -> Value {
    assert_eq!(response.status(), StatusCode::OK);

    body_of(response).await
}

async fn list(app: &Router, token: &str) -> (HeaderMap, Value) {
    sorted_list(app, token, "name", "ASC", 0, 0).await
}

async fn detail(app: &Router, token: &str, id: &str) -> Value {
    let response = send(app, "GET", &format!("{API}/{id}"), token, None).await;

    ok_body(response).await
}

async fn read_tracks(app: &Router, token: &str, id: &str) -> (HeaderMap, Value) {
    let response = send(
        app,
        "GET",
        &format!("{API}/{id}/tracks?_start=0&_end=-1&_sort=id&_order=ASC"),
        token,
        None,
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let headers = response.headers().clone();

    (headers, body_of(response).await)
}

async fn create(app: &Router, token: &str, body: Value) -> String {
    let response = send(app, "POST", API, token, Some(&body)).await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_of(response).await;

    body["id"].as_str().unwrap().to_owned()
}

async fn add(app: &Router, token: &str, id: &str, ids: &[&TrackId]) -> Response {
    let ids = ids
        .iter()
        .map(|track| track.to_string())
        .collect::<Vec<_>>();

    send(
        app,
        "POST",
        &format!("{API}/{id}/tracks"),
        token,
        Some(&json!({ "ids": ids })),
    )
    .await
}

async fn update(app: &Router, token: &str, id: &str, body: Value) -> Response {
    send(app, "PUT", &format!("{API}/{id}"), token, Some(&body)).await
}

async fn remove(app: &Router, token: &str, id: &str, item_ids: &[&str]) -> Response {
    let query = item_ids
        .iter()
        .map(|item_id| format!("id={item_id}"))
        .collect::<Vec<_>>()
        .join("&");

    send(
        app,
        "DELETE",
        &format!("{API}/{id}/tracks?{query}"),
        token,
        None,
    )
    .await
}

async fn move_track(app: &Router, token: &str, id: &str, item_id: &str, before: &str) -> Response {
    send(
        app,
        "PUT",
        &format!("{API}/{id}/tracks/{item_id}"),
        token,
        Some(&json!({ "insert_before": before })),
    )
    .await
}

fn item_ids(songs: &Value) -> Vec<String> {
    songs
        .as_array()
        .unwrap()
        .iter()
        .map(|song| song["id"].as_str().unwrap().to_owned())
        .collect()
}

fn media_ids(songs: &Value) -> Vec<String> {
    songs
        .as_array()
        .unwrap()
        .iter()
        .map(|song| song["mediaFileId"].as_str().unwrap().to_owned())
        .collect()
}

fn titles(songs: &Value) -> Vec<String> {
    songs
        .as_array()
        .unwrap()
        .iter()
        .map(|song| song["title"].as_str().unwrap().to_owned())
        .collect()
}

async fn native_detail(app: &Router, auth: &str, id: &str) -> Value {
    let request = Request::builder()
        .method("GET")
        .uri(format!("{NATIVE}/{id}{auth}"))
        .header("x-request-id", "request-123")
        .body(Body::empty())
        .unwrap();
    let response = app.clone().oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    body_of(response).await
}

fn native_entry_titles(body: &Value) -> Vec<String> {
    body["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry["track"]["title"].as_str().unwrap().to_owned())
        .collect()
}

fn native_entry_ids(body: &Value) -> Vec<String> {
    body["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry["entry_id"].as_str().unwrap().to_owned())
        .collect()
}

async fn track(ports: &StorePorts, external_id: &str, title: &str) -> TrackId {
    track_of_duration(ports, external_id, title, 1_000).await
}

async fn track_of_duration(
    ports: &StorePorts,
    external_id: &str,
    title: &str,
    duration_ms: u64,
) -> TrackId {
    ports
        .catalogue()
        .get_or_create(
            source(external_id),
            TrackMetadata::from_names(
                title.to_owned(),
                vec!["Artist".to_owned()],
                Some(duration_ms),
            )
            .unwrap(),
        )
        .await
        .unwrap()
        .id()
        .clone()
}

async fn sorted_list(
    app: &Router,
    token: &str,
    sort: &str,
    order: &str,
    start: u32,
    end: u32,
) -> (HeaderMap, Value) {
    let response = send(
        app,
        "GET",
        &format!("{API}?_start={start}&_end={end}&_sort={sort}&_order={order}"),
        token,
        None,
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let headers = response.headers().clone();

    (headers, body_of(response).await)
}

fn ids_of(playlists: &Value) -> Vec<String> {
    playlists
        .as_array()
        .unwrap()
        .iter()
        .map(|playlist| playlist["id"].as_str().unwrap().to_owned())
        .collect()
}

async fn four_tracks(ports: &StorePorts) -> Vec<TrackId> {
    let mut tracks = Vec::new();
    for (index, title) in ["First", "Second", "Third", "Fourth"].iter().enumerate() {
        tracks.push(track(ports, &format!("path-{index}"), title).await);
    }

    tracks
}

#[sqlx::test]
async fn navidrome_playlist_list_and_detail_report_the_real_owner(pool: PgPool) {
    let fixture = fixture(pool).await;
    let first = track(&fixture.ports, "path-a", "First").await;
    let private = create(
        &fixture.app,
        &fixture.owner,
        json!({ "name": "Mine", "comment": "private" }),
    )
    .await;
    let shared = create(
        &fixture.app,
        &fixture.owner,
        json!({ "name": "Shared", "comment": "public", "public": true }),
    )
    .await;
    let response = add(&fixture.app, &fixture.owner, &shared, &[&first]).await;
    assert_eq!(response.status(), StatusCode::OK);

    let (headers, body) = list(&fixture.app, &fixture.stranger).await;
    assert_eq!(headers["x-total-count"], "1");
    assert_eq!(body.as_array().unwrap().len(), 1);
    assert_eq!(body[0]["id"], json!(shared));
    assert_eq!(
        body[0]["ownerId"],
        json!(fixture.owner_id),
        "the list reports the playlist's owner, not the caller"
    );
    assert_ne!(body[0]["ownerId"], json!(fixture.stranger_id));
    assert_eq!(body[0]["ownerName"], json!("flynn"));
    assert_eq!(body[0]["songs"][0]["title"], json!("First"));

    let (headers, body) = list(&fixture.app, &fixture.owner).await;
    assert_eq!(headers["x-total-count"], "2");
    assert_eq!(body.as_array().unwrap().len(), 2);

    let response = send(
        &fixture.app,
        "GET",
        &format!("{API}?_start=1&_end=2&_sort=name&_order=ASC"),
        &fixture.owner,
        None,
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers()["x-total-count"],
        "2",
        "the header carries the matching total, not the page length"
    );
    let page = body_of(response).await;
    assert_eq!(page.as_array().unwrap().len(), 1, "the window is honoured");
    assert_eq!(page[0]["name"], json!("Shared"));

    let response = send(
        &fixture.app,
        "GET",
        &format!("{API}?q=Mine"),
        &fixture.owner,
        None,
    )
    .await;
    assert_eq!(
        response.status(),
        StatusCode::UNPROCESSABLE_ENTITY,
        "an unsupported playlist search is refused rather than ignored"
    );

    let seen = detail(&fixture.app, &fixture.stranger, &shared).await;
    assert_eq!(seen["ownerId"], json!(fixture.owner_id));
    assert_eq!(seen["name"], json!("Shared"));
    assert_eq!(seen["songs"].as_array().unwrap().len(), 1);

    let response = send(
        &fixture.app,
        "GET",
        &format!("{API}/{private}"),
        &fixture.stranger,
        None,
    )
    .await;
    assert_eq!(
        response.status(),
        StatusCode::NOT_FOUND,
        "a non-owner cannot read a private playlist"
    );

    let response = update(
        &fixture.app,
        &fixture.stranger,
        &shared,
        json!({ "name": "Hijacked" }),
    )
    .await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    assert_eq!(body_of(response).await, json!({ "error": "Forbidden." }));

    let response = add(&fixture.app, &fixture.stranger, &shared, &[&first]).await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);

    let response = remove(&fixture.app, &fixture.stranger, &shared, &["1"]).await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);

    let response = move_track(&fixture.app, &fixture.stranger, &shared, "1", "1").await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);

    let response = send(
        &fixture.app,
        "DELETE",
        &format!("{API}/{shared}"),
        &fixture.stranger,
        None,
    )
    .await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);

    let response = update(
        &fixture.app,
        &fixture.stranger,
        &private,
        json!({ "name": "Hijacked" }),
    )
    .await;
    assert_eq!(
        response.status(),
        StatusCode::NOT_FOUND,
        "an invisible playlist is refused as absent, not as forbidden"
    );

    let kept = detail(&fixture.app, &fixture.owner, &shared).await;
    assert_eq!(
        kept["name"],
        json!("Shared"),
        "the refused edits changed nothing"
    );
    assert_eq!(kept["ownerName"], json!("flynn"));

    let owned = detail(&fixture.app, &fixture.owner, &private).await;
    assert_eq!(owned["name"], json!("Mine"));
    assert_eq!(owned["ownerId"], json!(fixture.owner_id));
    assert_eq!(owned["comment"], json!("private"));

    let response = update(
        &fixture.app,
        &fixture.owner,
        &private,
        json!({ "name": "Renamed" }),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        detail(&fixture.app, &fixture.owner, &private).await["name"],
        json!("Renamed")
    );

    let response = update(
        &fixture.app,
        &fixture.owner,
        &shared,
        json!({ "comment": "edited" }),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        detail(&fixture.app, &fixture.owner, &shared).await["comment"],
        json!("edited")
    );
}

#[sqlx::test]
async fn navidrome_playlist_create_replaces_and_preserves_metadata(pool: PgPool) {
    let fixture = fixture(pool).await;
    let tracks = four_tracks(&fixture.ports).await;
    let id = create(
        &fixture.app,
        &fixture.owner,
        json!({ "name": "Trip", "comment": "original" }),
    )
    .await;
    let response = add(
        &fixture.app,
        &fixture.owner,
        &id,
        &[&tracks[0], &tracks[1], &tracks[2]],
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let response = update(&fixture.app, &fixture.owner, &id, json!({ "public": true })).await;
    assert_eq!(response.status(), StatusCode::OK);

    let replaced = create(
        &fixture.app,
        &fixture.owner,
        json!({
            "id": id,
            "name": "Replaced",
            "comment": "overwritten",
            "public": false,
            "ids": [tracks[3].to_string()],
        }),
    )
    .await;
    assert_eq!(replaced, id);

    let body = detail(&fixture.app, &fixture.owner, &id).await;
    assert_eq!(body["name"], json!("Trip"), "the name survives a replace");
    assert_eq!(
        body["comment"],
        json!("original"),
        "the comment survives a replace"
    );
    assert_eq!(body["public"], json!(true), "visibility survives a replace");
    assert_eq!(
        titles(&body["songs"]),
        vec!["Fourth"],
        "the old entries are gone"
    );
    assert_eq!(body["songCount"], json!(1));
    assert_eq!(body["id"], json!(id), "the replace keeps the playlist id");

    let (_, songs) = read_tracks(&fixture.app, &fixture.owner, &id).await;
    assert_eq!(titles(&songs), vec!["Fourth"]);

    let fresh = create(
        &fixture.app,
        &fixture.owner,
        json!({ "name": "Fresh", "comment": "brand new" }),
    )
    .await;
    assert_ne!(fresh, id);
    let body = detail(&fixture.app, &fixture.owner, &fresh).await;
    assert_eq!(body["name"], json!("Fresh"));
    assert_eq!(body["comment"], json!("brand new"));
    assert_eq!(body["ownerId"], json!(fixture.owner_id));
    assert_eq!(body["songCount"], json!(0));

    let response = send(
        &fixture.app,
        "POST",
        API,
        &fixture.stranger,
        Some(&json!({ "id": id, "ids": [] })),
    )
    .await;
    assert_eq!(
        response.status(),
        StatusCode::FORBIDDEN,
        "a non-owner cannot replace through create"
    );
    assert_eq!(
        titles(&detail(&fixture.app, &fixture.owner, &id).await["songs"]),
        vec!["Fourth"]
    );
}

#[sqlx::test]
async fn navidrome_playlist_repeated_tracks_are_distinct_occurrences(pool: PgPool) {
    let fixture = fixture(pool).await;
    let tracks = four_tracks(&fixture.ports).await;
    let id = create(&fixture.app, &fixture.owner, json!({ "name": "Repeats" })).await;

    let response = add(&fixture.app, &fixture.owner, &id, &[&tracks[0], &tracks[0]]).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(body_of(response).await, json!({ "added": 2 }));

    let (headers, songs) = read_tracks(&fixture.app, &fixture.owner, &id).await;
    assert_eq!(headers["x-total-count"], "2");
    assert_eq!(titles(&songs), vec!["First", "First"]);
    assert_eq!(
        media_ids(&songs),
        vec![tracks[0].to_string(), tracks[0].to_string()],
        "both occurrences name the same track"
    );
    assert_eq!(
        item_ids(&songs),
        vec!["1", "2"],
        "occurrences are addressed by their positional item id"
    );

    let native = native_detail(&fixture.app, &fixture.owner_auth, &id).await;
    assert_eq!(native["entry_count"], json!(2));
    let entries = native_entry_ids(&native);
    assert_ne!(
        entries[0], entries[1],
        "the two occurrences have distinct native entry ids"
    );

    let response = remove(&fixture.app, &fixture.owner, &id, &["1"]).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(body_of(response).await, json!({ "ids": ["1"] }));

    let (_, songs) = read_tracks(&fixture.app, &fixture.owner, &id).await;
    assert_eq!(
        titles(&songs),
        vec!["First"],
        "removing one positional id leaves the other occurrence"
    );
    assert_eq!(item_ids(&songs), vec!["1"]);

    let native = native_detail(&fixture.app, &fixture.owner_auth, &id).await;
    assert_eq!(native["entry_count"], json!(1));
    assert_eq!(
        native_entry_ids(&native),
        vec![entries[1].clone()],
        "the surviving occurrence is the one whose item id was not removed"
    );
}

#[sqlx::test]
async fn navidrome_playlist_move_uses_the_reference_base(pool: PgPool) {
    let fixture = fixture(pool).await;
    let tracks = four_tracks(&fixture.ports).await;
    let id = create(&fixture.app, &fixture.owner, json!({ "name": "Order" })).await;
    let response = add(
        &fixture.app,
        &fixture.owner,
        &id,
        &[&tracks[0], &tracks[1], &tracks[2], &tracks[3]],
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let started = native_entry_ids(&native_detail(&fixture.app, &fixture.owner_auth, &id).await);
    let (_, songs) = read_tracks(&fixture.app, &fixture.owner, &id).await;
    assert_eq!(item_ids(&songs), vec!["1", "2", "3", "4"]);
    assert_eq!(titles(&songs), vec!["First", "Second", "Third", "Fourth"]);

    let response = move_track(&fixture.app, &fixture.owner, &id, "3", "1").await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(body_of(response).await, json!({ "id": "3" }));

    let (_, songs) = read_tracks(&fixture.app, &fixture.owner, &id).await;
    assert_eq!(
        titles(&songs),
        vec!["Third", "First", "Second", "Fourth"],
        "the third item at insert_before 1 lands first"
    );
    assert_eq!(
        item_ids(&songs),
        vec!["1", "2", "3", "4"],
        "positional item ids follow the new order"
    );
    assert_eq!(
        media_ids(&songs)[0],
        tracks[2].to_string(),
        "the entry that moved is the one the client named"
    );

    let response = move_track(&fixture.app, &fixture.owner, &id, "2", "3").await;
    assert_eq!(response.status(), StatusCode::OK);

    let (_, songs) = read_tracks(&fixture.app, &fixture.owner, &id).await;
    assert_eq!(
        titles(&songs),
        vec!["Third", "Second", "First", "Fourth"],
        "an insert_before below the source lands the entry at that position"
    );

    let native = native_detail(&fixture.app, &fixture.owner_auth, &id).await;
    assert_eq!(
        native_entry_titles(&native),
        vec!["Third", "Second", "First", "Fourth"]
    );
    assert_eq!(
        native_entry_ids(&native),
        vec![
            started[2].clone(),
            started[1].clone(),
            started[0].clone(),
            started[3].clone()
        ],
        "the native entry ids do not change on reorder"
    );
    assert_eq!(
        item_ids(&songs),
        vec!["1", "2", "3", "4"],
        "the positional ids do change on reorder"
    );
}

#[sqlx::test]
async fn navidrome_playlist_update_applies_only_present_fields(pool: PgPool) {
    let fixture = fixture(pool).await;
    let id = create(
        &fixture.app,
        &fixture.owner,
        json!({ "name": "Trip", "comment": "back" }),
    )
    .await;
    let response = update(&fixture.app, &fixture.owner, &id, json!({ "public": true })).await;
    assert_eq!(response.status(), StatusCode::OK);

    let response = update(
        &fixture.app,
        &fixture.owner,
        &id,
        json!({ "name": "Renamed" }),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = detail(&fixture.app, &fixture.owner, &id).await;
    assert_eq!(body["name"], json!("Renamed"));
    assert_eq!(
        body["comment"],
        json!("back"),
        "an omitted comment survives"
    );
    assert_eq!(body["public"], json!(true), "an omitted flag survives");

    let response = update(&fixture.app, &fixture.owner, &id, json!({ "comment": "" })).await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = detail(&fixture.app, &fixture.owner, &id).await;
    assert_eq!(body["comment"], Value::Null, "an empty comment clears it");
    assert_eq!(body["name"], json!("Renamed"), "the name still survives");
    assert_eq!(body["public"], json!(true));

    let response = update(
        &fixture.app,
        &fixture.owner,
        &id,
        json!({ "public": false }),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = detail(&fixture.app, &fixture.owner, &id).await;
    assert_eq!(body["public"], json!(false), "a present flag applies");
    assert_eq!(body["name"], json!("Renamed"));
    assert_eq!(body["comment"], Value::Null);

    let response = update(&fixture.app, &fixture.owner, &id, json!({})).await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = detail(&fixture.app, &fixture.owner, &id).await;
    assert_eq!(body["name"], json!("Renamed"));
    assert_eq!(body["public"], json!(false));

    let response = update(
        &fixture.app,
        &fixture.owner,
        &id,
        json!({ "rules": { "limit": 10 } }),
    )
    .await;
    assert_eq!(
        response.status(),
        StatusCode::UNPROCESSABLE_ENTITY,
        "a smart playlist request is refused rather than silently made static"
    );

    let response = update(
        &fixture.app,
        &fixture.owner,
        &id,
        json!({ "ownerId": fixture.stranger_id }),
    )
    .await;
    assert_eq!(
        response.status(),
        StatusCode::FORBIDDEN,
        "an ordinary client cannot select another owner"
    );

    let response = update(&fixture.app, &fixture.owner, &id, json!({ "name": "" })).await;
    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(
        detail(&fixture.app, &fixture.owner, &id).await["name"],
        json!("Renamed"),
        "a rejected update changes nothing"
    );
}

#[sqlx::test]
async fn navidrome_playlist_stale_position_is_a_client_error(pool: PgPool) {
    let fixture = fixture(pool).await;
    let tracks = four_tracks(&fixture.ports).await;
    let id = create(&fixture.app, &fixture.owner, json!({ "name": "Stale" })).await;
    let response = add(&fixture.app, &fixture.owner, &id, &[&tracks[0], &tracks[1]]).await;
    assert_eq!(response.status(), StatusCode::OK);
    let before = read_tracks(&fixture.app, &fixture.owner, &id).await.1;

    let response = remove(&fixture.app, &fixture.owner, &id, &["9"]).await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    assert_eq!(body_of(response).await, json!({ "error": "Not found." }));
    let after = read_tracks(&fixture.app, &fixture.owner, &id).await.1;
    assert_eq!(
        item_ids(&after),
        item_ids(&before),
        "a stale removal id edits nothing"
    );
    assert_eq!(titles(&after), titles(&before));

    let response = move_track(&fixture.app, &fixture.owner, &id, "9", "1").await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let after = read_tracks(&fixture.app, &fixture.owner, &id).await.1;
    assert_eq!(
        titles(&after),
        titles(&before),
        "a stale move coordinate edits nothing"
    );

    let response = move_track(&fixture.app, &fixture.owner, &id, "0", "1").await;
    assert_eq!(
        response.status(),
        StatusCode::UNPROCESSABLE_ENTITY,
        "the wire coordinate is one-based, so item id zero is invalid"
    );

    let response = remove(&fixture.app, &fixture.owner, &id, &["0"]).await;
    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);

    let response = move_track(&fixture.app, &fixture.owner, &id, "1", "not-a-number").await;
    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let after = read_tracks(&fixture.app, &fixture.owner, &id).await.1;
    assert_eq!(titles(&after), titles(&before));
}

#[sqlx::test]
async fn navidrome_playlist_track_window_is_end_exclusive(pool: PgPool) {
    let fixture = fixture(pool).await;
    let tracks = four_tracks(&fixture.ports).await;
    let id = create(&fixture.app, &fixture.owner, json!({ "name": "Window" })).await;
    let response = add(
        &fixture.app,
        &fixture.owner,
        &id,
        &[&tracks[0], &tracks[1], &tracks[2], &tracks[3]],
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);

    let (headers, songs) = read_tracks(&fixture.app, &fixture.owner, &id).await;
    assert_eq!(titles(&songs).len(), 4);
    assert_eq!(headers["x-total-count"], "4");

    let response = send(
        &fixture.app,
        "GET",
        &format!("{API}/{id}/tracks?_start=1&_end=3&_sort=id&_order=ASC"),
        &fixture.owner,
        None,
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["x-total-count"], "4");
    let page = body_of(response).await;
    assert_eq!(
        titles(&page),
        vec!["Second", "Third"],
        "a bounded window returns exactly end - start rows"
    );

    let response = send(
        &fixture.app,
        "GET",
        &format!("{API}/{id}/tracks?_start=0&_end=0&_sort=id&_order=ASC"),
        &fixture.owner,
        None,
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let page = body_of(response).await;
    assert_eq!(
        titles(&page),
        vec!["First", "Second", "Third", "Fourth"],
        "an end at or before the start means the whole collection"
    );

    let response = send(
        &fixture.app,
        "GET",
        &format!("{API}/{id}/tracks?_start=2&_end=2&_sort=id&_order=ASC"),
        &fixture.owner,
        None,
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let page = body_of(response).await;
    assert_eq!(
        titles(&page),
        vec!["Third", "Fourth"],
        "the start still offsets a collection with no end bound"
    );

    let response = send(
        &fixture.app,
        "GET",
        &format!("{API}/{id}/tracks?_start=0&_end=-1&_sort=id&_order=ASC"),
        &fixture.owner,
        None,
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        titles(&body_of(response).await),
        vec!["First", "Second", "Third", "Fourth"],
        "the client's -1 still means the whole collection"
    );

    let response = send(
        &fixture.app,
        "GET",
        &format!("{API}/{id}/tracks?_end=two"),
        &fixture.owner,
        None,
    )
    .await;
    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
}

#[sqlx::test]
async fn existing_navidrome_cases_still_pass(pool: PgPool) {
    let fixture = fixture(pool).await;
    let first = track(&fixture.ports, "path-a", "First").await;
    let id = create(&fixture.app, &fixture.owner, json!({ "name": "Coexist" })).await;
    let response = add(&fixture.app, &fixture.owner, &id, &[&first]).await;
    assert_eq!(response.status(), StatusCode::OK);

    for (path, count) in [
        ("/compat/navidrome/api/song", "1"),
        ("/compat/navidrome/api/album", "0"),
        ("/compat/navidrome/api/artist", "1"),
        ("/compat/navidrome/api/playlist", "1"),
    ] {
        let response = send(&fixture.app, "GET", path, &fixture.owner, None).await;
        assert_eq!(response.status(), StatusCode::OK, "path: {path}");
        assert_eq!(response.headers()["x-total-count"], count, "path: {path}");
    }

    let response = send(
        &fixture.app,
        "GET",
        &format!("/compat/navidrome/api/song/{first}"),
        &fixture.owner,
        None,
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(body_of(response).await["title"], json!("First"));
}

#[sqlx::test]
async fn playlist_list_accepts_the_clients_default_sort_and_order(pool: PgPool) {
    let fixture = fixture(pool).await;
    let bravo = create(&fixture.app, &fixture.owner, json!({ "name": "Bravo" })).await;
    let alpha = create(&fixture.app, &fixture.owner, json!({ "name": "Alpha" })).await;
    let charlie = create(&fixture.app, &fixture.owner, json!({ "name": "Charlie" })).await;

    let (headers, body) =
        sorted_list(&fixture.app, &fixture.owner, "updatedAt", "DESC", 0, 20).await;
    assert_eq!(headers["x-total-count"], "3");
    assert_eq!(
        ids_of(&body),
        vec![charlie.clone(), alpha.clone(), bravo.clone()],
        "the client's updatedAt DESC returns the most recently changed playlist first"
    );

    let (_, by_name) = sorted_list(&fixture.app, &fixture.owner, "name", "ASC", 0, 20).await;
    assert_eq!(
        ids_of(&by_name),
        vec![alpha, bravo, charlie],
        "the timestamp order differs from the name order"
    );
}

#[sqlx::test]
async fn playlist_sorts_are_applied_before_the_window(pool: PgPool) {
    let fixture = fixture(pool).await;
    let mut created = Vec::new();
    for name in ["Echo", "Delta", "Charlie", "Bravo", "Alpha"] {
        created.push(create(&fixture.app, &fixture.owner, json!({ "name": name })).await);
    }
    let mut by_uuid = created.clone();
    by_uuid.sort();

    let (headers, first) =
        sorted_list(&fixture.app, &fixture.owner, "songCount", "ASC", 0, 3).await;
    assert_eq!(headers["x-total-count"], "5");
    assert_eq!(
        ids_of(&first),
        by_uuid[..3].to_vec(),
        "equal track counts fall back to ascending uuid, sorted before the window"
    );

    let (_, second) = sorted_list(&fixture.app, &fixture.owner, "songCount", "ASC", 3, 5).await;
    assert_eq!(ids_of(&second), by_uuid[3..].to_vec());

    let (_, descending) =
        sorted_list(&fixture.app, &fixture.owner, "songCount", "DESC", 0, 3).await;
    assert_eq!(
        ids_of(&descending),
        by_uuid[..3].to_vec(),
        "the uuid tie stays ascending in both directions"
    );
}

#[sqlx::test]
async fn playlist_list_supports_all_consumed_sort_fields(pool: PgPool) {
    let fixture = fixture(pool).await;
    let charlie = create(
        &fixture.app,
        &fixture.owner,
        json!({ "name": "Charlie", "public": true }),
    )
    .await;
    let bravo = create(
        &fixture.app,
        &fixture.owner,
        json!({ "name": "Bravo", "public": false }),
    )
    .await;
    let alpha = create(
        &fixture.app,
        &fixture.stranger,
        json!({ "name": "alpha", "public": true }),
    )
    .await;
    let hidden = create(
        &fixture.app,
        &fixture.stranger,
        json!({ "name": "aaa", "public": false }),
    )
    .await;

    let charlie_track = track_of_duration(&fixture.ports, "charlie", "Charlie song", 900).await;
    let bravo_first = track_of_duration(&fixture.ports, "bravo-1", "Bravo one", 4_500).await;
    let bravo_second = track_of_duration(&fixture.ports, "bravo-2", "Bravo two", 4_500).await;
    let alpha_track = track_of_duration(&fixture.ports, "alpha", "alpha song", 10).await;
    let hidden_track = track_of_duration(&fixture.ports, "hidden", "hidden song", 1).await;

    for (token, playlist, tracks) in [
        (&fixture.owner, &bravo, vec![&bravo_first, &bravo_second]),
        (&fixture.stranger, &alpha, vec![&alpha_track]),
        (&fixture.owner, &charlie, vec![&charlie_track]),
        (&fixture.stranger, &hidden, vec![&hidden_track]),
    ] {
        assert_eq!(
            add(&fixture.app, token, playlist, &tracks).await.status(),
            StatusCode::OK
        );
    }

    let mut owned_by_uuid = [charlie.clone(), bravo.clone()];
    owned_by_uuid.sort();
    let mut one_track_by_uuid = [charlie.clone(), alpha.clone()];
    one_track_by_uuid.sort();

    let expected_ascending = [
        ("name", vec![alpha.clone(), bravo.clone(), charlie.clone()]),
        (
            "updatedAt",
            vec![bravo.clone(), alpha.clone(), charlie.clone()],
        ),
        (
            "duration",
            vec![alpha.clone(), charlie.clone(), bravo.clone()],
        ),
        (
            "owner_name",
            vec![
                owned_by_uuid[0].clone(),
                owned_by_uuid[1].clone(),
                alpha.clone(),
            ],
        ),
        (
            "public",
            vec![
                bravo.clone(),
                one_track_by_uuid[0].clone(),
                one_track_by_uuid[1].clone(),
            ],
        ),
        (
            "songCount",
            vec![
                one_track_by_uuid[0].clone(),
                one_track_by_uuid[1].clone(),
                bravo.clone(),
            ],
        ),
    ];
    let expected_descending = [
        ("name", vec![charlie.clone(), bravo.clone(), alpha.clone()]),
        (
            "updatedAt",
            vec![charlie.clone(), alpha.clone(), bravo.clone()],
        ),
        (
            "duration",
            vec![bravo.clone(), charlie.clone(), alpha.clone()],
        ),
        (
            "owner_name",
            vec![
                alpha.clone(),
                owned_by_uuid[0].clone(),
                owned_by_uuid[1].clone(),
            ],
        ),
        (
            "public",
            vec![
                one_track_by_uuid[0].clone(),
                one_track_by_uuid[1].clone(),
                bravo.clone(),
            ],
        ),
        (
            "songCount",
            vec![
                bravo.clone(),
                one_track_by_uuid[0].clone(),
                one_track_by_uuid[1].clone(),
            ],
        ),
    ];

    for (sort, expected) in expected_ascending {
        let (headers, body) = sorted_list(&fixture.app, &fixture.owner, sort, "ASC", 0, 50).await;
        assert_eq!(headers["x-total-count"], "3", "sort {sort}");
        assert_eq!(ids_of(&body), expected, "sort {sort} ASC");
    }
    for (sort, expected) in expected_descending {
        let (headers, body) = sorted_list(&fixture.app, &fixture.owner, sort, "DESC", 0, 50).await;
        assert_eq!(headers["x-total-count"], "3", "sort {sort}");
        assert_eq!(ids_of(&body), expected, "sort {sort} DESC");
    }

    let (_, by_count) = sorted_list(&fixture.app, &fixture.owner, "songCount", "ASC", 0, 50).await;
    let counts = by_count
        .as_array()
        .unwrap()
        .iter()
        .map(|playlist| playlist["songCount"].as_u64().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(counts, vec![1, 1, 2], "the reported counts are real");
    assert!(
        !ids_of(&by_count).contains(&hidden),
        "an invisible private playlist is excluded"
    );
}

#[sqlx::test]
async fn an_unsupported_playlist_sort_is_refused_not_defaulted(pool: PgPool) {
    let fixture = fixture(pool).await;
    let id = create(&fixture.app, &fixture.owner, json!({ "name": "Sorted" })).await;

    for query in [
        "_sort=bogus",
        "_sort=updated_at",
        "_order=SIDEWAYS",
        "_sort=name&_order=descending",
        "smart=true",
    ] {
        let response = send(
            &fixture.app,
            "GET",
            &format!("{API}?{query}"),
            &fixture.owner,
            None,
        )
        .await;
        assert_eq!(
            response.status(),
            StatusCode::UNPROCESSABLE_ENTITY,
            "query {query}"
        );
    }

    let (headers, body) = list(&fixture.app, &fixture.owner).await;
    assert_eq!(headers["x-total-count"], "1");
    assert_eq!(ids_of(&body), vec![id]);
}
