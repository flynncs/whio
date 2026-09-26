use axum::{
    Extension, Json,
    extract::{Path, Query, State},
    http::{HeaderValue, StatusCode},
    response::{IntoResponse, Response},
};

use crate::{
    facades::navidrome::state::NavidromeState,
    features::music_library::domain::{
        AlbumListFilter, ArtistId, ArtistListFilter, IdSet, Range, RoleSelector, RoleSet,
        SelectorText,
    },
    features::music_library::library::{LibraryId, LibraryRegistry},
    platform::raw_query::RawQuery,
    platform::request::RequestContext,
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

const ALBUM_LIST_PARAMETERS: &[&str] = &[
    "_start",
    "_end",
    "_sort",
    "_order",
    "artist_id",
    "genre_id",
    "compilation",
    "has_rating",
    "recently_played",
    "starred",
    "name",
    "year",
    "library_id",
    "missing",
];

const ARTIST_LIST_PARAMETERS: &[&str] = &[
    "_start",
    "_end",
    "_sort",
    "_order",
    "library_id",
    "name",
    "starred",
    "role",
    "missing",
];

fn reject_unknown(query: &RawQuery, allowed: &[&str]) -> Result<(), SongError> {
    for name in query.names() {
        if !allowed.contains(&name) {
            return Err(SongError::InvalidSearch);
        }
    }

    Ok(())
}

fn elements<'a>(query: &'a RawQuery, name: &str) -> Vec<&'a str> {
    query
        .all(name)
        .into_iter()
        .flat_map(|raw| raw.split(','))
        .collect()
}

fn id_set<Id: Clone + Eq + std::hash::Hash>(
    query: &RawQuery,
    name: &str,
    parse: impl Fn(&str) -> Option<Id>,
) -> Result<Option<IdSet<Id>>, SongError> {
    if query.get(name).is_none() {
        return Ok(None);
    }
    let mut ids = Vec::new();

    for element in elements(query, name) {
        ids.push(parse(element).ok_or(SongError::InvalidSearch)?);
    }

    IdSet::new(ids)
        .map(Some)
        .map_err(|_| SongError::InvalidSearch)
}

fn number_set(query: &RawQuery, name: &str) -> Result<Option<Vec<i32>>, SongError> {
    if query.get(name).is_none() {
        return Ok(None);
    }
    let mut numbers = Vec::new();

    for element in elements(query, name) {
        numbers.push(
            element
                .trim()
                .parse::<i32>()
                .map_err(|_| SongError::InvalidSearch)?,
        );
    }

    Ok(Some(numbers))
}

fn selector_text(query: &RawQuery, name: &str) -> Result<Option<SelectorText>, SongError> {
    match query.get(name) {
        Some(raw) => SelectorText::new(raw).map_err(|_| SongError::InvalidSearch),
        None => Ok(None),
    }
}

fn flag(query: &RawQuery, name: &str) -> Result<Option<bool>, SongError> {
    match query.get(name) {
        Some("true") => Ok(Some(true)),
        Some("false") => Ok(Some(false)),
        Some(_) => Err(SongError::InvalidSearch),
        None => Ok(None),
    }
}

fn role_set(query: &RawQuery, name: &str) -> Result<Option<RoleSet>, SongError> {
    match query.get(name) {
        Some(raw) if raw.trim().is_empty() => Ok(None),
        Some(_) => {
            let mut roles = Vec::new();

            for element in elements(query, name) {
                roles.push(RoleSelector::parse(element).ok_or(SongError::InvalidSearch)?);
            }

            RoleSet::new(roles)
                .map(Some)
                .map_err(|_| SongError::InvalidSearch)
        }
        None => Ok(None),
    }
}

fn single_year(query: &RawQuery) -> Result<Option<Range<i32>>, SongError> {
    match query.get("year") {
        Some(raw) => {
            let year = raw
                .trim()
                .parse::<i32>()
                .map_err(|_| SongError::InvalidSearch)?;
            Range::new(Some(year), Some(year))
                .map(Some)
                .map_err(|_| SongError::InvalidSearch)
        }
        None => Ok(None),
    }
}

async fn library_id(
    libraries: &LibraryRegistry,
    query: &RawQuery,
) -> Result<Option<IdSet<LibraryId>>, SongError> {
    match number_set(query, "library_id")? {
        Some(requested) => service::canonical_libraries(libraries, &requested)
            .await
            .map(Some),
        None => Ok(None),
    }
}

