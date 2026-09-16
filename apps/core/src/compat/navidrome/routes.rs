use axum::{
    Extension, Json,
    extract::{Path, Query, State},
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
    query: Result<Query<SongListQuery>, axum::extract::rejection::QueryRejection>,
) -> Response {
    let Query(query) = match query {
        Ok(query) => query,
        Err(_) => {
            return errors::error(
                StatusCode::UNPROCESSABLE_ENTITY,
                "Invalid search parameters.",
            );
        }
    };
    match service::search_songs(
        state.catalogue(),
        state.credential(),
        &context,
        &token.0,
        &query,
    )
    .await
    {
        Ok(songs) => {
            let mut response = Json(&songs).into_response();
            response.headers_mut().insert(
                "x-total-count",
                songs
                    .len()
                    .to_string()
                    .parse()
                    .expect("a count is a valid header value"),
            );
            response
        }
        Err(error) => song_error(error, &context),
    }
}

pub(crate) async fn song_detail(
    State(state): State<AppState>,
    Extension(context): Extension<RequestContext>,
    token: NdToken,
    Path(id): Path<String>,
) -> Response {
    match service::song_detail(state.catalogue(), state.credential(), &id, &token.0).await {
        Ok(song) => Json(song).into_response(),
        Err(error) => song_error(error, &context),
    }
}

fn song_error(error: SongError, context: &RequestContext) -> Response {
    match error {
        SongError::Unauthorized => errors::error(StatusCode::UNAUTHORIZED, "Not authenticated"),
        SongError::NotFound => errors::error(StatusCode::NOT_FOUND, "Not found."),
        SongError::InvalidSearch => errors::error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "Invalid search parameters.",
        ),
        SongError::Authentication(error) => {
            tracing::error!(error = %error, request_id = context.request_id().as_str(), "session storage failed");
            errors::error(StatusCode::INTERNAL_SERVER_ERROR, "Internal error.")
        }
        SongError::Catalogue(error) => {
            tracing::error!(error = %error, request_id = context.request_id().as_str(), "catalogue search failed");
            errors::error(StatusCode::INTERNAL_SERVER_ERROR, "Internal error.")
        }
    }
}
