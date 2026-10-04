pub(crate) mod navidrome;
mod source_tags;
pub(crate) mod subsonic;

pub use source_tags::SourceTags;

use axum::{
    body::Body,
    http::{Method, Request, StatusCode, header},
    middleware::Next,
    response::{IntoResponse, Response},
};

pub(crate) async fn cors(request: Request<Body>, next: Next) -> Response {
    let mut response = if request.method() == Method::OPTIONS {
        StatusCode::NO_CONTENT.into_response()
    } else {
        next.run(request).await
    };
    let headers = response.headers_mut();
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_ORIGIN,
        header::HeaderValue::from_static("*"),
    );
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_METHODS,
        header::HeaderValue::from_static("GET, POST, PUT, DELETE, OPTIONS"),
    );
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_HEADERS,
        header::HeaderValue::from_static("*"),
    );
    headers.insert(
        header::ACCESS_CONTROL_EXPOSE_HEADERS,
        header::HeaderValue::from_static("x-total-count"),
    );
    headers.insert(
        header::ACCESS_CONTROL_MAX_AGE,
        header::HeaderValue::from_static("86400"),
    );
    response
}
