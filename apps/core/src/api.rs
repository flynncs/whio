use axum::{
    extract::Request, http::HeaderValue, middleware::Next, response::Response as AxumResponse,
};
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
