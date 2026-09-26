#![cfg(feature = "postgres-integration")]

mod support;

use std::{collections::HashSet, path::PathBuf, sync::Arc};

use axum::{
    Router,
    body::{Body, to_bytes},
    http::{HeaderMap, Request, StatusCode},
};
use base64::Engine;
use base64::engine::general_purpose::STANDARD_NO_PAD;
use serde_json::{Value, json};
use sqlx::PgPool;
use tower::ServiceExt;
use uuid::Uuid;

use whio_core::{
    app::AppState,
    features::identity::{provision, secrets::CredentialKey},
    features::music_library::{
        domain::{
            AlbumId, AlbumMetadata, AlbumTrack, ArtistCredit, ProviderId, SourceIdentity,
            SourceScope, StateTarget, TrackId, TrackMetadata,
        },
        library::{Library, LibraryRegistration, LibraryRegistry},
        test_support::{CatalogueWrite, PostgresLibraryStore, PostgresTrackRepository, StateStore},
    },
};

use support::{credentials::database_credential_service, services};

const MUSIC_ROOT: &str = "/music";
const ELSEWHERE_ROOT: &str = "/elsewhere";

struct Principal {
    auth: String,
    token: String,
    id: Uuid,
}

struct Fixture {
    app: Router,
    store: Arc<PostgresTrackRepository>,
    alice: Principal,
    bob: Principal,
    music: Library,
    elsewhere: Library,
    kept: AlbumId,
    other: AlbumId,
    alpha: TrackId,
    beta: TrackId,
}

fn credential_key() -> CredentialKey {
    CredentialKey::from_base64(&STANDARD_NO_PAD.encode([11u8; 32])).expect("valid test key")
}

fn local_source(root: &str, external_id: &str) -> SourceIdentity {
    SourceIdentity::new(
        ProviderId::new("local".to_owned()).unwrap(),
        SourceScope::Named(root.to_owned()),
        external_id.to_owned(),
    )
    .unwrap()
}

async fn add_album(
    store: &PostgresTrackRepository,
    title: &str,
    root: &str,
    external_id: &str,
) -> AlbumId {
    let album = store
        .resolve_or_create_album(
            &AlbumMetadata::new(title.to_owned())
                .unwrap()
                .with_album_artist(ArtistCredit::new("Album Artist".to_owned(), None).unwrap()),
        )
        .await
        .unwrap();
    store
        .set_album_source(album.id(), &local_source(root, external_id))
        .await
        .unwrap();

    album.id().clone()
}

async fn add_track(
    store: &PostgresTrackRepository,
    root: &str,
    external_id: &str,
    title: &str,
) -> TrackId {
    store
        .get_or_create(
            local_source(root, external_id),
            TrackMetadata::from_names(title.to_owned(), vec!["Artist".to_owned()], Some(1_000))
                .unwrap(),
        )
        .await
        .unwrap()
        .id()
        .clone()
}

async fn send(app: &Router, request: Request<Body>) -> (StatusCode, HeaderMap, Value) {
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = to_bytes(response.into_body(), 8 * 1024 * 1024)
        .await
        .unwrap();

    (status, headers, serde_json::from_slice(&bytes).unwrap())
}

async fn get(app: &Router, uri: &str) -> (StatusCode, HeaderMap, Value) {
    let request = Request::builder()
        .method("GET")
        .uri(uri)
        .header("x-request-id", "request-123")
        .body(Body::empty())
        .unwrap();

    send(app, request).await
}

async fn native_page(
    app: &Router,
    auth: &str,
    resource: &str,
    query: &str,
) -> (StatusCode, HeaderMap, Value) {
    get(app, &format!("/api/v1/library/{resource}?{auth}&{query}")).await
}

async fn navidrome(app: &Router, token: &str, path: &str) -> (StatusCode, HeaderMap, Value) {
    let request = Request::builder()
        .method("GET")
        .uri(format!("/compat/navidrome{path}"))
        .header("x-request-id", "request-123")
        .header("x-nd-authorization", format!("Bearer {token}"))
        .body(Body::empty())
        .unwrap();

    send(app, request).await
}

async fn rest(app: &Router, auth: &str, path: &str, query: &str) -> (StatusCode, HeaderMap, Value) {
    get(app, &format!("/rest{path}?{auth}&{query}")).await
}

