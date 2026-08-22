use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use serde_json::{Value, json};
use tower::ServiceExt;
use uuid::Uuid;
use whio_core::{
    catalogue::CatalogueCandidate,
    resolver::ResolverError,
    tracks::{ProviderId, SourceIdentity, SourceScope, TrackMetadata},
};

mod support;

use support::app;

fn candidate() -> CatalogueCandidate {
    let provider_id = ProviderId::new("youtube_music".to_owned()).unwrap();
    let source =
        SourceIdentity::new(provider_id, SourceScope::Global, "source-123".to_owned()).unwrap();
    let metadata = TrackMetadata::new(
        "Instant Crush".to_owned(),
        vec!["Daft Punk".to_owned()],
        Some(337_000),
    )
    .unwrap();

    CatalogueCandidate::new(source, metadata)
}

fn request(body: &str) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri("/catalogue/search")
        .header("content-type", "application/json")
        .header("x-request-id", "request-123")
        .body(Body::from(body.to_owned()))
        .unwrap()
}

#[tokio::test]
async fn search_returns_whio_tracks() {
    let response = app(vec![Ok(vec![candidate()])])
        .oneshot(request(r#"{"query":"Daft Punk","limit":5}"#))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["x-request-id"], "request-123");

    let body = to_bytes(response.into_body(), 4096).await.unwrap();
    let body: Value = serde_json::from_slice(&body).unwrap();
    let track = &body["results"][0];

    assert!(Uuid::parse_str(track["id"].as_str().unwrap()).is_ok());
    assert_eq!(track["title"], "Instant Crush");
    assert_eq!(track["artists"], json!(["Daft Punk"]));
    assert_eq!(track["duration_ms"], 337_000);
}

#[tokio::test]
async fn malformed_and_invalid_requests_share_the_public_error_shape() {
    for body in [
        r#"{"query":"","limit":5}"#,
        r#"{"query":"Daft Punk","limit":"five"}"#,
    ] {
        let response = app(vec![]).oneshot(request(body)).await.unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let body = to_bytes(response.into_body(), 4096).await.unwrap();
        assert_eq!(
            body.as_ref(),
            br#"{"code":"invalid_request","message":"The request is invalid","request_id":"request-123"}"#
        );
    }
}

#[tokio::test]
async fn unavailable_resolver_returns_service_unavailable() {
    let response = app(vec![Err(ResolverError::ProviderUnavailable)])
        .oneshot(request(r#"{"query":"Daft Punk","limit":5}"#))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    let body = to_bytes(response.into_body(), 4096).await.unwrap();
    assert_eq!(
        body.as_ref(),
        br#"{"code":"catalogue_unavailable","message":"The catalogue is temporarily unavailable","request_id":"request-123"}"#
    );
}
