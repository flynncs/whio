#![cfg(feature = "postgres-integration")]

use sqlx::PgPool;
use std::{collections::HashSet, path::PathBuf, sync::Arc};

mod support;

use support::{
    StorePorts, credential_service_with_known_api_key, credential_service_with_known_app_password,
    router,
};

use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use md5::{Digest, Md5};
use serde_json::{Value, json};
use tower::ServiceExt;
use whio_core::{
    app::AppState,
    features::identity::secrets::generate_app_secret,
    features::music_library::{
        domain::{
            AlbumId, AlbumMetadata, AlbumTrack, ArtistCredit, ProviderId, SourceIdentity,
            SourceScope, TrackId, TrackMetadata,
        },
        library::{Library, LibraryId, LibraryRegistration, LibraryRegistry},
        test_support::{
            CatalogueWrite, LibraryStore, PostgresLibraryStore, PostgresTrackRepository,
            TrackRepositoryError,
        },
    },
    features::playback::PlaybackProvider,
    features::playback::providers::YtmAdapter,
};

fn app(
    pool: PgPool,
    credential: Arc<whio_core::features::identity::service::CredentialService>,
) -> axum::Router {
    let ports = StorePorts::new(pool.clone());
    let (search, library) = ports.bare_catalogue();
    let playback = ports.streaming(vec![
        Arc::new(YtmAdapter::disabled()) as Arc<dyn PlaybackProvider>
    ]);

    router(credential, search, library, playback)
}

async fn get(app: axum::Router, uri: &str) -> Value {
    let request = Request::builder()
        .method("GET")
        .uri(uri)
        .header("x-request-id", "request-123")
        .body(Body::empty())
        .unwrap();

    let response = app.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let body = to_bytes(response.into_body(), 8192).await.unwrap();
    serde_json::from_slice(&body).unwrap()
}

fn failure(code: u16) -> Value {
    json!({
        "subsonic-response": {
            "status": "failed",
            "version": "1.16.1",
            "type": "whio",
            "serverVersion": "0.1.0",
            "openSubsonic": true,
            "error": {
                "code": code,
                "message": match code {
                    0 => "Unsupported operation.",
                    10 => "Required parameter is missing.",
                    40 => "Wrong username or password.",
                    43 => "Multiple conflicting authentication mechanisms provided.",
                    _ => unreachable!("no fixture message for this code"),
                }
            }
        }
    })
}

#[sqlx::test]
async fn ping_without_auth_params_reports_missing_parameter(pool: PgPool) {
    let response = get(
        app(pool.clone(), support::credential_service()),
        "/rest/ping.view?v=1.16.1&c=test-client",
    )
    .await;

    assert_eq!(response, failure(10));
}

#[sqlx::test]
async fn ping_without_any_mechanism_reports_missing_parameter(pool: PgPool) {
    let response = get(
        app(pool.clone(), support::credential_service()),
        "/rest/ping.view?u=flynn&v=1.16.1&c=test-client",
    )
    .await;

    assert_eq!(response, failure(10));
}

#[sqlx::test]
async fn ping_with_half_a_token_pair_reports_missing_parameter(pool: PgPool) {
    let response = get(
        app(pool.clone(), support::credential_service()),
        "/rest/ping.view?u=flynn&v=1.16.1&c=test-client&t=abc123",
    )
    .await;

    assert_eq!(response, failure(10));
}

#[sqlx::test]
async fn ping_with_conflicting_mechanisms_reports_code_43(pool: PgPool) {
    let response = get(
        app(pool.clone(), support::credential_service()),
        "/rest/ping.view?u=flynn&v=1.16.1&c=test-client&p=whio_one&apiKey=whio_two",
    )
    .await;

    assert_eq!(response, failure(43));
}

#[sqlx::test]
async fn ping_with_unknown_api_key_is_indistinguishable_from_wrong_password(pool: PgPool) {
    let (service, _known) = credential_service_with_known_api_key();
    let unknown = generate_app_secret();
    let response = get(
        app(pool.clone(), service),
        &format!("/rest/ping.view?u=flynn&v=1.16.1&c=test-client&apiKey={unknown}"),
    )
    .await;

    assert_eq!(response, failure(40));
}

#[sqlx::test]
async fn ping_with_known_api_key_succeeds(pool: PgPool) {
    let (service, secret) = credential_service_with_known_api_key();
    let response = get(
        app(pool.clone(), service),
        &format!("/rest/ping.view?u=flynn&v=1.16.1&c=test-client&apiKey={secret}"),
    )
    .await;

    assert_eq!(
        response,
        json!({
            "subsonic-response": {
                "status": "ok",
                "version": "1.16.1",
                "type": "whio",
                "serverVersion": "0.1.0",
                "openSubsonic": true
            }
        })
    );
}

