use sqlx::PgPool;
use std::sync::Arc;
use whio_core::features::music_library::test_support::PostgresTrackRepository;

use crate::support;

use support::{NATIVE_AUTH_QUERY, StorePorts, credential_service, router};

use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use serde_json::{Value, json};
use tower::ServiceExt;
use whio_core::{
    features::music_library::catalogue::CatalogueProvider,
    features::music_library::domain::{ProviderId, SourceIdentity, SourceScope, TrackMetadata},
    features::music_library::test_support::CatalogueWrite,
    features::playback::PlaybackProvider,
    integrations::ytm::catalogue::YtmCatalogue,
    integrations::ytm::playback::YtmPlayback,
};

fn source() -> SourceIdentity {
    SourceIdentity::new(
        ProviderId::new("youtube_music".to_owned()).unwrap(),
        SourceScope::Global,
        "source-123".to_owned(),
    )
    .unwrap()
}

fn metadata() -> TrackMetadata {
    TrackMetadata::from_names("Title".to_owned(), vec!["Artist".to_owned()], Some(1_000)).unwrap()
}

async fn app(pool: PgPool) -> (axum::Router, String) {
    let repository = Arc::new(PostgresTrackRepository::new(pool.clone()));
    let track_id = repository
        .get_or_create(source(), metadata())
        .await
        .unwrap()
        .id()
        .to_string();
    // Disabled catalogue and playback adapters both map to 503 responses.
    let ports = StorePorts { store: repository };
    let (search, library) = ports.catalogue_service(vec![
        Arc::new(YtmCatalogue::disabled()) as Arc<dyn CatalogueProvider>
    ]);
    let playback = ports.streaming(vec![
        Arc::new(YtmPlayback::disabled()) as Arc<dyn PlaybackProvider>
    ]);

    (
        router(credential_service(), search, library, playback),
        track_id.to_string(),
    )
}

fn request(method: &str, uri: &str, body: &str) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(format!("{uri}{NATIVE_AUTH_QUERY}"))
        .header("content-type", "application/json")
        .header("x-request-id", "request-123")
        .body(Body::from(body.to_owned()))
        .unwrap()
}

async fn response_body(response: axum::response::Response) -> Value {
    let body = to_bytes(response.into_body(), 4096).await.unwrap();
    serde_json::from_slice(&body).unwrap()
}

#[sqlx::test]
async fn disabled_youtube_returns_a_safe_catalogue_error(pool: PgPool) {
    let (app, _) = app(pool.clone()).await;
    let response = app
        .oneshot(request(
            "POST",
            "/catalogue/search",
            r#"{"query":"Daft Punk","limit":5}"#,
        ))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        response_body(response).await,
        json!({
            "code": "catalogue_unavailable",
            "message": "The catalogue is temporarily unavailable",
            "request_id": "request-123"
        })
    );
}

#[sqlx::test]
async fn disabled_youtube_returns_a_safe_playback_error(pool: PgPool) {
    let (app, track_id) = app(pool.clone()).await;
    let response = app
        .oneshot(request(
            "GET",
            &format!("/playback/tracks/{track_id}/stream"),
            "",
        ))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        response_body(response).await,
        json!({
            "code": "playback_unavailable",
            "message": "Playback is temporarily unavailable",
            "request_id": "request-123"
        })
    );
}
