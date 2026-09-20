use axum::{
    Extension,
    extract::{Path, State},
    http::{HeaderMap, header::RANGE},
    response::Response,
};

use crate::{
    AppState,
    http::{auth::AuthenticatedPrincipal, errors::ApiError},
    playback::PlaybackError,
    request::RequestContext,
    tracks::TrackId,
};

pub(crate) enum StreamFailure {
    InvalidId(uuid::Error),
    Stream(PlaybackError),
}

pub(crate) async fn stream_response(
    state: &AppState,
    context: &RequestContext,
    raw_id: &str,
    headers: &HeaderMap,
    principal: &AuthenticatedPrincipal,
) -> Result<Response, StreamFailure> {
    use crate::delivery::into_response;

    let track_id = raw_id
        .parse::<TrackId>()
        .map_err(StreamFailure::InvalidId)?;

    let user_uuid = principal.0.user_id.as_uuid();
    let client_range = headers
        .get(RANGE)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);

    state
        .streaming()
        .stream(&track_id, Some(&user_uuid), context, client_range)
        .await
        .map(into_response)
        .map_err(StreamFailure::Stream)
}

pub(crate) async fn stream(
    State(state): State<AppState>,
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

pub(crate) async fn artwork(
    State(state): State<AppState>,
    Extension(context): Extension<RequestContext>,
    _principal: AuthenticatedPrincipal,
    Path(cover_id): Path<String>,
    headers: HeaderMap,
    query: crate::http::raw_query::RawQuery,
) -> Result<Response, ApiError> {
    use super::artwork::{ArtworkFailure, found_response, not_modified_response};
    use crate::artwork::ArtworkOutcome;
    use axum::http::header::IF_NONE_MATCH;

    let if_none_match = headers
        .get(IF_NONE_MATCH)
        .and_then(|value| value.to_str().ok());
    let size = super::artwork::requested_size(query.get("size"));

    match super::artwork::load(&state, &cover_id, if_none_match, size).await {
        Ok(ArtworkOutcome::Found(artwork)) => Ok(found_response(artwork)),
        Ok(ArtworkOutcome::NotModified(etag)) => Ok(not_modified_response(etag)),
        Ok(ArtworkOutcome::Missing) => Err(ApiError::NotFound(context)),
        Err(ArtworkFailure::InvalidId(error)) => Err(ApiError::InvalidTrackId { context, error }),
        Err(ArtworkFailure::Service(error)) => Err(ApiError::Artwork { context, error }),
    }
}