#[sqlx::test]
async fn ping_accepts_salted_token_of_a_stored_app_password(pool: PgPool) {
    let (service, secret) = credential_service_with_known_app_password();
    let salt = "abcdef123456";

    let mut hasher = Md5::new();
    hasher.update(secret.as_bytes());
    hasher.update(salt.as_bytes());
    let token: String = hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();

    let response = get(
        app(pool.clone(), service),
        &format!("/rest/ping.view?u=flynn&v=1.16.1&c=test-client&t={token}&s={salt}"),
    )
    .await;

    assert_eq!(
        response["subsonic-response"]["status"],
        json!("ok"),
        "full response: {response}"
    );
}

#[sqlx::test]
async fn ping_accepts_enc_prefixed_password(pool: PgPool) {
    let (service, secret) = credential_service_with_known_app_password();
    let hex_encoded: String = secret.bytes().map(|byte| format!("{byte:02x}")).collect();

    let response = get(
        app(pool.clone(), service),
        &format!("/rest/ping.view?u=flynn&v=1.16.1&c=test-client&p=enc:{hex_encoded}"),
    )
    .await;

    assert_eq!(response["subsonic-response"]["status"], json!("ok"));
}

#[sqlx::test]
async fn open_subsonic_extensions_requires_authentication(pool: PgPool) {
    let response = get(
        app(pool.clone(), support::credential_service()),
        "/rest/getOpenSubsonicExtensions.view",
    )
    .await;

    assert_eq!(response, failure(10));
}

#[sqlx::test]
async fn open_subsonic_extensions_lists_the_advertised_extensions(pool: PgPool) {
    let (service, secret) = credential_service_with_known_api_key();
    let response = get(
        app(pool.clone(), service),
        &format!(
            "/rest/getOpenSubsonicExtensions.view?u=flynn&v=1.16.1&c=test-client&apiKey={secret}"
        ),
    )
    .await;

    assert_eq!(
        response,
        json!({
            "subsonic-response": {
                "status": "ok",
                "version": "1.16.1",
                "type": "whio",
                "serverVersion": "0.1.0",
                "openSubsonic": true,
                "openSubsonicExtensions": [
                    {
                        "name": "indexBasedQueue",
                        "versions": [1]
                    }
                ]
            }
        })
    );
}

#[sqlx::test]
async fn unknown_rest_endpoints_answer_with_a_subsonic_envelope_not_product_json(pool: PgPool) {
    // clients probe unimplemented endpoints during discovery; the compat plane
    // must speak subsonic even there
    let response = get(
        app(pool.clone(), support::credential_service()),
        "/rest/getPodcasts.view?u=flynn&v=1.16.1&c=test-client&apiKey=whio_whatever",
    )
    .await;

    assert_eq!(response, failure(0));
}

#[sqlx::test]
async fn revoked_credentials_still_fail_authentication(pool: PgPool) {
    use base64::Engine;
    use base64::engine::general_purpose::STANDARD_NO_PAD;

    let key = whio_core::features::identity::secrets::CredentialKey::from_base64(
        &STANDARD_NO_PAD.encode([11u8; 32]),
    )
    .expect("valid key");
    let provisioning = whio_core::features::identity::provision(pool.clone());
    provisioning.create_user("flynn", "Flynn").await.unwrap();
    let (credential_id, secret) = provisioning
        .mint_app_password(&key, "flynn", "integration")
        .await
        .unwrap();
    let service = Arc::new(whio_core::features::identity::credentials(
        pool.clone(),
        key,
    ));
    let app = app(pool, service);
    let auth = format!("u=flynn&v=1.16.1&c=test-client&p={secret}");

    let response = get(app.clone(), &format!("/rest/ping.view?{auth}")).await;
    assert_eq!(response["subsonic-response"]["status"], json!("ok"));

    assert!(provisioning.revoke_credential(credential_id).await.unwrap());

    let response = get(app, &format!("/rest/ping.view?{auth}")).await;
    assert_eq!(response, failure(40));
}

const MUSIC_ROOT: &str = "/music";
const ELSEWHERE_ROOT: &str = "/elsewhere";

struct FolderFixture {
    app: axum::Router,
    auth: String,
    music: i32,
    elsewhere: i32,
}

struct FailingLibraryStore;

