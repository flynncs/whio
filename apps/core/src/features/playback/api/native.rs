use std::sync::Arc;

use axum::{
    Extension,
    extract::{Path, State},
    http::{HeaderMap, header::RANGE},
    response::Response,
};

use crate::{
    features::identity::api::native::AuthenticatedPrincipal,
    features::music_library::domain::TrackId, features::playback::PlaybackError,
    features::playback::api::errors::ApiError, platform::request::RequestContext,
};

#[derive(Clone)]
pub(crate) struct NativeState(pub(crate) Arc<crate::features::playback::StreamingService>);

pub(crate) enum StreamFailure {
    InvalidId(uuid::Error),
    Stream(PlaybackError),
}

pub(crate) async fn stream_response(
    state: &NativeState,
    context: &RequestContext,
    raw_id: &str,
    headers: &HeaderMap,
    principal: &AuthenticatedPrincipal,
) -> Result<Response, StreamFailure> {
    use crate::features::playback::delivery::into_response;

    let track_id = raw_id
        .parse::<TrackId>()
        .map_err(StreamFailure::InvalidId)?;

    let user_uuid = principal.0.user_id.as_uuid();
    let client_range = headers
        .get(RANGE)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);

    state
        .0
        .stream(&track_id, Some(&user_uuid), context, client_range)
        .await
        .map(into_response)
        .map_err(StreamFailure::Stream)
}

pub(crate) async fn stream(
    State(state): State<NativeState>,
    Extension(context): Extension<RequestContext>,
    principal: AuthenticatedPrincipal,
    Path(track_id): Path<String>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    stream_response(&state, &context, &track_id, &headers, &principal)
        .await
        .map_err(|failure| match failure {
            StreamFailure::InvalidId(error) => ApiError::InvalidTrackId {
                context: context.clone(),
                error,
            },
            StreamFailure::Stream(error) => ApiError::PlaybackStream {
                context: context.clone(),
                error,
            },
        })
}
