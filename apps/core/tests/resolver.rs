use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

use axum::{
    Json, Router,
    extract::State,
    http::{HeaderMap, StatusCode, header},
    response::IntoResponse,
    routing::post,
};
use serde_json::{Value, json};
use tokio::{net::TcpListener, task::JoinHandle};
use whio_core::{
    catalogue::ValidationError,
    resolver::{CatalogueResolver, ResolverClient, ResolverError},
};

#[derive(Debug)]
struct CapturedRequest {
    body: Value,
    request_id: Option<String>,
}

async fn spawn_server(router: Router) -> (reqwest::Url, JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });

    let base_url = format!("http://{address}/").parse().unwrap();
    (base_url, server)
}

async fn stop_server(server: JoinHandle<()>) {
    server.abort();
    let _ = server.await;
}

fn client(base_url: reqwest::Url) -> ResolverClient {
    ResolverClient::new(base_url, Duration::from_secs(1), Duration::from_secs(2)).unwrap()
}

async fn successful_search(
    State(captured): State<Arc<Mutex<Option<CapturedRequest>>>>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> impl IntoResponse {
    let request_id = headers
        .get("x-request-id")
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);

    *captured.lock().unwrap() = Some(CapturedRequest { body, request_id });

    (
        StatusCode::OK,
        Json(json!({
            "results": [{
                "source": {
                    "provider_id": "example_music",
                    "external_id": "example-source-id"
                },
                "title": "Instant Crush",
                "artists": ["Daft Punk"],
                "duration_ms": 337000
            }]
        })),
    )
}

async fn unavailable() -> impl IntoResponse {
    StatusCode::SERVICE_UNAVAILABLE
}

async fn malformed_response() -> impl IntoResponse {
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/json")],
        "{not-json",
    )
}

async fn negative_duration() -> impl IntoResponse {
    (
        StatusCode::OK,
        Json(json!({
            "results": [{
                "source": {
                    "provider_id": "example_music",
                    "external_id": "example-source-id"
                },
                "title": "Instant Crush",
                "artists": ["Daft Punk"],
                "duration_ms": -1
            }]
        })),
    )
}

#[tokio::test]
async fn search_deserializes_maps_and_forwards_request_id() {
    let captured = Arc::new(Mutex::new(None));
    let router = Router::new()
        .route("/v1/catalogue/search", post(successful_search))
        .with_state(captured.clone());
    let (base_url, server) = spawn_server(router).await;
    let result = client(base_url)
        .search("Instant Crush", 5, Some("request-123"))
        .await;
    stop_server(server).await;

    let candidates = result.unwrap();
    assert_eq!(candidates.len(), 1);
    assert_eq!(
        candidates[0].source().provider_id().as_str(),
        "example_music"
    );
    assert_eq!(candidates[0].source().external_id(), "example-source-id");
    assert_eq!(candidates[0].title(), "Instant Crush");
    assert_eq!(candidates[0].artists(), ["Daft Punk"]);
    assert_eq!(candidates[0].duration_ms(), Some(337_000));

    let captured = captured.lock().unwrap().take().unwrap();
    assert_eq!(captured.body, json!({"query": "Instant Crush", "limit": 5}));
    assert_eq!(captured.request_id.as_deref(), Some("request-123"));
}

#[tokio::test]
async fn unavailable_resolver_becomes_provider_unavailable() {
    let router = Router::new().route("/v1/catalogue/search", post(unavailable));
    let (base_url, server) = spawn_server(router).await;
    let result = client(base_url).search("query", 1, None).await;
    stop_server(server).await;

    assert!(matches!(result, Err(ResolverError::ProviderUnavailable)));
}

#[tokio::test]
async fn malformed_success_response_becomes_request_error() {
    let router = Router::new().route("/v1/catalogue/search", post(malformed_response));
    let (base_url, server) = spawn_server(router).await;
    let result = client(base_url).search("query", 1, None).await;
    stop_server(server).await;

    assert!(matches!(result, Err(ResolverError::Request(_))));
}

#[tokio::test]
async fn invalid_candidate_data_becomes_validation_error() {
    let router = Router::new().route("/v1/catalogue/search", post(negative_duration));
    let (base_url, server) = spawn_server(router).await;
    let result = client(base_url).search("query", 1, None).await;
    stop_server(server).await;

    assert!(matches!(
        result,
        Err(ResolverError::InvalidResponse(
            ValidationError::InvalidDuration
        ))
    ));
}

#[tokio::test]
async fn invalid_limit_is_rejected_before_request() {
    let base_url = "http://127.0.0.1:1/".parse().unwrap();
    let result = client(base_url).search("query", 0, None).await;

    assert!(matches!(result, Err(ResolverError::InvalidLimit)));
}
