use axum::{
    Json,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde::Serialize;

use super::request::RequestContext;

#[derive(Serialize)]
struct ErrorBody {
    code: &'static str,
    message: &'static str,
    request_id: String,
}

pub(crate) fn error_response(
    status: StatusCode,
    code: &'static str,
    message: &'static str,
    context: RequestContext,
) -> Response {
    (
        status,
        Json(ErrorBody {
            code,
            message,
            request_id: context.request_id().as_str().to_owned(),
        }),
    )
        .into_response()
}

pub(crate) fn invalid_request_response(context: RequestContext) -> Response {
    error_response(
        StatusCode::BAD_REQUEST,
        "invalid_request",
        "The request is invalid",
        context,
    )
}
