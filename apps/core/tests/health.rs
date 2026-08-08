use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use tower::ServiceExt;
use uuid::Uuid;

mod support;

use support::app;

#[tokio::test]
async fn health_routes_return_their_status() {
    for (uri, expected_body) in [
        ("/health/live", r#"{"status":"ok"}"#),
        ("/health/ready", r#"{"status":"ready"}"#),
    ] {
        let response = app(vec![])
            .oneshot(
                Request::builder()
                    .uri(uri)
                    .header("x-request-id", "test-request-id")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK, "{uri}");
        assert_eq!(
            response.headers().get("x-request-id").unwrap(),
            "test-request-id",
            "{uri}"
        );

        let body = to_bytes(response.into_body(), 1024).await.unwrap();
        assert_eq!(body.as_ref(), expected_body.as_bytes(), "{uri}");
    }
}

#[tokio::test]
async fn missing_request_id_is_created() {
    let response = app(vec![])
        .oneshot(
            Request::builder()
                .uri("/health/live")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    let request_id = response
        .headers()
        .get("x-request-id")
        .unwrap()
        .to_str()
        .unwrap();

    assert!(Uuid::parse_str(request_id).is_ok());
}

#[tokio::test]
async fn invalid_request_id_is_replaced() {
    let response = app(vec![])
        .oneshot(
            Request::builder()
                .uri("/health/live")
                .header("x-request-id", "x".repeat(129))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    let request_id = response
        .headers()
        .get("x-request-id")
        .unwrap()
        .to_str()
        .unwrap();

    assert!(Uuid::parse_str(request_id).is_ok());
}

#[tokio::test]
async fn not_found_returns_a_safe_error() {
    let response = app(vec![])
        .oneshot(
            Request::builder()
                .uri("/missing")
                .header("x-request-id", "test-request-id")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    assert_eq!(
        response.headers().get("x-request-id").unwrap(),
        "test-request-id"
    );

    let body = to_bytes(response.into_body(), 1024).await.unwrap();
    assert_eq!(
        body.as_ref(),
        br#"{"code":"not_found","message":"Not found","request_id":"test-request-id"}"#
    );
}
