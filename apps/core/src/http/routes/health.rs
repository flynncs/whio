use axum::Json;
use serde::Serialize;

#[derive(Serialize)]
pub(crate) struct HealthResponse {
    status: &'static str,
}

pub(crate) async fn live() -> Json<HealthResponse> {
    Json(HealthResponse { status: "ok" })
}

pub(crate) async fn ready() -> Json<HealthResponse> {
    Json(HealthResponse { status: "ready" })
}
