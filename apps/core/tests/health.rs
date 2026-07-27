use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use tower::ServiceExt;
use whio_core::router;

#[tokio::test]
async fn health_routes_return_their_status() {
    for (uri, expected_body) in [
        ("/health/live", r#"{"status":"ok"}"#),
        ("/health/ready", r#"{"status":"ready"}"#),
    ] {
        let response = router()
            .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK, "{uri}");

        let body = to_bytes(response.into_body(), 1024).await.unwrap();
        assert_eq!(body.as_ref(), expected_body.as_bytes(), "{uri}");
    }
}
