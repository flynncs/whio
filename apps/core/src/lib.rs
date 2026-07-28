use axum::{Json, Router, routing::get};
use serde::Serialize;

mod api;

#[derive(Serialize)]
struct HealthResponse {
    status: &'static str,
}

async fn live() -> Json<HealthResponse> {
    Json(HealthResponse { status: "ok" })
}

async fn ready() -> Json<HealthResponse> {
    Json(HealthResponse { status: "ready" })
}

pub fn router() -> Router {
    Router::new()
        .route("/health/live", get(live))
        .route("/health/ready", get(ready))
        .fallback(api::not_found)
        .layer(axum::middleware::from_fn(api::request_id))
}
