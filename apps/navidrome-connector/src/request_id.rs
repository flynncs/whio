use axum::{extract::Request, middleware::Next, response::Response};
use uuid::Uuid;

#[derive(Clone)]
pub(crate) struct RequestId(pub(crate) String);

pub(crate) async fn middleware(mut request: Request, next: Next) -> Response {
    let request_id = request
        .headers()
        .get("x-request-id")
        .and_then(|value| value.to_str().ok())
        .filter(|value| !value.is_empty() && value.len() <= 128)
        .map(str::to_owned)
        .unwrap_or_else(|| Uuid::now_v7().to_string());

    request
        .extensions_mut()
        .insert(RequestId(request_id.clone()));
    let mut response = next.run(request).await;
    response.headers_mut().insert(
        "x-request-id",
        axum::http::HeaderValue::from_str(&request_id).expect("generated request ids are valid"),
    );
    response
}
