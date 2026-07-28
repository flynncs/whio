use axum::{
    Extension, Json,
    extract::Request,
    http::{HeaderValue, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response as AxumResponse},
};
use serde::Serialize;
use tracing::Instrument;
use uuid::Uuid;

#[derive(Clone, Debug)]
pub(crate) struct RequestId(String);

pub(crate) async fn request_id(mut request: Request, next: Next) -> AxumResponse {
    let request_id = request
        .headers()
        .get("x-request-id")
        .and_then(|value| value.to_str().ok())
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| Uuid::now_v7().to_string());

    request
        .extensions_mut()
        .insert(RequestId(request_id.clone()));

    let span = tracing::info_span!("http_request", request_id = %request_id,);

    let mut response = next.run(request).instrument(span).await;

    let header_val = HeaderValue::from_str(&request_id).expect("request ID was already validated");

    response.headers_mut().insert("x-request-id", header_val);

    response
}

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
                let request_id = request_id.0;

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

pub(crate) async fn not_found(Extension(request_id): Extension<RequestId>) -> ApiError {
    ApiError::NotFound(request_id)
}
