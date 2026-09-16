use crate::playback::{DeliveryError, DeliveryResult, PlayableMedia};
use axum::{
    body::Body,
    http::{
        HeaderMap, HeaderValue, StatusCode,
        header::{ACCEPT_RANGES, CONTENT_LENGTH, CONTENT_RANGE, CONTENT_TYPE, RANGE},
    },
    response::Response,
};

pub fn media_client() -> reqwest::Client {
    crate::media::client(crate::media::same_origin_redirect_policy())
}

pub async fn relay(
    http: &reqwest::Client,
    media: &PlayableMedia,
    client_range: Option<String>,
) -> Result<DeliveryResult, DeliveryError> {
    let mut outgoing = http.get(media.url().as_url().clone());
    for (name, value) in media.headers() {
        outgoing = outgoing.header(name, value);
    }
    if let Some(range) = client_range {
        outgoing = outgoing.header(RANGE, range);
    }
    let upstream = outgoing
        .send()
        .await
        .map_err(|_| DeliveryError::UpstreamUnavailable)?;
    match upstream.status() {
        StatusCode::UNAUTHORIZED => return Err(DeliveryError::UpstreamUnauthorized),
        StatusCode::NOT_FOUND => return Err(DeliveryError::UpstreamNotFound),
        status if !status.is_success() && status != StatusCode::RANGE_NOT_SATISFIABLE => {
            return Err(DeliveryError::UpstreamUnavailable);
        }
        _ => {}
    }
    let status = upstream.status();
    let mut headers = HeaderMap::new();
    // Preserve the server's capabilities rather than advertising invented range support.
    for name in [ACCEPT_RANGES, CONTENT_TYPE, CONTENT_LENGTH, CONTENT_RANGE] {
        if let Some(value) = upstream.headers().get(&name) {
            headers.insert(name, value.clone());
        }
    }
    if status == StatusCode::PARTIAL_CONTENT {
        headers.insert(ACCEPT_RANGES, HeaderValue::from_static("bytes"));
    }
    Ok(DeliveryResult {
        status,
        headers,
        body: Body::from_stream(upstream.bytes_stream()),
    })
}

pub fn into_response(delivery: DeliveryResult) -> Response {
    let mut response = Response::new(delivery.body);
    *response.status_mut() = delivery.status;
    *response.headers_mut() = delivery.headers;
    response
}
