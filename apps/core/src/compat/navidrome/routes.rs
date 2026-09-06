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
        state.credential().key(),
        &context,
        &token.0,
        query.title.as_deref(),
        query.start,
        query.end,
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
