use axum::{
    Extension, Json,
    extract::{Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};

use crate::{AppState, request::RequestContext};

use super::{
    auth::NdToken,
    dto::SongListQuery,
    errors,
    service::{self, SongError},
};

pub(crate) async fn song_list(
    State(state): State<AppState>,
    Extension(context): Extension<RequestContext>,
    token: NdToken,
    Query(query): Query<SongListQuery>,
) -> Response {
    match service::search_songs(
        state.catalogue(),
        state.credential().key(),
        &context,
        &token.0,
        query.filter.as_deref(),
        query.range.as_deref(),
    )
    .await
    {
        Ok(songs) => Json(songs).into_response(),
        Err(SongError::Unauthorized) => {
            errors::error(StatusCode::UNAUTHORIZED, "Not authenticated")
        }
        Err(SongError::InvalidSearch) => errors::error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "Invalid search parameters.",
        ),
        Err(SongError::Catalogue(error)) => {
            tracing::error!(error = %error, request_id = context.request_id().as_str(), "catalogue search failed");
            errors::error(StatusCode::INTERNAL_SERVER_ERROR, "Internal error.")
        }
    }
}
