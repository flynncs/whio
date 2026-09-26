#![cfg(feature = "postgres-integration")]

use sqlx::PgPool;
use std::sync::Arc;
use whio_core::features::music_library::test_support::PostgresTrackRepository;

mod support;

use axum::{
    body::Body,
    http::{Request, StatusCode, header},
};
use base64::Engine;
use base64::engine::general_purpose::STANDARD_NO_PAD;
use tower::ServiceExt;
use uuid::Uuid;

use support::credentials::{MemoryCredentialStore, TEST_PASSWORD};
use support::{NATIVE_AUTH_QUERY, StorePorts, router_with_artwork};
use whio_core::{
    features::identity::{
        domain::{User, UserId, UserRole},
        secrets::CredentialKey,
        service::CredentialService,
    },
    features::music_library::artwork::ArtworkService,
    features::music_library::domain::{
        ArtworkRef, ProviderId, SourceIdentity, SourceScope, TrackMetadata,
    },
};

fn credential_key() -> CredentialKey {
    CredentialKey::from_base64(&STANDARD_NO_PAD.encode([11u8; 32])).expect("valid test key")
}

fn credentials() -> (Arc<CredentialService>, Uuid, CredentialKey) {
    let key = credential_key();
    let store = Arc::new(MemoryCredentialStore::new());
    let user_id = UserId::from_uuid(Uuid::now_v7());
    let user_uuid = user_id.as_uuid();
    store.insert_user(User {
        id: user_id,
        username: "test-user".to_owned(),
        display_name: "Integration Test".to_owned(),
        role: UserRole::Member,
    });
    store.insert_app_password(&key, TEST_PASSWORD, user_id);

    (
        Arc::new(CredentialService::new(store, key.clone())),
        user_uuid,
        key,
    )
}

fn metadata() -> TrackMetadata {
    TrackMetadata::from_names("Song".to_owned(), vec!["Artist".to_owned()], Some(1_000)).unwrap()
}

async fn image_server(bytes: &'static [u8]) -> String {
    use axum::{Router, routing::get};

    let app = Router::new().route(
        "/cover.jpg",
        get(move || async move { ([(header::CONTENT_TYPE, "image/jpeg")], bytes) }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await });
    format!("http://{address}")
}

async fn public_app(pool: PgPool, url: &str) -> (axum::Router, String) {
    use whio_core::features::music_library::test_support::{CatalogueWrite, SourceLookup};

    let (credential, _user, _key) = credentials();
    let repository = Arc::new(PostgresTrackRepository::new(pool.clone()));

    let source = SourceIdentity::new(
        ProviderId::new("youtube_music".to_owned()).unwrap(),
        SourceScope::Global,
        "vid-1".to_owned(),
    )
    .unwrap();
    let track = repository
        .get_or_create(source.clone(), metadata())
        .await
        .unwrap();
    repository
        .set_source_artwork(&source, &ArtworkRef::url(url.to_owned()).unwrap())
        .await
        .unwrap();

    let ports = StorePorts { store: repository };
    let (search, library) = ports.bare_catalogue();
    let artwork = Arc::new(ArtworkService::new(
        ports.library(),
        ports.sources(),
        whio_core::features::music_library::artwork::default_host_suffixes(),
    ));
    let playback = ports.streaming(Vec::new());

    (
        router_with_artwork(credential, search, library, playback, artwork),
        track.id().to_string(),
    )
}

fn artwork_request(track_id: &str) -> Request<Body> {
    Request::builder()
        .method("GET")
        .uri(format!(
            "/playback/tracks/{track_id}/artwork{NATIVE_AUTH_QUERY}"
        ))
        .header("x-request-id", "request-123")
        .body(Body::empty())
        .unwrap()
}

#[sqlx::test]
async fn unknown_and_malformed_track_ids_do_not_serve_artwork(pool: PgPool) {
    let base = image_server(b"cover-bytes").await;
    let (app, _track_id) = public_app(pool.clone(), &format!("{base}/cover.jpg")).await;

    let response = app
        .clone()
        .oneshot(artwork_request(&Uuid::now_v7().to_string()))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);

    let response = app
        .oneshot(artwork_request("not-a-track-id"))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[sqlx::test]