#[async_trait::async_trait]
impl LibraryStore for FailingLibraryStore {
    async fn find_by_label(&self, _label: &str) -> Result<Option<Library>, TrackRepositoryError> {
        Err(TrackRepositoryError::Unavailable)
    }

    async fn insert_library(
        &self,
        _label: &str,
        _roots: &[PathBuf],
    ) -> Result<Library, TrackRepositoryError> {
        Err(TrackRepositoryError::Unavailable)
    }

    async fn replace_roots(
        &self,
        _library_id: &LibraryId,
        _roots: &[PathBuf],
    ) -> Result<(), TrackRepositoryError> {
        Err(TrackRepositoryError::Unavailable)
    }

    async fn list_libraries(&self) -> Result<Vec<Library>, TrackRepositoryError> {
        Err(TrackRepositoryError::Unavailable)
    }

    async fn scopes_for_ids(
        &self,
        _ids: &[LibraryId],
    ) -> Result<Vec<String>, TrackRepositoryError> {
        Err(TrackRepositoryError::Unavailable)
    }
}

fn folder_source(root: &str, external_id: &str) -> SourceIdentity {
    SourceIdentity::new(
        ProviderId::new("local".to_owned()).expect("valid provider"),
        SourceScope::Named(root.to_owned()),
        external_id.to_owned(),
    )
    .expect("valid source")
}

async fn folder_album(
    store: &PostgresTrackRepository,
    title: &str,
    year: i32,
    root: &str,
    external_id: &str,
) -> AlbumId {
    let album = store
        .resolve_or_create_album(
            &AlbumMetadata::new(title.to_owned())
                .expect("valid album")
                .with_album_artist(
                    ArtistCredit::new("Album Artist".to_owned(), None).expect("valid credit"),
                )
                .with_year(Some(year))
                .expect("valid year"),
        )
        .await
        .expect("a stored album");
    store
        .set_album_source(album.id(), &folder_source(root, external_id))
        .await
        .expect("an album source");

    album.id().clone()
}

async fn folder_track(
    store: &PostgresTrackRepository,
    title: &str,
    root: &str,
    external_id: &str,
    genres: &[&str],
) -> TrackId {
    store
        .get_or_create(
            folder_source(root, external_id),
            TrackMetadata::from_names(title.to_owned(), vec!["Artist".to_owned()], Some(1_000))
                .expect("valid metadata")
                .with_genres(genres.iter().map(|genre| (*genre).to_owned()).collect()),
        )
        .await
        .expect("a stored track")
        .id()
        .clone()
}

async fn folder_fixture(pool: PgPool) -> FolderFixture {
    let ports = StorePorts::new(pool.clone());
    let (search, library) = ports.bare_catalogue();
    let playback = ports.streaming(vec![
        Arc::new(YtmAdapter::disabled()) as Arc<dyn PlaybackProvider>
    ]);
    let registry = Arc::new(LibraryRegistry::new(Arc::new(PostgresLibraryStore::new(
        pool.clone(),
    ))));
    let registered = registry
        .register(&[
            LibraryRegistration::new("music".to_owned(), vec![PathBuf::from(MUSIC_ROOT)]),
            LibraryRegistration::new("elsewhere".to_owned(), vec![PathBuf::from(ELSEWHERE_ROOT)]),
        ])
        .await
        .expect("registered libraries");
    let (credential, secret) = credential_service_with_known_app_password();
    let app = whio_core::app::router(
        AppState::new(credential, search, library, playback).with_library_registry(registry),
    );
    let store = Arc::new(PostgresTrackRepository::new(pool));
    let alpha_album = folder_album(&store, "Alpha Album", 1994, MUSIC_ROOT, "alpha.flac").await;
    let zeta_album = folder_album(&store, "Zeta Album", 1995, MUSIC_ROOT, "zeta.flac").await;
    let middle_album =
        folder_album(&store, "Middle Album", 2001, ELSEWHERE_ROOT, "middle.flac").await;
    let alpha = folder_track(
        &store,
        "Alpha",
        MUSIC_ROOT,
        "alpha.flac",
        &["shared", "rock"],
    )
    .await;
    let zeta = folder_track(&store, "Zeta", MUSIC_ROOT, "zeta.flac", &["shared"]).await;
    let middle = folder_track(
        &store,
        "Middle",
        ELSEWHERE_ROOT,
        "middle.flac",
        &["shared", "jazz"],
    )
    .await;

    for (album, track) in [
        (&alpha_album, &alpha),
        (&zeta_album, &zeta),
        (&middle_album, &middle),
    ] {
        store
            .set_album_track(album, track, &AlbumTrack::unplaced())
            .await
            .expect("membership stored");
    }

    let numeric = |label: &str| {
        registered
            .iter()
            .find(|library| library.label() == label)
            .expect("a registered library")
            .numeric_id()
    };

    FolderFixture {
        app,
        auth: format!("u=flynn&v=1.16.1&c=test-client&p={secret}"),
        music: numeric("music"),
        elsewhere: numeric("elsewhere"),
    }
}

