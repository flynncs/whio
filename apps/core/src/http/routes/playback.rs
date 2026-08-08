use std::collections::HashMap;

use axum::{
    Extension, Json,
    extract::{State, rejection::JsonRejection},
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::{
    AppState,
    http::errors::ApiError,
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
