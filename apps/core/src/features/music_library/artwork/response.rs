use axum::{
    body::Body,
    http::{
        HeaderMap, HeaderValue, StatusCode,
        header::{CACHE_CONTROL, CONTENT_TYPE, ETAG},
    },
    response::{IntoResponse, Response},
};

use super::{Artwork, thumbnail};

const CACHE_CONTROL_VALUE: &str = "public, max-age=86400";

pub(crate) fn requested_size(raw: Option<&str>) -> Option<u32> {
    raw?.trim()
        .parse::<u32>()
        .ok()
        .map(|size| size.clamp(thumbnail::MIN_SIZE, thumbnail::MAX_SIZE))
}

pub(crate) fn found_response(artwork: Artwork) -> Response {
    let (content_type, bytes, etag) = artwork.into_parts();
    let mut headers = validators(&etag);
    headers.insert(
        CONTENT_TYPE,
        HeaderValue::from_str(&content_type)
            .unwrap_or(HeaderValue::from_static("application/octet-stream")),
    );
    (headers, Body::from(bytes)).into_response()
}

pub(crate) fn not_modified_response(etag: String) -> Response {
    (StatusCode::NOT_MODIFIED, validators(&etag)).into_response()
}

fn validators(etag: &str) -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(
        ETAG,
        HeaderValue::from_str(etag).expect("an etag is a valid header value"),
    );
    headers.insert(CACHE_CONTROL, HeaderValue::from_static(CACHE_CONTROL_VALUE));
    headers
}