async fn rest(app: &axum::Router, uri: &str) -> (StatusCode, Value) {
    let request = Request::builder()
        .method("GET")
        .uri(uri)
        .header("x-request-id", "request-123")
        .body(Body::empty())
        .unwrap();
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let body = to_bytes(response.into_body(), 8 * 1024 * 1024)
        .await
        .unwrap();

    (status, serde_json::from_slice(&body).unwrap())
}

async fn rest_ok(app: &axum::Router, auth: &str, path: &str, query: &str) -> Value {
    let (status, body) = rest(app, &format!("/rest{path}?{auth}&{query}")).await;

    assert_eq!(status, StatusCode::OK, "{path}?{query}: {body}");
    body
}

fn section<'a>(body: &'a Value, name: &str) -> &'a Value {
    &body["subsonic-response"][name]
}

fn set(values: &[&str]) -> HashSet<String> {
    values.iter().map(|value| (*value).to_owned()).collect()
}

fn albums(body: &Value) -> HashSet<String> {
    section(body, "albumList2")["album"]
        .as_array()
        .map(|items| {
            items
                .iter()
                .map(|item| item["name"].as_str().expect("an album name").to_owned())
                .collect()
        })
        .unwrap_or_default()
}

fn titles(songs: &Value) -> HashSet<String> {
    songs["song"]
        .as_array()
        .map(|items| {
            items
                .iter()
                .map(|item| item["title"].as_str().expect("a song title").to_owned())
                .collect()
        })
        .unwrap_or_default()
}

#[sqlx::test]
async fn subsonic_music_folder_scopes_the_browse(pool: PgPool) {
    let fixture = folder_fixture(pool).await;
    let music = fixture.music;
    let elsewhere = fixture.elsewhere;

    let unscoped = rest_ok(
        &fixture.app,
        &fixture.auth,
        "/getAlbumList2.view",
        "type=alphabeticalByName",
    )
    .await;
    assert_eq!(
        albums(&unscoped),
        set(&["Alpha Album", "Middle Album", "Zeta Album"]),
        "an unscoped request is unchanged"
    );

    let scoped_music = rest_ok(
        &fixture.app,
        &fixture.auth,
        "/getAlbumList2.view",
        &format!("type=alphabeticalByName&musicFolderId={music}"),
    )
    .await;
    assert_eq!(albums(&scoped_music), set(&["Alpha Album", "Zeta Album"]));

    let scoped_elsewhere = rest_ok(
        &fixture.app,
        &fixture.auth,
        "/getAlbumList2.view",
        &format!("type=alphabeticalByName&musicFolderId={elsewhere}"),
    )
    .await;
    assert_eq!(albums(&scoped_elsewhere), set(&["Middle Album"]));

    let combined = rest_ok(
        &fixture.app,
        &fixture.auth,
        "/getAlbumList2.view",
        &format!("type=alphabeticalByName&musicFolderId={music}&musicFolderId={elsewhere}"),
    )
    .await;
    assert_eq!(
        albums(&combined),
        set(&["Alpha Album", "Middle Album", "Zeta Album"]),
        "the documented form is repeatable and selects the union"
    );

    let genre_scoped = rest_ok(
        &fixture.app,
        &fixture.auth,
        "/getSongsByGenre.view",
        &format!("genre=shared&musicFolderId={music}"),
    )
    .await;
    assert_eq!(
        titles(section(&genre_scoped, "songsByGenre")),
        set(&["Alpha", "Zeta"])
    );

    let random_scoped = rest_ok(
        &fixture.app,
        &fixture.auth,
        "/getRandomSongs.view",
        &format!("genre=shared&size=10&musicFolderId={elsewhere}"),
    )
    .await;
    assert_eq!(
        titles(section(&random_scoped, "randomSongs")),
        set(&["Middle"])
    );
}

