use axum::{
    Json,
    http::StatusCode,
    response::{IntoResponse, Response},
};

use super::dto::NdError;

pub(crate) fn error(status: StatusCode, message: &str) -> Response {
    (
        status,
        Json(NdError {
            error: message.to_string(),
        }),
    )
        .into_response()
}

pub(crate) async fn not_found() -> Response {
    error(StatusCode::NOT_FOUND, "not found")
}
