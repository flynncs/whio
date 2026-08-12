use std::collections::HashMap;

use axum::{
    Extension, Json,
    body::Body,
    extract::{Path, State, rejection::JsonRejection},
    http::{
        HeaderMap, HeaderValue, StatusCode,
        header::{ACCEPT_RANGES, CONTENT_LENGTH, CONTENT_RANGE, RANGE},
    },
    response::Response,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::{
    AppState,
    http::{
        errors::ApiError,
        range::{RangeError, RangeRequest, parse_range_header},
    },
    media::ByteRange,
    playback::{MediaMetadata, PlayableMedia},
    request::RequestContext,
    tracks::TrackId,
};

#[derive(Deserialize)]
pub(crate) struct PlaybackRequest {
    track_id: String,
}

#[derive(Serialize)]
pub(crate) struct PlaybackResponse {
    url: String,
    headers: HashMap<String, String>,
    expires_at: Option<DateTime<Utc>>,
    media: MediaResponse,
}

#[derive(Serialize)]
pub(crate) struct MediaResponse {
    content_type: Option<String>,
    content_length_bytes: Option<u64>,
    codec: Option<String>,
    bitrate_kbps: Option<f64>,
    duration_ms: Option<u64>,
}

impl From<PlayableMedia> for PlaybackResponse {
    fn from(value: PlayableMedia) -> Self {
        let (url, headers, expires_at, metadata) = value.into_parts();

        Self {
            url: url.as_url().to_string(),
            headers,
            expires_at,
            media: metadata.into(),
        }
    }
}

impl From<MediaMetadata> for MediaResponse {
    fn from(value: MediaMetadata) -> Self {
        Self {
            content_type: value.content_type().map(str::to_owned),
            content_length_bytes: value.content_length_bytes(),
            codec: value.codec().map(str::to_owned),
            bitrate_kbps: value.bitrate_kbps(),
            duration_ms: value.duration_ms(),
        }
    }
}

pub(crate) async fn resolve(
    State(state): State<AppState>,
    Extension(context): Extension<RequestContext>,
    payload: Result<Json<PlaybackRequest>, JsonRejection>,
) -> Result<Json<PlaybackResponse>, ApiError> {
    let Json(request) = payload.map_err(|error| ApiError::MalformedRequest {
        context: context.clone(),
        error,
    })?;

    let track_id =
        request
            .track_id
            .parse::<TrackId>()
            .map_err(|error| ApiError::InvalidTrackId {
                context: context.clone(),
                error,
            })?;

    let playable_media = state
        .playback()
        .resolve(&track_id, &context)
        .await
        .map_err(|error| ApiError::Playback {
            context: context.clone(),
            error,
        })?;

    Ok(Json(playable_media.into()))
}

pub(crate) async fn stream(
    State(state): State<AppState>,
    Extension(context): Extension<RequestContext>,
    Path(track_id): Path<String>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let track_id = track_id
        .parse::<TrackId>()
        .map_err(|err| ApiError::InvalidTrackId {
            context: context.clone(),
            error: err,
        })?;

    let prepared_playback = state
        .playback_stream()
        .prepare(&track_id, &context)
        .await
        .map_err(|error| ApiError::PlaybackStream {
            context: context.clone(),
            error,
        })?;

    let content_length = prepared_playback.info.content_length;

    let header_range = headers
        .get(RANGE)
        .map(|value| value.to_str())
        .transpose()
        .map_err(|_| ApiError::Range {
            context: context.clone(),
            content_length,
            error: RangeError::Malformed,
        })?;

    let range_request =
        parse_range_header(header_range, content_length).map_err(|error| ApiError::Range {
            context: context.clone(),
            content_length,
            error,
        })?;

    let range = match &range_request {
        RangeRequest::Full => ByteRange {
            start: 0,
            end: content_length - 1,
        },
        RangeRequest::Partial(byte_range) => byte_range.clone(),
    };

    let fetched = state
        .playback_stream()
        .fetch_range(&prepared_playback, &range)
        .await
        .map_err(|error| ApiError::PlaybackStream {
            context: context.clone(),
            error,
        })?;

    let status = match range_request {
        RangeRequest::Full => StatusCode::OK,
        RangeRequest::Partial(_) => StatusCode::PARTIAL_CONTENT,
    };

    let fetched_length = fetched.bytes.len();
    let mut response = Response::new(Body::from(fetched.bytes));

    *response.status_mut() = status;

    let response_headers = response.headers_mut();
    response_headers.insert(ACCEPT_RANGES, HeaderValue::from_static("bytes"));
    response_headers.insert(
        CONTENT_LENGTH,
        HeaderValue::from_str(&fetched_length.to_string())
            .expect("a byte length is a valid header value"),
    );

    if status == StatusCode::PARTIAL_CONTENT {
        response_headers.insert(
            CONTENT_RANGE,
            HeaderValue::from_str(&format!(
                "bytes {}-{}/{}",
                fetched.range.start(),
                fetched.range.end(),
                content_length,
            ))
            .expect("a validated byte range is a valid header value"),
        );
    }

    Ok(response)
}
