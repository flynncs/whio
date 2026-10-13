use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use serde_json::{Value, json};
use sqlx::PgPool;
use tower::ServiceExt;
use uuid::Uuid;
use whio_core::features::music_library::test_support::PostgresTrackRepository;
use whio_core::{
    features::music_library::catalogue::CatalogueCandidate,
    features::music_library::catalogue::CatalogueProvider,
    features::music_library::catalogue::ProviderError,
    features::music_library::domain::{ProviderId, SourceIdentity, SourceScope, TrackMetadata},
};

use crate::support;

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

struct ProviderOnlyTrack;

#[async_trait::async_trait]
impl CatalogueProvider for ProviderOnlyTrack {
    async fn search_tracks(
        &self,
        _search: &whio_core::features::music_library::catalogue::CatalogueSearch,
        _context: &whio_core::platform::request::RequestContext,
    ) -> Result<Vec<CatalogueCandidate>, ProviderError> {
        Ok(vec![CatalogueCandidate::new(
            SourceIdentity::new(
                ProviderId::new("youtube_music".to_owned()).unwrap(),
                SourceScope::Global,
                "vid-1".to_owned(),
            )
            .unwrap(),
            TrackMetadata::from_names("Provider Song".to_owned(), vec!["Artist".to_owned()], None)
                .unwrap(),
        )])
    }
}

fn request(body: &str) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(format!("/catalogue/search{NATIVE_AUTH_QUERY}"))
        .header("content-type", "application/json")
        .header("x-request-id", support::TEST_REQUEST_ID)
        .body(Body::from(body.to_owned()))
        .unwrap()
}

fn unauthenticated_request(body: &str) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri("/catalogue/search")
        .header("content-type", "application/json")
        .header("x-request-id", support::TEST_REQUEST_ID)
        .body(Body::from(body.to_owned()))
        .unwrap()
}

#[sqlx::test]
async fn search_requires_authentication(pool: PgPool) {
    let response = app(pool.clone(), vec![])
        .oneshot(unauthenticated_request(
            r#"{"query":"Daft Punk","limit":5}"#,
        ))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[sqlx::test]
async fn credential_storage_failure_is_a_server_error(pool: PgPool) {
    let (search, library, playback) = services(pool.clone(), vec![]);
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
            .header("x-request-id", support::TEST_REQUEST_ID)
            .body(Body::from(r#"{"query":"Daft Punk","limit":5}"#))
            .unwrap(),
    )
    .await
    .unwrap();

    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
}

#[sqlx::test]
async fn search_returns_whio_tracks(pool: PgPool) {
    let response = app(pool.clone(), vec![Ok(vec![candidate()])])
        .oneshot(request(r#"{"query":"Daft Punk","limit":5}"#))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["x-request-id"], support::TEST_REQUEST_ID);

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

#[sqlx::test]
async fn malformed_and_invalid_requests_share_the_public_error_shape(pool: PgPool) {
    for body in [
        r#"{"query":"","limit":5}"#,
        r#"{"query":"Daft Punk","limit":"five"}"#,
    ] {
        let response = app(pool.clone(), vec![])
            .oneshot(request(body))
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let body = to_bytes(response.into_body(), 4096).await.unwrap();
        let expected = format!(
            r#"{{"code":"invalid_request","message":"The request is invalid","request_id":"{}"}}"#,
            support::TEST_REQUEST_ID
        );
        assert_eq!(body.as_ref(), expected.as_bytes());
    }
}

#[sqlx::test]
async fn unavailable_resolver_returns_service_unavailable(pool: PgPool) {
    let response = app(pool.clone(), vec![Err(ProviderError::ProviderUnavailable)])
        .oneshot(request(r#"{"query":"Daft Punk","limit":5}"#))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    let body = to_bytes(response.into_body(), 4096).await.unwrap();
    let expected = format!(
        r#"{{"code":"catalogue_unavailable","message":"The catalogue is temporarily unavailable","request_id":"{}"}}"#,
        support::TEST_REQUEST_ID
    );
    assert_eq!(body.as_ref(), expected.as_bytes());
}

#[sqlx::test]
async fn a_disabled_provider_track_is_reported_unavailable(pool: PgPool) {
    use std::sync::Arc;
    use whio_core::{
        features::music_library::availability::ProviderRegistry,
        features::music_library::catalogue::LibraryService,
        features::music_library::test_support::CatalogueWrite,
    };

    fn source(provider: &str, scope: SourceScope, external_id: &str) -> SourceIdentity {
        SourceIdentity::new(
            ProviderId::new(provider.to_owned()).unwrap(),
            scope,
            external_id.to_owned(),
        )
        .unwrap()
    }

    let repository = Arc::new(PostgresTrackRepository::new(pool.clone()));
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
    let (search, _) = ports.catalogue_service(vec![
        Arc::new(ProviderOnlyTrack) as Arc<dyn CatalogueProvider>
    ]);
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
