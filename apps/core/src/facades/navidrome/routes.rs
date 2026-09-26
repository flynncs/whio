use axum::{
    Extension, Json,
    extract::{Path, Query, State},
    http::{HeaderValue, StatusCode},
    response::{IntoResponse, Response},
};

use crate::{
    facades::navidrome::state::NavidromeState, features::music_library::domain::AlbumListFilter,
    platform::raw_query::RawQuery, platform::request::RequestContext,
};

use super::{
    auth::NdToken,
    dto::SongListQuery,
    errors,
    service::{self, SongError},
};

pub(crate) async fn song_list(
    State(state): State<NavidromeState>,
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

    if let Some(album_id) = query.album_id.as_deref() {
        return match service::album_songs(
            state.completion(),
            state.credential(),
            &token.0,
            album_id,
            state.source_tags(),
        )
        .await
        {
            Ok(songs) => list_response(&songs, songs.len() as u32),
            Err(error) => song_error(error, &context),
        };
    }

    match service::song_list(
        state.library(),
        state.credential(),
        &token.0,
        &query,
        state.source_tags(),
    )
    .await
    {
        Ok((songs, total)) => list_response(&songs, total),
        Err(error) => song_error(error, &context),
    }
}

pub(crate) async fn song_detail(
    State(state): State<NavidromeState>,
    Extension(context): Extension<RequestContext>,
    token: NdToken,
    Path(id): Path<String>,
) -> Response {
    match service::song_detail(
        state.library(),
        state.credential(),
        &id,
        &token.0,
        state.source_tags(),
    )
    .await
    {
        Ok(song) => Json(song).into_response(),
        Err(error) => song_error(error, &context),
    }
}

pub(crate) async fn album_list(
    State(state): State<NavidromeState>,
    Extension(context): Extension<RequestContext>,
    token: NdToken,
    query: RawQuery,
) -> Response {
    let order = service::album_order(query.get("_sort"));
    let year = query
        .get("year")
        .and_then(|raw| raw.trim().parse::<i32>().ok());
    let mut filter = AlbumListFilter::new()
        .with_genre(query.get("genre").map(str::to_owned))
        .with_year(year, year);
    if query
        .get("starred")
        .is_some_and(|raw| matches!(raw.trim().to_ascii_lowercase().as_str(), "true" | "1"))
    {
        filter = filter.starred();
    }

    match service::album_list(
        state.library(),
        state.credential(),
        &token.0,
        order,
        filter,
        query.get("artist_id"),
        page_start(&query),
        page_end(&query),
    )
    .await
    {
        Ok((albums, total)) => list_response(&albums, total),
        Err(error) => song_error(error, &context),
    }
}

pub(crate) async fn album_detail(
    State(state): State<NavidromeState>,
    Extension(context): Extension<RequestContext>,
    token: NdToken,
    Path(id): Path<String>,
) -> Response {
    match service::album_detail(state.completion(), state.credential(), &token.0, &id).await {
        Ok(album) => Json(album).into_response(),
        Err(error) => song_error(error, &context),
    }
}

pub(crate) async fn artist_list(
    State(state): State<NavidromeState>,
    Extension(context): Extension<RequestContext>,
    token: NdToken,
    query: RawQuery,
) -> Response {
    match service::artist_list(
        state.library(),
        state.credential(),
        &token.0,
        page_start(&query),
        page_end(&query),
    )
    .await
    {
        Ok((artists, total)) => list_response(&artists, total),
        Err(error) => song_error(error, &context),
    }
}

pub(crate) async fn artist_detail(
    State(state): State<NavidromeState>,
    Extension(context): Extension<RequestContext>,
    token: NdToken,
    Path(id): Path<String>,
) -> Response {
    match service::artist_detail(state.library(), state.credential(), &token.0, &id).await {
        Ok(artist) => Json(artist).into_response(),
        Err(error) => song_error(error, &context),
    }
}

pub(crate) async fn genre_list(
    State(state): State<NavidromeState>,
    Extension(context): Extension<RequestContext>,
    token: NdToken,
) -> Response {
    match service::genre_list(state.library(), state.credential(), &token.0).await {
        Ok(genres) => Json(genres).into_response(),
        Err(error) => song_error(error, &context),
    }
}

fn page_start(query: &RawQuery) -> u32 {
    query
        .get("_start")
        .and_then(|raw| raw.trim().parse().ok())
        .unwrap_or(0)
}

fn page_end(query: &RawQuery) -> u32 {
    query
        .get("_end")
        .and_then(|raw| raw.trim().parse().ok())
        .unwrap_or(0)
}

fn list_response<T: serde::Serialize>(items: &[T], total: u32) -> Response {
    let mut response = Json(items).into_response();
    response.headers_mut().insert(
        "x-total-count",
        HeaderValue::from_str(&total.to_string()).expect("a count is a valid header value"),
    );
    response
}

pub(crate) async fn playlist_list(
    State(state): State<NavidromeState>,
    Extension(context): Extension<RequestContext>,
    token: NdToken,
) -> Response {
    match service::playlist_list(
        state.playlists(),
        state.credential(),
        &token.0,
        state.source_tags(),
    )
    .await
    {
        Ok(playlists) => Json(playlists).into_response(),
        Err(error) => song_error(error, &context),
    }
}

pub(crate) async fn playlist_detail(
    State(state): State<NavidromeState>,
    Extension(context): Extension<RequestContext>,
    token: NdToken,
    Path(id): Path<String>,
) -> Response {
    match service::playlist_detail(
        state.playlists(),
        state.credential(),
        &token.0,
        &id,
        state.source_tags(),
    )
    .await
    {
        Ok(playlist) => Json(playlist).into_response(),
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
            tracing::error!(error = %error, request_id = context.request_id().as_str(), "catalogue read failed");
            errors::error(StatusCode::INTERNAL_SERVER_ERROR, "Internal error.")
        }
        SongError::Playlist(error) => {
            tracing::error!(error = %error, request_id = context.request_id().as_str(), "playlist read failed");
            errors::error(StatusCode::INTERNAL_SERVER_ERROR, "Internal error.")
        }
    }
}