pub(crate) async fn album_list(
    State(state): State<NavidromeState>,
    Extension(context): Extension<RequestContext>,
    token: NdToken,
    query: RawQuery,
) -> Response {
    let filter = match album_filter(&state, &query).await {
        Ok(filter) => filter,
        Err(error) => return song_error(error, &context),
    };
    let order = match service::album_order(query.get("_sort")) {
        Ok(order) => order,
        Err(error) => return song_error(error, &context),
    };
    let (start, end) = match window(&query) {
        Ok(window) => window,
        Err(error) => return song_error(error, &context),
    };

    match service::album_list(
        state.library(),
        state.credential(),
        &token.0,
        order,
        filter,
        start,
        end,
    )
    .await
    {
        Ok((albums, total)) => list_response(&albums, total),
        Err(error) => song_error(error, &context),
    }
}

async fn album_filter(
    state: &NavidromeState,
    query: &RawQuery,
) -> Result<AlbumListFilter, SongError> {
    reject_unknown(query, ALBUM_LIST_PARAMETERS)?;
    let direction = service::sort_direction(query.get("_order"))?;
    let artist_id = id_set(query, "artist_id", |raw| raw.parse::<ArtistId>().ok())?;
    let genre_id = id_set(query, "genre_id", |raw| {
        SelectorText::new(raw).ok().flatten()
    })?;
    let library_id = library_id(state.libraries(), query).await?;

    Ok(AlbumListFilter::new()
        .with_sort_direction(direction)
        .with_artist_id(artist_id)
        .with_genre(genre_id)
        .with_compilation(flag(query, "compilation")?)
        .with_has_rating(flag(query, "has_rating")?)
        .with_recently_played(flag(query, "recently_played")?)
        .with_starred(flag(query, "starred")?)
        .with_missing(flag(query, "missing")?)
        .with_name(selector_text(query, "name")?)
        .with_year(single_year(query)?)
        .with_library_id(library_id))
}

async fn artist_filter(
    state: &NavidromeState,
    query: &RawQuery,
) -> Result<ArtistListFilter, SongError> {
    reject_unknown(query, ARTIST_LIST_PARAMETERS)?;
    let direction = service::sort_direction(query.get("_order"))?;
    let library_id = library_id(state.libraries(), query).await?;

    Ok(ArtistListFilter::new()
        .with_sort_direction(direction)
        .with_name(selector_text(query, "name")?)
        .with_starred(flag(query, "starred")?)
        .with_role(role_set(query, "role")?)
        .with_missing(flag(query, "missing")?)
        .with_library_id(library_id))
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
    let filter = match artist_filter(&state, &query).await {
        Ok(filter) => filter,
        Err(error) => return song_error(error, &context),
    };
    let sort = match service::artist_sort(query.get("_sort")) {
        Ok(sort) => sort,
        Err(error) => return song_error(error, &context),
    };
    let (start, end) = match window(&query) {
        Ok(window) => window,
        Err(error) => return song_error(error, &context),
    };

    match service::artist_list(
        state.library(),
        state.credential(),
        &token.0,
        sort,
        filter,
        start,
        end,
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

fn window(query: &RawQuery) -> Result<(u32, u32), SongError> {
    Ok((page_start(query)?, page_end(query)?))
}

fn page_start(query: &RawQuery) -> Result<u32, SongError> {
    match query.get("_start") {
        Some(raw) => raw.trim().parse().map_err(|_| SongError::InvalidSearch),
        None => Ok(0),
    }
}

fn page_end(query: &RawQuery) -> Result<u32, SongError> {
    match query.get("_end") {
        Some(raw) => match raw.trim().parse::<i64>() {
            Ok(end) if end < 0 => Ok(0),
            Ok(end) => u32::try_from(end).map_err(|_| SongError::InvalidSearch),
            Err(_) => Err(SongError::InvalidSearch),
        },
        None => Ok(0),
    }
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
        SongError::Registry(error) => {
            tracing::error!(error = %error, request_id = context.request_id().as_str(), "library registry read failed");
            errors::error(StatusCode::INTERNAL_SERVER_ERROR, "Internal error.")
        }
    }
}
