use std::sync::Arc;

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
use support::{NATIVE_AUTH_QUERY, delivery, router};
use whio_core::{
    catalogue::CatalogueService,
    identity::{
        domain::{User, UserId},
        secrets::CredentialKey,
        service::CredentialService,
    },
    playback::PlaybackService,
    tracks::{
        ArtworkRef, InMemoryTrackRepository, ProviderId, SourceIdentity, SourceScope,
        TrackMetadata, TrackRepository,
    },
};

fn credential_key() -> CredentialKey {
    CredentialKey::from_base64(&STANDARD_NO_PAD.encode([11u8; 32])).expect("valid test key")
}

/// one user whose credentials match `NATIVE_AUTH_QUERY`.
fn credentials() -> (Arc<CredentialService>, Uuid, CredentialKey) {
    let key = credential_key();
    let store = Arc::new(MemoryCredentialStore::new());
    let user_id = UserId::from_uuid(Uuid::now_v7());
    let user_uuid = user_id.as_uuid();
    store.insert_user(User {
        id: user_id,
        username: "test-user".to_owned(),
        display_name: "Integration Test".to_owned(),
    });
    store.insert_app_password(&key, TEST_PASSWORD, user_id);

    (
        Arc::new(CredentialService::new(store, key.clone())),
        user_uuid,
        key,
    )
}

fn metadata() -> TrackMetadata {
    TrackMetadata::new("Song".to_owned(), vec!["Artist".to_owned()], Some(1_000)).unwrap()
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

/// a global (public-URL) source, which is what provider-supplied artwork is
/// today: the reference is a URL core fetches under the allowlist.
async fn public_app(url: &str) -> (axum::Router, String) {
    let (credential, _user, _key) = credentials();
    let repository = Arc::new(InMemoryTrackRepository::default());

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

    let catalogue = Arc::new(CatalogueService::new(
        Arc::clone(&repository) as Arc<dyn TrackRepository>
    ));
    let playback = Arc::new(PlaybackService::new(repository, Vec::new(), delivery()));

    (
        router(credential, catalogue, playback),
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

#[tokio::test]
async fn unknown_and_malformed_track_ids_do_not_serve_artwork() {
    let base = image_server(b"cover-bytes").await;
    let (app, _track_id) = public_app(&format!("{base}/cover.jpg")).await;

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

#[tokio::test]
async fn public_artwork_outside_the_allowed_hosts_is_not_fetched() {
    let base = image_server(b"cover-bytes").await;
    let (app, track_id) = public_app(&format!("{base}/cover.jpg")).await;

    let response = app.oneshot(artwork_request(&track_id)).await.unwrap();

    // the local http host is not an allowed https suffix, and the request never
    // reaches the image server
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}
