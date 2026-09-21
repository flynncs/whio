use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use serde_json::{Value, json};
use tower::ServiceExt;
use uuid::Uuid;
use whio_core::{
    catalogue::CatalogueCandidate,
    providers::ProviderError,
    tracks::{ProviderId, SourceIdentity, SourceScope, TrackMetadata},
};

mod support;

use support::{
    NATIVE_AUTH_QUERY, StorePorts, app, credential_service,
    credential_service_with_storage_failure, router, services,
};

fn candidate() -> CatalogueCandidate {
    let provider_id = ProviderId::new("youtube_music".to_owned()).unwrap();
    let source =
        SourceIdentity::new(provider_id, SourceScope::Global, "source-123".to_owned()).unwrap();
    let metadata = TrackMetadata::from_names(
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
        .uri(format!("/catalogue/search{NATIVE_AUTH_QUERY}"))
        .header("content-type", "application/json")
        .header("x-request-id", "request-123")
        .body(Body::from(body.to_owned()))
        .unwrap()
}

fn unauthenticated_request(body: &str) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri("/catalogue/search")
        .header("content-type", "application/json")
        .header("x-request-id", "request-123")
        .body(Body::from(body.to_owned()))
        .unwrap()
}

#[tokio::test]
async fn search_requires_authentication() {
    let response = app(vec![])
        .oneshot(unauthenticated_request(
            r#"{"query":"Daft Punk","limit":5}"#,
        ))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn credential_storage_failure_is_a_server_error() {
    let (search, library, playback) = services(vec![]);
    let response = router(
        credential_service_with_storage_failure(),
        search,
        library,
        playback,
    )
    .oneshot(
        Request::builder()
            .method("POST")
            .uri(format!("/catalogue/search{NATIVE_AUTH_QUERY}"))
            .header("content-type", "application/json")
            .header("x-request-id", "request-123")
            .body(Body::from(r#"{"query":"Daft Punk","limit":5}"#))
            .unwrap(),
    )
    .await
    .unwrap();

    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
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
    assert_eq!(body["confidence"], "none");
    assert!(track.get("match_quality").is_none());
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
    let response = app(vec![Err(ProviderError::ProviderUnavailable)])
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

#[tokio::test]
async fn a_disabled_provider_track_is_reported_unavailable() {
    use std::sync::Arc;
    use whio_core::{
        catalogue::{LibraryService, SearchService},
        tracks::{CatalogueWrite, InMemoryTrackRepository, ProviderRegistry},
    };

    fn source(provider: &str, scope: SourceScope, external_id: &str) -> SourceIdentity {
        SourceIdentity::new(
            ProviderId::new(provider.to_owned()).unwrap(),
            scope,
            external_id.to_owned(),
        )
        .unwrap()
    }

    let repository = Arc::new(InMemoryTrackRepository::default());
    repository
        .get_or_create(
            source("local", SourceScope::Named("/music".to_owned()), "path-a"),
            TrackMetadata::from_names("Owned Song".to_owned(), vec!["Artist".to_owned()], None)
                .unwrap(),
        )
        .await
        .unwrap();
    repository
        .get_or_create(
            source("youtube_music", SourceScope::Global, "vid-1"),
            TrackMetadata::from_names("Provider Song".to_owned(), vec!["Artist".to_owned()], None)
                .unwrap(),
        )
        .await
        .unwrap();

    let ports = StorePorts { store: repository };
    let search = Arc::new(SearchService::new(
        ports.library(),
        ports.catalogue(),
        ports.sources(),
        ports.state(),
    ));
    let library = Arc::new(
        LibraryService::new(ports.library(), ports.sources(), ports.state())
            .with_registry(ProviderRegistry::with_enabled(Vec::new())),
    );
    let playback = ports.streaming(Vec::new());
    let response = router(credential_service(), search, library, playback)
        .oneshot(request(r#"{"query":"Song","limit":5}"#))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), 8192).await.unwrap();
    let body: Value = serde_json::from_slice(&body).unwrap();
    let track = |title: &str| {
        body["results"]
            .as_array()
            .unwrap()
            .iter()
            .find(|hit| hit["title"] == json!(title))
            .unwrap()
    };

    let owned = track("Owned Song");
    assert_eq!(owned["ownership"], json!("owned"));
    assert_eq!(owned["availability"], json!("owned"));

    let provider = track("Provider Song");
    assert_eq!(provider["ownership"], json!("provider"));
    assert_eq!(provider["availability"], json!("unavailable"));
}