async fn public_artwork_outside_the_allowed_hosts_is_not_fetched(pool: PgPool) {
    let base = image_server(b"cover-bytes").await;
    let (app, track_id) = public_app(pool.clone(), &format!("{base}/cover.jpg")).await;

    let response = app.oneshot(artwork_request(&track_id)).await.unwrap();

    // the local http host is not an allowed https suffix, and the request never
    // reaches the image server
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

async fn pictured_app(pool: PgPool) -> (axum::Router, String, String, String) {
    use whio_core::features::music_library::domain::{AlbumMetadata, AlbumTrack, ArtistCredit};
    use whio_core::features::music_library::test_support::{CatalogueWrite, SourceFileFacts};

    let (credential, _user, _key) = credentials();
    let root = std::env::temp_dir().join(format!("whio-http-art-{}", Uuid::now_v7()));
    std::fs::create_dir_all(&root).unwrap();
    let file = support::pictured_flac(&root);

    let repository = Arc::new(PostgresTrackRepository::new(pool.clone()));
    let source = SourceIdentity::new(
        ProviderId::new("local".to_owned()).unwrap(),
        SourceScope::Named(root.display().to_string()),
        "recording-1".to_owned(),
    )
    .unwrap();
    let track = repository
        .get_or_create(source.clone(), metadata())
        .await
        .unwrap();
    repository
        .set_source_file_facts(
            &source,
            &SourceFileFacts {
                path: Some("a.flac".to_owned()),
                size_bytes: std::fs::metadata(&file).unwrap().len(),
                modified_at: None,
                missing_at: None,
                content_type: None,
                suffix: Some("flac".to_owned()),
                acoustid_fingerprint: None,
            },
        )
        .await
        .unwrap();
    let artist = repository
        .resolve_or_create_artist(&ArtistCredit::new("Artist".to_owned(), None).unwrap())
        .await
        .unwrap();
    let album = repository
        .resolve_or_create_album(
            &AlbumMetadata::new("Album".to_owned())
                .unwrap()
                .with_album_artist(ArtistCredit::new("Artist".to_owned(), None).unwrap()),
        )
        .await
        .unwrap();
    repository
        .set_album_track(
            album.id(),
            track.id(),
            &AlbumTrack::new(Some(1), Some(1), 0).unwrap(),
        )
        .await
        .unwrap();

    let ports = StorePorts { store: repository };
    let (search, library) = ports.bare_catalogue();
    let artwork = Arc::new(ArtworkService::new(
        ports.library(),
        ports.sources(),
        whio_core::features::music_library::artwork::default_host_suffixes(),
    ));
    let playback = ports.streaming(Vec::new());

    (
        router_with_artwork(credential, search, library, playback, artwork),
        format!("al-{}", album.id()),
        format!("ar-{}", artist.id()),
        track.id().to_string(),
    )
}

fn subsonic_cover_art_request(id: &str) -> Request<Body> {
    Request::builder()
        .method("GET")
        .uri(format!(
            "/rest/getCoverArt.view?u=test-user&v=1.16.1&c=test&p={TEST_PASSWORD}&id={id}"
        ))
        .header("x-request-id", "request-123")
        .body(Body::empty())
        .unwrap()
}

#[sqlx::test]
async fn album_and_artist_ids_serve_embedded_art_with_validators(pool: PgPool) {
    let (app, album_id, artist_id, track_id) = pictured_app(pool.clone()).await;

    for id in [&album_id, &artist_id, &track_id] {
        let response = app
            .clone()
            .oneshot(subsonic_cover_art_request(id))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK, "id: {id}");
        assert_eq!(
            response.headers().get(header::CONTENT_TYPE).unwrap(),
            "image/jpeg"
        );
        let etag = response
            .headers()
            .get(header::ETAG)
            .expect("etag")
            .to_str()
            .unwrap()
            .to_owned();
        assert_eq!(
            response.headers().get(header::CACHE_CONTROL).unwrap(),
            "public, max-age=86400"
        );

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!(
                        "/rest/getCoverArt.view?u=test-user&v=1.16.1&c=test&p={TEST_PASSWORD}&id={id}"
                    ))
                    .header("x-request-id", "request-123")
                    .header(header::IF_NONE_MATCH, etag)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_MODIFIED, "id: {id}");
    }
}

#[sqlx::test]
async fn unknown_albums_and_malformed_ids_do_not_serve_artwork(pool: PgPool) {
    let (app, _, _, _) = pictured_app(pool.clone()).await;

    let response = app
        .clone()
        .oneshot(subsonic_cover_art_request(&format!(
            "al-{}",
            Uuid::now_v7()
        )))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);

    let response = app
        .oneshot(subsonic_cover_art_request("al-not-a-uuid"))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}
