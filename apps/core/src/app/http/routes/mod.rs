use crate::platform::{http_response::error_response, request::RequestContext};
use axum::{Extension, http::StatusCode, response::Response};

pub(crate) mod health;

pub(crate) async fn not_found(Extension(context): Extension<RequestContext>) -> Response {
    error_response(StatusCode::NOT_FOUND, "not_found", "Not found", context)
}
