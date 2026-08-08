use super::middleware::RequestId;
use axum::{
    Json,
    http::StatusCode,
    response::{IntoResponse, Response as AxumResponse},
};
use serde::Serialize;

#[derive(Serialize)]
struct ApiErrorResponse {
    code: &'static str,
    message: &'static str,
    request_id: String,
}

pub(crate) enum ApiError {
    NotFound(RequestId),
}

impl IntoResponse for ApiError {
    fn into_response(self) -> AxumResponse {
        match self {
            ApiError::NotFound(request_id) => {
                let status = StatusCode::NOT_FOUND;
                let code = "not_found";
                let message = "Not found";
                let request_id = request_id.into_string();

                let body = ApiErrorResponse {
                    code,
                    message,
                    request_id,
                };

                (status, Json(body)).into_response()
            }
        }
    }
}