async fn login(app: &Router, username: &str, password: &str) -> (String, Uuid) {
    let request = Request::builder()
        .method("POST")
        .uri("/compat/navidrome/auth/login")
        .header("content-type", "application/json")
        .header("x-request-id", "request-123")
        .body(Body::from(
            json!({"username": username, "password": password}).to_string(),
        ))
        .unwrap();
    let (status, _, body) = send(app, request).await;

    assert_eq!(status, StatusCode::OK, "login for {username}: {body}");

    (
        body["token"].as_str().unwrap().to_owned(),
        body["id"].as_str().unwrap().parse().unwrap(),
    )
}

fn query_auth(username: &str, secret: &str) -> String {
    format!("u={username}&v=1.16.1&c=integration-test&p={secret}")
}

fn total(headers: &HeaderMap) -> u32 {
    headers["x-total-count"].to_str().unwrap().parse().unwrap()
}

fn ids(items: &[Value]) -> HashSet<String> {
    items
        .iter()
        .map(|item| item["id"].as_str().unwrap().to_owned())
        .collect()
}

fn item_ids(body: &Value) -> HashSet<String> {
    ids(body["items"].as_array().unwrap())
}

fn album<'a>(body: &'a Value, id: &AlbumId) -> &'a Value {
    body.as_array()
        .unwrap()
        .iter()
        .find(|item| item["id"] == json!(id.to_string()))
        .unwrap_or_else(|| panic!("album {id} in the response"))
}

fn album_ids(body: &Value) -> HashSet<String> {
    ids(body["subsonic-response"]["albumList2"]["album"]
        .as_array()
        .unwrap())
}

async fn fixture(pool: PgPool) -> Fixture {
    let (credential, alice_secret) = database_credential_service(pool.clone()).await;
    let provisioning = provision(pool.clone());
    provisioning.create_user("bob", "Bob").await.unwrap();
    let (_, bob_secret) = provisioning
        .mint_app_password(&credential_key(), "bob", "integration")
        .await
        .unwrap();
    let (search, library, playback) = services(pool.clone(), Vec::new());
    let registry = Arc::new(LibraryRegistry::new(Arc::new(PostgresLibraryStore::new(
        pool.clone(),
    ))));
    let registered = registry
        .register(&[
            LibraryRegistration::new("music".to_owned(), vec![PathBuf::from(MUSIC_ROOT)]),
            LibraryRegistration::new("elsewhere".to_owned(), vec![PathBuf::from(ELSEWHERE_ROOT)]),
        ])
        .await
        .unwrap();
    let app = whio_core::app::router(
        AppState::new(credential, search, library, playback).with_library_registry(registry),
    );
    let store = Arc::new(PostgresTrackRepository::new(pool.clone()));
    let kept = add_album(&store, "Kept", MUSIC_ROOT, "kept.flac").await;
    let other = add_album(&store, "Other", ELSEWHERE_ROOT, "other.flac").await;
    let alpha = add_track(&store, MUSIC_ROOT, "alpha.flac", "Alpha").await;
    let beta = add_track(&store, ELSEWHERE_ROOT, "beta.flac", "Beta").await;
    store
        .set_album_track(&kept, &alpha, &AlbumTrack::unplaced())
        .await
        .unwrap();
    store
        .set_album_track(&other, &beta, &AlbumTrack::unplaced())
        .await
        .unwrap();
    let named = |label: &str| {
        registered
            .iter()
            .find(|library| library.label() == label)
            .unwrap()
            .clone()
    };
    let (alice_token, alice_id) = login(&app, "flynn", &alice_secret).await;
    let (bob_token, bob_id) = login(&app, "bob", &bob_secret).await;

    Fixture {
        app,
        store,
        alice: Principal {
            auth: query_auth("flynn", &alice_secret),
            token: alice_token,
            id: alice_id,
        },
        bob: Principal {
            auth: query_auth("bob", &bob_secret),
            token: bob_token,
            id: bob_id,
        },
        music: named("music"),
        elsewhere: named("elsewhere"),
        kept,
        other,
        alpha,
        beta,
    }
}

