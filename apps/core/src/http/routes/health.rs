use axum::{Json, extract::State, http::StatusCode, response::IntoResponse};
use serde::Serialize;

use crate::AppState;

#[derive(Serialize)]
pub(crate) struct HealthResponse {
    status: &'static str,
}

pub(crate) async fn live() -> Json<HealthResponse> {
    Json(HealthResponse { status: "ok" })
}

pub(crate) async fn ready(State(state): State<AppState>) -> impl IntoResponse {
    let Some(database) = state.database() else {
        return (StatusCode::OK, Json(HealthResponse { status: "ready" })).into_response();
    };

    match database.check().await {
        Ok(()) => (StatusCode::OK, Json(HealthResponse { status: "ready" })).into_response(),
        Err(error) => {
            tracing::error!(error = %error, "database readiness check failed");
            (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(HealthResponse {
                    status: "not_ready",
                }),
            )
                .into_response()
        }
    }
}
