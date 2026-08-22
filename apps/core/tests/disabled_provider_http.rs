use std::sync::Arc;

#[path = "support/media.rs"]
mod media_support;
use media_support::playback_stream;

use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use serde_json::{Value, json};
use tower::ServiceExt;
use whio_core::{
    AppState,
    catalogue::CatalogueService,
    playback::PlaybackService,
    resolver::DisabledResolver,
    tracks::{
        InMemoryTrackRepository, ProviderId, SourceIdentity, SourceScope, TrackMetadata,
        TrackRepository,
    },
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
    TrackMetadata::new("Title".to_owned(), vec!["Artist".to_owned()], Some(1_000)).unwrap()
}

async fn app() -> (axum::Router, String) {
    let resolver = Arc::new(DisabledResolver);
    let repository = Arc::new(InMemoryTrackRepository::default());
    let track_id = repository
        .get_or_create(source(), metadata())
        .await
        .unwrap()
        .id()
        .to_string();
    let catalogue = Arc::new(CatalogueService::new(resolver.clone(), repository.clone()));
    let playback = Arc::new(PlaybackService::new(resolver, repository));
    let playback_stream = playback_stream(Arc::clone(&playback));

    (
        whio_core::router(AppState::new(catalogue, playback, playback_stream)),
        track_id.to_string(),
    )
}

fn request(method: &str, uri: &str, body: &str) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json")
        .header("x-request-id", "request-123")
        .body(Body::from(body.to_owned()))
        .unwrap()
}

async fn response_body(response: axum::response::Response) -> Value {
    let body = to_bytes(response.into_body(), 4096).await.unwrap();
    serde_json::from_slice(&body).unwrap()
}

#[tokio::test]
async fn disabled_youtube_returns_a_safe_catalogue_error() {
    let (app, _) = app().await;
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

#[tokio::test]
async fn disabled_youtube_returns_a_safe_playback_error() {
    let (app, track_id) = app().await;
    let response = app
        .oneshot(request(
            "POST",
            "/playback/resolve",
            &json!({"track_id": track_id}).to_string(),
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