#[sqlx::test]
async fn native_and_navidrome_agree_on_a_filtered_set(pool: PgPool) {
    let fixture = fixture(pool).await;
    let mut matches = HashSet::new();

    for (title, external_id) in [
        ("Discovery", "discovery.flac"),
        ("Discovery II", "discovery-ii.flac"),
        ("discovery live", "discovery-live.flac"),
        ("Homework", "homework.flac"),
    ] {
        let album_id = add_album(&fixture.store, title, MUSIC_ROOT, external_id).await;
        let track = add_track(&fixture.store, MUSIC_ROOT, external_id, title).await;
        fixture
            .store
            .set_album_track(&album_id, &track, &AlbumTrack::unplaced())
            .await
            .unwrap();

        if title.to_lowercase().contains("discovery") {
            matches.insert(album_id.to_string());
        }
    }

    assert_eq!(matches.len(), 3, "three spellings match the selector");

    let native_query = "name=discovery&order=name&direction=desc";
    let facade_window = "/api/album?name=discovery&_sort=name&_order=desc&_start=0&_end=2";
    let (status, headers, native) = native_page(
        &fixture.app,
        &fixture.alice.auth,
        "albums",
        &format!("{native_query}&limit=2"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "native window: {native}");
    assert_eq!(
        native["total"],
        json!(3),
        "the total is the match count, not the window"
    );
    assert_eq!(total(&headers), 3);
    let native_window = item_ids(&native);
    assert_eq!(native_window.len(), 2);

    let (status, headers, facade) =
        navidrome(&fixture.app, &fixture.alice.token, facade_window).await;
    assert_eq!(status, StatusCode::OK, "facade window: {facade}");
    assert_eq!(
        total(&headers),
        3,
        "the facade total is the operation count, not the page length"
    );
    let facade_window_ids = ids(facade.as_array().unwrap());
    assert_eq!(facade_window_ids.len(), 2);
    assert_eq!(
        facade_window_ids, native_window,
        "the windowed entity sets agree across protocols"
    );

    let (_, _, native_all) = native_page(
        &fixture.app,
        &fixture.alice.auth,
        "albums",
        &format!("{native_query}&limit=20"),
    )
    .await;
    let (_, headers, facade_all) = navidrome(
        &fixture.app,
        &fixture.alice.token,
        "/api/album?name=discovery&_sort=name&_order=desc&_start=0&_end=20",
    )
    .await;
    assert_eq!(total(&headers), 3);
    assert_eq!(item_ids(&native_all), matches);
    assert_eq!(ids(facade_all.as_array().unwrap()), matches);
    assert!(native_window.is_subset(&matches));
}

#[sqlx::test]
async fn a_library_scoped_read_is_the_same_everywhere(pool: PgPool) {
    let fixture = fixture(pool).await;
    let music = fixture.music.id().to_string();
    let elsewhere = fixture.elsewhere.id().to_string();

    let (status, headers, native_tracks) = native_page(
        &fixture.app,
        &fixture.alice.auth,
        "tracks",
        &format!("library_id={music}"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "native tracks: {native_tracks}");
    assert_eq!(
        item_ids(&native_tracks),
        HashSet::from([fixture.alpha.to_string()])
    );
    assert_eq!(native_tracks["total"], json!(1));
    assert_eq!(total(&headers), 1);

    let (status, _, native_albums) = native_page(
        &fixture.app,
        &fixture.alice.auth,
        "albums",
        &format!("library_id={music}"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "native albums: {native_albums}");
    assert_eq!(
        item_ids(&native_albums),
        HashSet::from([fixture.kept.to_string()])
    );
    assert_eq!(native_albums["total"], json!(1));

    let (_, _, other_tracks) = native_page(
        &fixture.app,
        &fixture.alice.auth,
        "tracks",
        &format!("library_id={elsewhere}"),
    )
    .await;
    assert_eq!(
        item_ids(&other_tracks),
        HashSet::from([fixture.beta.to_string()]),
        "the other library's tracks are the other library's"
    );
    let (_, _, other_albums) = native_page(
        &fixture.app,
        &fixture.alice.auth,
        "albums",
        &format!("library_id={elsewhere}"),
    )
    .await;
    assert_eq!(
        item_ids(&other_albums),
        HashSet::from([fixture.other.to_string()])
    );

    let (status, headers, scoped) = navidrome(
        &fixture.app,
        &fixture.alice.token,
        &format!("/api/album?library_id={}", fixture.music.numeric_id()),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "facade albums: {scoped}");
    assert_eq!(total(&headers), 1);
    assert_eq!(
        ids(scoped.as_array().unwrap()),
        item_ids(&native_albums),
        "the facade and the native route scope to the same entity set"
    );

    let (_, headers, other_scoped) = navidrome(
        &fixture.app,
        &fixture.alice.token,
        &format!("/api/album?library_id={}", fixture.elsewhere.numeric_id()),
    )
    .await;
    assert_eq!(total(&headers), 1);
    assert_eq!(
        ids(other_scoped.as_array().unwrap()),
        item_ids(&other_albums)
    );

    let (status, _, unscoped) = rest(
        &fixture.app,
        &fixture.alice.auth,
        "/getAlbumList2.view",
        "type=alphabeticalByName",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "subsonic albums: {unscoped}");
    let both = HashSet::from([fixture.kept.to_string(), fixture.other.to_string()]);
    assert_eq!(
        album_ids(&unscoped),
        both,
        "an unscoped getAlbumList2 lists both libraries"
    );

    let (status, _, music_scoped) = rest(
        &fixture.app,
        &fixture.alice.auth,
        "/getAlbumList2.view",
        &format!(
            "type=alphabeticalByName&musicFolderId={}",
            fixture.music.numeric_id()
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "subsonic music: {music_scoped}");
    assert_eq!(
        album_ids(&music_scoped),
        HashSet::from([fixture.kept.to_string()]),
        "musicFolderId scopes getAlbumList2 to the named library"
    );

    let (status, _, elsewhere_scoped) = rest(
        &fixture.app,
        &fixture.alice.auth,
        "/getAlbumList2.view",
        &format!(
            "type=alphabeticalByName&musicFolderId={}",
            fixture.elsewhere.numeric_id()
        ),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "subsonic elsewhere: {elsewhere_scoped}"
    );
    assert_eq!(
        album_ids(&elsewhere_scoped),
        HashSet::from([fixture.other.to_string()])
    );

    let (status, _, unknown) = rest(
        &fixture.app,
        &fixture.alice.auth,
        "/getAlbumList2.view",
        "type=alphabeticalByName&musicFolderId=999",
    )
    .await;
    assert_eq!(
        status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "a folder id naming no library is a client error"
    );
    assert_eq!(
        unknown["subsonic-response"]["error"]["message"],
        json!("Invalid browse parameters.")
    );

    for body in [
        &native_tracks,
        &native_albums,
        &other_tracks,
        &other_albums,
        &scoped,
        &other_scoped,
        &unscoped,
    ] {
        let rendered = body.to_string();
        assert!(
            !rendered.contains(MUSIC_ROOT),
            "a response body exposes a fixture root: {rendered}"
        );
        assert!(!rendered.contains(ELSEWHERE_ROOT));
    }

    let unregistered = Uuid::now_v7();
    let (status, _, body) = native_page(
        &fixture.app,
        &fixture.alice.auth,
        "albums",
        &format!("library_id={unregistered}"),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "native unregistered: {body}"
    );
    assert_eq!(
        body,
        json!({
            "code": "invalid_request",
            "message": "The request is invalid",
            "request_id": "request-123"
        })
    );

    let (status, _, body) = navidrome(
        &fixture.app,
        &fixture.alice.token,
        "/api/album?library_id=999",
    )
    .await;
    assert_eq!(
        status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "facade unregistered: {body}"
    );
    assert_eq!(body, json!({"error": "Invalid search parameters."}));
}

#[sqlx::test]
async fn one_principals_state_is_invisible_to_another(pool: PgPool) {
    let fixture = fixture(pool).await;
    fixture
        .store
        .set_starred(fixture.alice.id, StateTarget::Album(&fixture.kept), true)
        .await
        .unwrap();
    fixture
        .store
        .set_rating(fixture.alice.id, StateTarget::Album(&fixture.kept), Some(4))
        .await
        .unwrap();

    let (status, _, native_alice) =
        native_page(&fixture.app, &fixture.alice.auth, "albums", "order=name").await;
    assert_eq!(status, StatusCode::OK, "alice native: {native_alice}");
    let owned = album(&native_alice["items"], &fixture.kept);
    assert_eq!(owned["starred"], json!(true));
    assert_eq!(owned["rating"], json!(4));

    let (_, _, native_bob) =
        native_page(&fixture.app, &fixture.bob.auth, "albums", "order=name").await;
    let borrowed = album(&native_bob["items"], &fixture.kept);
    assert_eq!(borrowed["starred"], json!(false));
    assert!(borrowed.get("rating").is_none());

    let (status, headers, facade_alice) = navidrome(
        &fixture.app,
        &fixture.alice.token,
        "/api/album?starred=true",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "alice facade: {facade_alice}");
    assert_eq!(total(&headers), 1);
    assert_eq!(
        ids(facade_alice.as_array().unwrap()),
        HashSet::from([fixture.kept.to_string()])
    );
    let starred = &facade_alice.as_array().unwrap()[0];
    assert_eq!(starred["starred"], json!(true));
    assert_eq!(starred["rating"], json!(4));

    let (status, headers, facade_bob) =
        navidrome(&fixture.app, &fixture.bob.token, "/api/album?starred=true").await;
    assert_eq!(status, StatusCode::OK, "bob facade: {facade_bob}");
    assert!(facade_bob.as_array().unwrap().is_empty());
    assert_eq!(total(&headers), 0);

    let (_, _, facade_bob_all) =
        navidrome(&fixture.app, &fixture.bob.token, "/api/album?_sort=name").await;
    let borrowed = album(&facade_bob_all, &fixture.kept);
    assert!(borrowed.get("starred").is_none());
    assert!(borrowed.get("rating").is_none());

    let scoped = format!("order=rating&library_id={}", fixture.music.id());
    let (status, headers, native_rated) =
        native_page(&fixture.app, &fixture.bob.auth, "albums", &scoped).await;
    assert_eq!(status, StatusCode::OK, "bob rating sort: {native_rated}");
    assert_eq!(
        item_ids(&native_rated),
        HashSet::from([fixture.kept.to_string()])
    );
    assert_eq!(native_rated["total"], json!(1));
    assert_eq!(total(&headers), 1);

    let (status, headers, facade_rated) = navidrome(
        &fixture.app,
        &fixture.bob.token,
        &format!(
            "/api/album?library_id={}&_sort=rating",
            fixture.music.numeric_id()
        ),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "bob facade rating sort: {facade_rated}"
    );
    assert_eq!(total(&headers), 1);
    assert_eq!(
        ids(facade_rated.as_array().unwrap()),
        HashSet::from([fixture.kept.to_string()])
    );
    assert!(
        facade_rated
            .as_array()
            .unwrap()
            .iter()
            .all(|album| album.get("rating").is_none())
    );
}

async fn seed_albums(store: &PostgresTrackRepository, count: u32) -> Vec<AlbumId> {
    let mut albums = Vec::new();

    for index in 0..count {
        let external_id = format!("deep-{index:04}.flac");
        let album = add_album(store, &format!("zz{index:04}"), MUSIC_ROOT, &external_id).await;
        let track = add_track(store, MUSIC_ROOT, &external_id, &format!("Deep {index:04}")).await;
        store
            .set_album_track(&album, &track, &AlbumTrack::unplaced())
            .await
            .unwrap();

        albums.push(album);
    }

    albums
}

#[sqlx::test]
async fn native_album_page_past_1000_agrees_with_the_count(pool: PgPool) {
    let fixture = fixture(pool).await;
    let seeded = seed_albums(&fixture.store, 1_000).await;
    let total_albums = 1_002;

    let (status, headers, deep) = native_page(
        &fixture.app,
        &fixture.alice.auth,
        "albums",
        "order=name&limit=500&offset=1000",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "native deep albums: {deep}");
    assert_eq!(
        total(&headers),
        total_albums,
        "the header is the count, not the page length"
    );
    assert_eq!(deep["total"], json!(total_albums));
    assert_eq!(
        item_ids(&deep),
        HashSet::from([seeded[998].to_string(), seeded[999].to_string()]),
        "the deep album page is the ordering's tail"
    );
}

#[sqlx::test]
async fn subsonic_album_window_past_1000_agrees_with_the_count(pool: PgPool) {
    let fixture = fixture(pool).await;
    let seeded = seed_albums(&fixture.store, 1_000).await;
    let total_albums = 1_002;

    let (status, headers, native_deep) = native_page(
        &fixture.app,
        &fixture.alice.auth,
        "albums",
        "order=name&limit=500&offset=1000",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "native deep albums: {native_deep}");
    assert_eq!(total(&headers), total_albums);

    let (status, _, deep) = rest(
        &fixture.app,
        &fixture.alice.auth,
        "/getAlbumList2.view",
        "type=alphabeticalByName&size=500&offset=1000",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "subsonic deep window: {deep}");
    let window = album_ids(&deep);
    assert_eq!(
        window.len() as u32,
        total_albums - 1_000,
        "the window is the remainder the count leaves past the offset"
    );
    assert_eq!(
        window,
        HashSet::from([seeded[998].to_string(), seeded[999].to_string()]),
        "the deep window is the ordering's tail"
    );
    assert_eq!(
        window,
        item_ids(&native_deep),
        "the subsonic window is the native page's slice"
    );
}