#[sqlx::test]
async fn subsonic_music_folder_rejects_an_unknown_id(pool: PgPool) {
    let fixture = folder_fixture(pool).await;

    for (path, query) in [
        (
            "/getAlbumList2.view",
            "type=alphabeticalByName&musicFolderId=999",
        ),
        ("/getSongsByGenre.view", "genre=shared&musicFolderId=999"),
        ("/getRandomSongs.view", "musicFolderId=999"),
        (
            "/getAlbumList2.view",
            "type=alphabeticalByName&musicFolderId=abc",
        ),
    ] {
        let (status, body) = rest(
            &fixture.app,
            &format!("/rest{path}?{}&{query}", fixture.auth),
        )
        .await;

        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{path}: {body}");
        assert_eq!(
            body["subsonic-response"]["error"]["message"],
            json!("Invalid browse parameters."),
            "{path}"
        );
    }
}

#[sqlx::test]
async fn subsonic_music_folder_store_failure_is_not_a_client_error(pool: PgPool) {
    let ports = StorePorts::new(pool);
    let (search, library) = ports.bare_catalogue();
    let playback = ports.streaming(vec![
        Arc::new(YtmAdapter::disabled()) as Arc<dyn PlaybackProvider>
    ]);
    let (credential, secret) = credential_service_with_known_app_password();
    let failing = Arc::new(LibraryRegistry::new(Arc::new(FailingLibraryStore)));
    let app = whio_core::app::router(
        AppState::new(credential, search, library, playback).with_library_registry(failing),
    );
    let auth = format!("u=flynn&v=1.16.1&c=test-client&p={secret}");

    for (path, query) in [
        (
            "/getAlbumList2.view",
            "type=alphabeticalByName&musicFolderId=1",
        ),
        ("/getSongsByGenre.view", "genre=shared&musicFolderId=1"),
        ("/getRandomSongs.view", "musicFolderId=1"),
    ] {
        let (status, body) = rest(&app, &format!("/rest{path}?{auth}&{query}")).await;

        assert_eq!(
            status,
            StatusCode::INTERNAL_SERVER_ERROR,
            "a store failure is not a client error: {path}: {body}"
        );
        assert_eq!(
            body["subsonic-response"]["error"]["message"],
            json!("Internal error."),
            "{path}"
        );
    }

    let (status, _) = rest(
        &app,
        &format!("/rest/getAlbumList2.view?{auth}&type=alphabeticalByName"),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "a list without musicFolderId needs no registry"
    );
}

#[sqlx::test]
async fn subsonic_music_folder_leaves_other_parameters_alone(pool: PgPool) {
    let fixture = folder_fixture(pool).await;
    let music = fixture.music;

    let genre = rest_ok(
        &fixture.app,
        &fixture.auth,
        "/getSongsByGenre.view",
        "genre=shared",
    )
    .await;
    let genre_scoped = rest_ok(
        &fixture.app,
        &fixture.auth,
        "/getSongsByGenre.view",
        &format!("genre=shared&musicFolderId={music}"),
    )
    .await;
    assert_eq!(
        titles(section(&genre, "songsByGenre")),
        set(&["Alpha", "Middle", "Zeta"]),
        "genre alone is unchanged"
    );
    assert_eq!(
        titles(section(&genre_scoped, "songsByGenre")),
        set(&["Alpha", "Zeta"]),
        "the genre and the folder both apply"
    );

    let window = rest_ok(
        &fixture.app,
        &fixture.auth,
        "/getAlbumList2.view",
        "type=alphabeticalByName&size=1&offset=1",
    )
    .await;
    let window_scoped = rest_ok(
        &fixture.app,
        &fixture.auth,
        "/getAlbumList2.view",
        &format!("type=alphabeticalByName&size=1&offset=1&musicFolderId={music}"),
    )
    .await;
    assert_eq!(
        albums(&window),
        set(&["Middle Album"]),
        "the window alone is unchanged"
    );
    assert_eq!(
        albums(&window_scoped),
        set(&["Zeta Album"]),
        "the window and the folder both apply"
    );

    let years = rest_ok(
        &fixture.app,
        &fixture.auth,
        "/getRandomSongs.view",
        "size=10&fromYear=2000&toYear=2010",
    )
    .await;
    let years_scoped = rest_ok(
        &fixture.app,
        &fixture.auth,
        "/getRandomSongs.view",
        &format!("size=10&fromYear=2000&toYear=2010&musicFolderId={music}"),
    )
    .await;
    assert_eq!(
        titles(section(&years, "randomSongs")),
        set(&["Middle"]),
        "the year range alone is unchanged"
    );
    assert_eq!(
        titles(section(&years_scoped, "randomSongs")),
        set(&[]),
        "the year range and the folder both apply"
    );
}
