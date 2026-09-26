mod auth;
mod envelope;
mod mapping;
mod query_auth;
mod service;
pub(crate) mod state;

use axum::{
    Extension, Json, Router,
    extract::{FromRef, OriginalUri, State},
    http::{HeaderMap, HeaderValue, StatusCode, header::CONTENT_RANGE},
    response::{IntoResponse, Response},
    routing::get,
};
use whio_subsonic_api::models;

use crate::{
    features::identity::{domain::Principal, service::AuthError},
    features::music_library::domain::{RandomSeed, Resource, TrackId},
    features::music_library::library::{ScanMode, ScanRequestError},
    features::music_library::playlists::{PlaylistEdit, PlaylistError},
    features::playback::{DeliveryError, PlaybackError},
    platform::http::FetchError,
    platform::raw_query::RawQuery,
    platform::request::RequestContext,
};
pub(crate) use auth::SubsonicAuth;
use auth::SubsonicError;
use envelope::{
    album_envelope, album_list2_envelope, artist_envelope, artists_envelope,
    create_playlist_envelope, directory_envelope, extensions_envelope, failed_envelope,
    genres_envelope, indexes_envelope, ok_envelope, playlist_envelope, playlists_envelope,
    random_songs_envelope, scan_status_envelope, search3_envelope, songs_by_genre_envelope,
    starred2_envelope, start_scan_envelope, user_envelope,
};
use state::SubsonicState;

pub(crate) fn router<S>() -> Router<S>
where
    S: Clone + Send + Sync + 'static,
    SubsonicState: FromRef<S>,
{
    [
        ("/ping", get(ping).post(ping)),
        ("/getUser", get(get_user).post(get_user)),
        ("/search3", get(search3).post(search3)),
        (
            "/getSongsByGenre",
            get(get_songs_by_genre).post(get_songs_by_genre),
        ),
        (
            "/getRandomSongs",
            get(get_random_songs).post(get_random_songs),
        ),
        ("/getIndexes", get(get_indexes).post(get_indexes)),
        (
            "/getMusicDirectory",
            get(get_music_directory).post(get_music_directory),
        ),
        ("/getAlbumList2", get(get_album_list2)),
        ("/getAlbum", get(get_album)),
        ("/getArtists", get(get_artists)),
        ("/getArtist", get(get_artist)),
        ("/getStarred2", get(get_starred2).post(get_starred2)),
        ("/getGenres", get(get_genres).post(get_genres)),
        ("/getPlaylists", get(get_playlists).post(get_playlists)),
        ("/getPlaylist", get(get_playlist).post(get_playlist)),
        (
            "/createPlaylist",
            get(create_playlist).post(create_playlist),
        ),
        (
            "/updatePlaylist",
            get(update_playlist).post(update_playlist),
        ),
        (
            "/deletePlaylist",
            get(delete_playlist).post(delete_playlist),
        ),
        ("/getScanStatus", get(get_scan_status).post(get_scan_status)),
        ("/startScan", get(start_scan).post(start_scan)),
        ("/star", get(star).post(star)),
        ("/unstar", get(unstar).post(unstar)),
        ("/setRating", get(set_rating).post(set_rating)),
        ("/scrobble", get(scrobble).post(scrobble)),
        ("/stream", get(stream)),
        ("/getCoverArt", get(get_cover_art)),
        (
            "/getOpenSubsonicExtensions",
            get(get_open_subsonic_extensions).post(get_open_subsonic_extensions),
        ),
    ]
    .into_iter()
    .fold(Router::new(), |router, (path, methods)| {
        router
            .route(&format!("{path}.view"), methods.clone())
            .route(path, methods)
    })
    .fallback(unsupported)
}

// grammar failures never reach these handlers: the SubsonicAuth rejection is
// already the spec-shaped failed envelope

pub(crate) async fn ping(
    State(state): State<SubsonicState>,
    Extension(context): Extension<RequestContext>,
    auth: SubsonicAuth,
) -> Response {
    match authenticate(&state, &context, auth).await {
        Ok(_) => Json(ok_envelope()).into_response(),
        Err(response) => response,
    }
}

pub(crate) async fn get_user(
    State(state): State<SubsonicState>,
    Extension(context): Extension<RequestContext>,
    auth: SubsonicAuth,
    query: RawQuery,
) -> Response {
    let principal = match authenticate(&state, &context, auth).await {
        Ok(principal) => principal,
        Err(response) => return response,
    };
    let Some(name) = query.get("username") else {
        return SubsonicError::MissingParameter.into_response();
    };
    match state.credential().visible_user(&principal, name).await {
        Ok(Some(user)) => Json(user_envelope(&user)).into_response(),
        Ok(None) => Json(failed_envelope(
            models::error::Code::Variant70,
            "User not found.",
        ))
        .into_response(),
        Err(error) => {
            tracing::error!(
                error = %error,
                request_id = context.request_id().as_str(),
                "credential storage failed"
            );

            let mut response = Json(failed_envelope(
                models::error::Code::Variant0,
                "Internal error.",
            ))
            .into_response();
            *response.status_mut() = StatusCode::INTERNAL_SERVER_ERROR;
            response
        }
    }
}

pub(crate) async fn search3(
    State(state): State<SubsonicState>,
    Extension(context): Extension<RequestContext>,
    auth: SubsonicAuth,
    query: RawQuery,
) -> Response {
    let principal = match authenticate(&state, &context, auth).await {
        Ok(principal) => principal,
        Err(response) => return response,
    };
    let Some(q) = query.get("query") else {
        return SubsonicError::MissingParameter.into_response();
    };
    let user_uuid = principal.user_id.as_uuid();
    let result = if q.is_empty() {
        match enumeration_request(&query) {
            Ok(request) => {
                service::enumerate(
                    state.library(),
                    Some(&user_uuid),
                    &request,
                    state.source_tags(),
                )
                .await
            }
            Err(error) => Err(error),
        }
    } else {
        service::search(
            state.search(),
            &context,
            Some(&user_uuid),
            q,
            query.get("songCount"),
            query.get("songOffset"),
            state.source_tags(),
        )
        .await
    };
    match result {
        Ok(result) => Json(search3_envelope(result)).into_response(),
        Err(service::SearchError::InvalidSearch) => {
            let mut response = Json(failed_envelope(
                models::error::Code::Variant0,
                "Invalid search parameters.",
            ))
            .into_response();
            *response.status_mut() = StatusCode::UNPROCESSABLE_ENTITY;
            response
        }
        Err(service::SearchError::Catalogue(error)) => {
            tracing::error!(
                error = %error,
                request_id = context.request_id().as_str(),
                "catalogue search failed"
            );

            let mut response = Json(failed_envelope(
                models::error::Code::Variant0,
                "Internal error.",
            ))
            .into_response();
            *response.status_mut() = StatusCode::INTERNAL_SERVER_ERROR;
            response
        }
    }
}

fn enumeration_request(
    query: &RawQuery,
) -> Result<service::EnumerationRequest, service::SearchError> {
    Ok(service::EnumerationRequest {
        songs: enumeration_window(query.get("songCount"), query.get("songOffset"))?,
        albums: enumeration_window(query.get("albumCount"), query.get("albumOffset"))?,
        artists: enumeration_window(query.get("artistCount"), query.get("artistOffset"))?,
    })
}

fn enumeration_window(
    raw_count: Option<&str>,
    raw_offset: Option<&str>,
) -> Result<service::EnumerationWindow, service::SearchError> {
    const DEFAULT_COUNT: u32 = 20;
    let count = match raw_count {
        None => DEFAULT_COUNT,
        Some(raw) => raw
            .trim()
            .parse::<u32>()
            .map_err(|_| service::SearchError::InvalidSearch)?,
    };
    let offset = match raw_offset {
        None => 0,
        Some(raw) => raw
            .trim()
            .parse::<u32>()
            .map_err(|_| service::SearchError::InvalidSearch)?,
    };

    service::EnumerationWindow::new(count, offset)
}

pub(crate) async fn get_album_list2(
    State(state): State<SubsonicState>,
    Extension(context): Extension<RequestContext>,
    auth: SubsonicAuth,
    query: RawQuery,
) -> Response {
    let principal = match authenticate(&state, &context, auth).await {
        Ok(principal) => principal,
        Err(response) => return response,
    };
    let Some(list_type) = query.get("type") else {
        return SubsonicError::MissingParameter.into_response();
    };
    let user = principal.user_id.as_uuid();
    let request = service::AlbumListRequest {
        list_type,
        size: query.get("size"),
        offset: query.get("offset"),
        from_year: query.get("fromYear"),
        to_year: query.get("toYear"),
        genre: query.get("genre"),
    };

    match service::album_list(state.library(), &request, Some(&user)).await {
        Ok(albums) => Json(album_list2_envelope(albums)).into_response(),
        Err(error) => browse_failure_response(&context, error),
    }
}

pub(crate) async fn get_album(
    State(state): State<SubsonicState>,
    Extension(context): Extension<RequestContext>,
    auth: SubsonicAuth,
    query: RawQuery,
) -> Response {
    let principal = match authenticate(&state, &context, auth).await {
        Ok(principal) => principal,
        Err(response) => return response,
    };
    let Some(id) = query.get("id") else {
        return SubsonicError::MissingParameter.into_response();
    };
    let user = principal.user_id.as_uuid();

    match service::album_detail(state.completion(), id, Some(&user), state.source_tags()).await {
        Ok(album) => Json(album_envelope(album)).into_response(),
        Err(error) => browse_failure_response(&context, error),
    }
}

pub(crate) async fn get_artists(
    State(state): State<SubsonicState>,
    Extension(context): Extension<RequestContext>,
    auth: SubsonicAuth,
) -> Response {
    let principal = match authenticate(&state, &context, auth).await {
        Ok(principal) => principal,
        Err(response) => return response,
    };
    let user = principal.user_id.as_uuid();

    match service::artists(state.library(), Some(&user)).await {
        Ok(artists) => Json(artists_envelope(service::artist_indexes(artists))).into_response(),
        Err(error) => browse_failure_response(&context, error),
    }
}

pub(crate) async fn get_artist(
    State(state): State<SubsonicState>,
    Extension(context): Extension<RequestContext>,
    auth: SubsonicAuth,
    query: RawQuery,
) -> Response {
    let principal = match authenticate(&state, &context, auth).await {
        Ok(principal) => principal,
        Err(response) => return response,
    };
    let Some(id) = query.get("id") else {
        return SubsonicError::MissingParameter.into_response();
    };
    let user = principal.user_id.as_uuid();

    match service::artist_detail(state.library(), id, Some(&user)).await {
        Ok(artist) => Json(artist_envelope(artist)).into_response(),
        Err(error) => browse_failure_response(&context, error),
    }
}

pub(crate) async fn get_songs_by_genre(
    State(state): State<SubsonicState>,
    Extension(context): Extension<RequestContext>,
    auth: SubsonicAuth,
    query: RawQuery,
) -> Response {
    let principal = match authenticate(&state, &context, auth).await {
        Ok(principal) => principal,
        Err(response) => return response,
    };
    let user = principal.user_id.as_uuid();
    let request = service::SongsByGenreRequest {
        genre: query.get("genre"),
        count: query.get("count"),
        offset: query.get("offset"),
    };

    match service::songs_by_genre(state.library(), Some(&user), &request, state.source_tags()).await
    {
        Ok(songs) => Json(songs_by_genre_envelope(songs)).into_response(),
        Err(error) => browse_failure_response(&context, error),
    }
}

pub(crate) async fn get_random_songs(
    State(state): State<SubsonicState>,
    Extension(context): Extension<RequestContext>,
    auth: SubsonicAuth,
    query: RawQuery,
) -> Response {
    let principal = match authenticate(&state, &context, auth).await {
        Ok(principal) => principal,
        Err(response) => return response,
    };
    let user = principal.user_id.as_uuid();
    let request = service::RandomSongsRequest {
        size: query.get("size"),
        genre: query.get("genre"),
        from_year: query.get("fromYear"),
        to_year: query.get("toYear"),
    };
    let seed =
        RandomSeed::for_request(Some(user), Resource::Track, chrono::Utc::now().date_naive());

    match service::random_songs(
        state.library(),
        Some(&user),
        &request,
        seed,
        state.source_tags(),
    )
    .await
    {
        Ok(songs) => Json(random_songs_envelope(songs)).into_response(),
        Err(error) => browse_failure_response(&context, error),
    }
}

pub(crate) async fn get_indexes(
    State(state): State<SubsonicState>,
    Extension(context): Extension<RequestContext>,
    auth: SubsonicAuth,
) -> Response {
    let principal = match authenticate(&state, &context, auth).await {
        Ok(principal) => principal,
        Err(response) => return response,
    };
    let user = principal.user_id.as_uuid();

    match service::indexes(state.library(), state.scanner(), Some(&user)).await {
        Ok(indexes) => Json(indexes_envelope(indexes)).into_response(),
        Err(error) => browse_failure_response(&context, error),
    }
}

pub(crate) async fn get_music_directory(
    State(state): State<SubsonicState>,
    Extension(context): Extension<RequestContext>,
    auth: SubsonicAuth,
    query: RawQuery,
) -> Response {
    let principal = match authenticate(&state, &context, auth).await {
        Ok(principal) => principal,
        Err(response) => return response,
    };
    let Some(id) = query.get("id") else {
        return SubsonicError::MissingParameter.into_response();
    };
    let user = principal.user_id.as_uuid();

    match service::music_directory(state.library(), id, Some(&user), state.source_tags()).await {
        Ok(directory) => Json(directory_envelope(directory)).into_response(),
        Err(error) => browse_failure_response(&context, error),
    }
}

fn browse_failure_response(context: &RequestContext, error: service::BrowseError) -> Response {
    let (status, code, message) = match error {
        service::BrowseError::InvalidRequest => (
            StatusCode::UNPROCESSABLE_ENTITY,
            models::error::Code::Variant0,
            "Invalid browse parameters.",
        ),
        service::BrowseError::MissingParameter => (
            StatusCode::BAD_REQUEST,
            models::error::Code::Variant10,
            "Required parameter is missing.",
        ),
        service::BrowseError::UnsupportedType => (
            StatusCode::BAD_REQUEST,
            models::error::Code::Variant10,
            "Unsupported album list type.",
        ),
        service::BrowseError::AlbumNotFound | service::BrowseError::ArtistNotFound => (
            StatusCode::NOT_FOUND,
            models::error::Code::Variant70,
            "Not found.",
        ),
        service::BrowseError::Catalogue(error) => {
            tracing::error!(
                error = %error,
                request_id = context.request_id().as_str(),
                "catalogue read failed"
            );
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                models::error::Code::Variant0,
                "Internal error.",
            )
        }
    };

    let mut response = Json(failed_envelope(code, message)).into_response();
    *response.status_mut() = status;
    response
}

pub(crate) async fn stream(
    State(state): State<SubsonicState>,
    Extension(context): Extension<RequestContext>,
    auth: SubsonicAuth,
    query: RawQuery,
    headers: HeaderMap,
) -> Response {
    use crate::{
        features::music_library::domain::TrackId, features::playback::delivery::into_response,
    };

    let principal = match authenticate(&state, &context, auth).await {
        Ok(principal) => principal,
        Err(response) => return response,
    };
    let Some(id) = query.get("id") else {
        return SubsonicError::MissingParameter.into_response();
    };
    let Ok(track_id) = id.parse::<TrackId>() else {
        let mut response = Json(failed_envelope(
            models::error::Code::Variant0,
            "Invalid track ID.",
        ))
        .into_response();
        *response.status_mut() = StatusCode::BAD_REQUEST;
        return response;
    };

    let user_uuid = principal.user_id.as_uuid();
    let client_range = headers
        .get(axum::http::header::RANGE)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);

    match state
        .streaming()
        .stream(&track_id, Some(&user_uuid), &context, client_range)
        .await
    {
        Ok(delivery) => into_response(delivery),
        Err(error) => stream_failure_response(&context, error),
    }
}

pub(crate) async fn get_cover_art(
    State(state): State<SubsonicState>,
    Extension(context): Extension<RequestContext>,
    auth: SubsonicAuth,
    query: RawQuery,
    headers: HeaderMap,
) -> Response {
    use crate::features::music_library::artwork::{ArtworkOutcome, CoverArtTarget, response};
    use axum::http::header::IF_NONE_MATCH;

    if let Err(response) = authenticate(&state, &context, auth).await {
        return response;
    }
    let Some(id) = query.get("id") else {
        return SubsonicError::MissingParameter.into_response();
    };
    let target = match CoverArtTarget::parse(id) {
        Ok(target) => target,
        Err(_) => {
            let mut response = Json(failed_envelope(
                models::error::Code::Variant0,
                "Invalid cover art ID.",
            ))
            .into_response();
            *response.status_mut() = StatusCode::BAD_REQUEST;
            return response;
        }
    };

    let if_none_match = headers
        .get(IF_NONE_MATCH)
        .and_then(|value| value.to_str().ok());
    let size = response::requested_size(query.get("size"));

    match state.artwork().load(target, if_none_match, size).await {
        Ok(ArtworkOutcome::Found(artwork)) => response::found_response(artwork),
        Ok(ArtworkOutcome::NotModified(etag)) => response::not_modified_response(etag),
        Ok(ArtworkOutcome::Missing) => {
            let mut response = Json(failed_envelope(
                models::error::Code::Variant70,
                "Cover art was not found.",
            ))
            .into_response();
            *response.status_mut() = StatusCode::NOT_FOUND;
            response
        }
        Err(_) => {
            tracing::warn!(
                request_id = context.request_id().as_str(),
                "cover art fetch failed"
            );
            let mut response = Json(failed_envelope(
                models::error::Code::Variant0,
                "Internal error.",
            ))
            .into_response();
            *response.status_mut() = StatusCode::INTERNAL_SERVER_ERROR;
            response
        }
    }
}

fn stream_failure_response(context: &RequestContext, error: PlaybackError) -> Response {
    let (status, code, message) = match &error {
        PlaybackError::TrackNotFound | PlaybackError::SourceNotFound => (
            StatusCode::NOT_FOUND,
            models::error::Code::Variant70,
            "Track was not found.",
        ),
        PlaybackError::CredentialsStale => (
            StatusCode::UNAUTHORIZED,
            models::error::Code::Variant40,
            "Upstream credentials have expired.",
        ),
        PlaybackError::Unavailable => (
            StatusCode::SERVICE_UNAVAILABLE,
            models::error::Code::Variant0,
            "Playback is temporarily unavailable.",
        ),
        PlaybackError::Library(_)
        | PlaybackError::Internal(_)
        | PlaybackError::Delivery(DeliveryError::Spool | DeliveryError::Local(_)) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            models::error::Code::Variant0,
            "Internal error.",
        ),
        PlaybackError::Provider(_) => (
            StatusCode::BAD_GATEWAY,
            models::error::Code::Variant0,
            "The playback resolver failed.",
        ),
        PlaybackError::Delivery(DeliveryError::Overloaded) => (
            StatusCode::SERVICE_UNAVAILABLE,
            models::error::Code::Variant0,
            "The server is busy.",
        ),
        PlaybackError::Delivery(DeliveryError::Range { content_length, .. }) => {
            let mut response = Json(failed_envelope(
                models::error::Code::Variant0,
                "The requested range is not satisfiable.",
            ))
            .into_response();
            *response.status_mut() = StatusCode::RANGE_NOT_SATISFIABLE;
            response.headers_mut().insert(
                CONTENT_RANGE,
                HeaderValue::from_str(&format!("bytes */{content_length}"))
                    .expect("formatted content range is a valid header value"),
            );
            return response;
        }
        PlaybackError::Delivery(DeliveryError::Media(FetchError::Request(_))) => (
            StatusCode::SERVICE_UNAVAILABLE,
            models::error::Code::Variant0,
            "The stream is temporarily unavailable",
        ),
        PlaybackError::Delivery(DeliveryError::Media(_)) => (
            StatusCode::BAD_GATEWAY,
            models::error::Code::Variant0,
            "The upstream media response was invalid",
        ),
    };
    match status {
        StatusCode::NOT_FOUND => tracing::debug!(error = %error, "playback track was not found"),
        _ => tracing::error!(
            error = %error,
            request_id = context.request_id().as_str(),
            "media stream failed"
        ),
    }
    let mut response = Json(failed_envelope(code, message)).into_response();
    *response.status_mut() = status;
    response
}

pub(crate) async fn get_open_subsonic_extensions(
    State(state): State<SubsonicState>,
    Extension(context): Extension<RequestContext>,
    auth: SubsonicAuth,
) -> Response {
    match authenticate(&state, &context, auth).await {
        Ok(_) => Json(extensions_envelope()).into_response(),
        Err(response) => response,
    }
}

async fn authenticate(
    state: &SubsonicState,
    context: &RequestContext,
    auth: SubsonicAuth,
) -> Result<Principal, Response> {
    match state.credential().authenticate(&auth.0).await {
        Ok(principal) => Ok(principal),
        Err(AuthError::InvalidCredentials) => Err(Json(failed_envelope(
            models::error::Code::Variant40,
            "Wrong username or password.",
        ))
        .into_response()),
        Err(AuthError::Storage(error)) => {
            tracing::error!(
                error = %error,
                request_id = context.request_id().as_str(),
                "credential storage failed"
            );

            // storage failures must never surface as auth failures
            let mut response = Json(failed_envelope(
                models::error::Code::Variant0,
                "Internal error.",
            ))
            .into_response();
            *response.status_mut() = StatusCode::INTERNAL_SERVER_ERROR;
            Err(response)
        }
    }
}

pub(crate) async fn get_starred2(
    State(state): State<SubsonicState>,
    Extension(context): Extension<RequestContext>,
    auth: SubsonicAuth,
) -> Response {
    let principal = match authenticate(&state, &context, auth).await {
        Ok(principal) => principal,
        Err(response) => return response,
    };
    let user = principal.user_id.as_uuid();

    match service::starred(state.library_state(), user, state.source_tags()).await {
        Ok(result) => Json(starred2_envelope(result)).into_response(),
        Err(error) => state_failure_response(&context, error),
    }
}

pub(crate) async fn get_genres(
    State(state): State<SubsonicState>,
    Extension(context): Extension<RequestContext>,
    auth: SubsonicAuth,
) -> Response {
    if let Err(response) = authenticate(&state, &context, auth).await {
        return response;
    }

    match service::genres(state.library()).await {
        Ok(genres) => Json(genres_envelope(genres)).into_response(),
        Err(error) => browse_failure_response(&context, error),
    }
}

fn playlist_failure(context: &RequestContext, error: PlaylistError) -> Response {
    tracing::warn!(
        error = %error,
        request_id = context.request_id().as_str(),
        "playlist request failed"
    );

    let mut response = Json(failed_envelope(
        models::error::Code::Variant0,
        "Internal error.",
    ))
    .into_response();
    *response.status_mut() = StatusCode::INTERNAL_SERVER_ERROR;
    response
}

fn playlist_not_found() -> Response {
    Json(failed_envelope(
        models::error::Code::Variant70,
        "Playlist not found.",
    ))
    .into_response()
}

pub(crate) async fn get_playlists(
    State(state): State<SubsonicState>,
    Extension(context): Extension<RequestContext>,
    auth: SubsonicAuth,
) -> Response {
    let principal = match authenticate(&state, &context, auth).await {
        Ok(principal) => principal,
        Err(response) => return response,
    };
    let user = principal.user_id.as_uuid();

    match state.playlists().list(user).await {
        Ok(playlists) => Json(playlists_envelope(
            playlists
                .iter()
                .map(mapping::playlist_from_domain)
                .collect(),
        ))
        .into_response(),
        Err(error) => playlist_failure(&context, error),
    }
}

pub(crate) async fn get_playlist(
    State(state): State<SubsonicState>,
    Extension(context): Extension<RequestContext>,
    auth: SubsonicAuth,
    query: RawQuery,
) -> Response {
    let principal = match authenticate(&state, &context, auth).await {
        Ok(principal) => principal,
        Err(response) => return response,
    };
    let user = principal.user_id.as_uuid();

    let Some(raw_id) = query.get("id") else {
        return SubsonicError::MissingParameter.into_response();
    };
    let Ok(playlist_id) = raw_id.parse() else {
        return playlist_not_found();
    };

    match state.playlists().detail(user, &playlist_id).await {
        Ok(Some(detail)) => Json(playlist_envelope(mapping::playlist_with_songs(
            &detail,
            state.source_tags(),
        )))
        .into_response(),
        Ok(None) => playlist_not_found(),
        Err(error) => playlist_failure(&context, error),
    }
}

pub(crate) async fn create_playlist(
    State(state): State<SubsonicState>,
    Extension(context): Extension<RequestContext>,
    auth: SubsonicAuth,
    query: RawQuery,
) -> Response {
    let principal = match authenticate(&state, &context, auth).await {
        Ok(principal) => principal,
        Err(response) => return response,
    };
    let user = principal.user_id.as_uuid();

    let name = query.get("name").unwrap_or_default().trim().to_owned();
    let playlist_id = query.get("playlistId").and_then(|raw| {
        raw.parse::<crate::features::music_library::domain::PlaylistId>()
            .ok()
    });
    let additions = query
        .all("songId")
        .iter()
        .filter_map(|raw| raw.parse::<TrackId>().ok())
        .collect::<Vec<_>>();

    match state
        .playlists()
        .create_or_append(user, playlist_id, &name, &additions)
        .await
    {
        Ok(Some(detail)) => Json(create_playlist_envelope(mapping::playlist_with_songs(
            &detail,
            state.source_tags(),
        )))
        .into_response(),
        Ok(None) => playlist_not_found(),
        Err(error) => playlist_failure(&context, error),
    }
}

pub(crate) async fn update_playlist(
    State(state): State<SubsonicState>,
    Extension(context): Extension<RequestContext>,
    auth: SubsonicAuth,
    query: RawQuery,
) -> Response {
    let principal = match authenticate(&state, &context, auth).await {
        Ok(principal) => principal,
        Err(response) => return response,
    };
    let user = principal.user_id.as_uuid();

    let Some(raw_id) = query.get("playlistId") else {
        return SubsonicError::MissingParameter.into_response();
    };
    let Ok(playlist_id) = raw_id.parse() else {
        return playlist_not_found();
    };

    let additions = query
        .all("songIdToAdd")
        .iter()
        .filter_map(|raw| raw.parse::<TrackId>().ok())
        .collect::<Vec<_>>();
    let removals = query
        .all("songIndexToRemove")
        .iter()
        .filter_map(|raw| raw.parse::<usize>().ok())
        .collect::<Vec<_>>();
    let public = query
        .get("public")
        .map(|raw| raw.trim().eq_ignore_ascii_case("true"));
    match state
        .playlists()
        .edit(
            user,
            &playlist_id,
            PlaylistEdit {
                name: query.get("name"),
                comment: query.get("comment"),
                public,
                additions: &additions,
                removals,
            },
        )
        .await
    {
        Ok(true) => Json(ok_envelope()).into_response(),
        Ok(false) => playlist_not_found(),
        Err(error) => playlist_failure(&context, error),
    }
}

pub(crate) async fn delete_playlist(
    State(state): State<SubsonicState>,
    Extension(context): Extension<RequestContext>,
    auth: SubsonicAuth,
    query: RawQuery,
) -> Response {
    let principal = match authenticate(&state, &context, auth).await {
        Ok(principal) => principal,
        Err(response) => return response,
    };
    let user = principal.user_id.as_uuid();

    let Some(raw_id) = query.get("id") else {
        return SubsonicError::MissingParameter.into_response();
    };
    let Ok(playlist_id) = raw_id.parse() else {
        return playlist_not_found();
    };

    match state.playlists().delete(user, &playlist_id).await {
        Ok(true) => Json(ok_envelope()).into_response(),
        Ok(false) => playlist_not_found(),
        Err(error) => playlist_failure(&context, error),
    }
}

pub(crate) async fn get_scan_status(
    State(state): State<SubsonicState>,
    Extension(context): Extension<RequestContext>,
    auth: SubsonicAuth,
) -> Response {
    if let Err(response) = authenticate(&state, &context, auth).await {
        return response;
    }

    Json(scan_status_envelope(&state.scanner().status())).into_response()
}

pub(crate) async fn start_scan(
    State(state): State<SubsonicState>,
    Extension(context): Extension<RequestContext>,
    auth: SubsonicAuth,
    query: RawQuery,
) -> Response {
    let principal = match authenticate(&state, &context, auth).await {
        Ok(principal) => principal,
        Err(response) => return response,
    };

    let full = query
        .get("fullScan")
        .is_some_and(|raw| matches!(raw.trim().to_ascii_lowercase().as_str(), "true" | "1"));
    let mode = if full {
        ScanMode::Full
    } else {
        ScanMode::Incremental
    };

    match state.scanner().request(&principal, mode) {
        Ok(mut status) => {
            if state.scanner().is_configured() {
                status.scanning = true;
            }
            Json(start_scan_envelope(&status)).into_response()
        }
        Err(ScanRequestError::Forbidden) => {
            let mut response = Json(failed_envelope(
                models::error::Code::Variant50,
                "Not authorized.",
            ))
            .into_response();
            *response.status_mut() = StatusCode::FORBIDDEN;
            response
        }
    }
}

pub(crate) async fn star(
    State(state): State<SubsonicState>,
    Extension(context): Extension<RequestContext>,
    auth: SubsonicAuth,
    query: RawQuery,
) -> Response {
    star_request(&state, &context, auth, &query, true).await
}

pub(crate) async fn unstar(
    State(state): State<SubsonicState>,
    Extension(context): Extension<RequestContext>,
    auth: SubsonicAuth,
    query: RawQuery,
) -> Response {
    star_request(&state, &context, auth, &query, false).await
}

async fn star_request(
    state: &SubsonicState,
    context: &RequestContext,
    auth: SubsonicAuth,
    query: &RawQuery,
    starred: bool,
) -> Response {
    let principal = match authenticate(state, context, auth).await {
        Ok(principal) => principal,
        Err(response) => return response,
    };
    let user = principal.user_id.as_uuid();
    let songs = query
        .all("id")
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let albums = query
        .all("albumId")
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let artists = query
        .all("artistId")
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();

    match service::set_starred(
        state.library_state(),
        user,
        &songs,
        &albums,
        &artists,
        starred,
    )
    .await
    {
        Ok(()) => Json(ok_envelope()).into_response(),
        Err(error) => state_failure_response(context, error),
    }
}

pub(crate) async fn set_rating(
    State(state): State<SubsonicState>,
    Extension(context): Extension<RequestContext>,
    auth: SubsonicAuth,
    query: RawQuery,
) -> Response {
    let principal = match authenticate(&state, &context, auth).await {
        Ok(principal) => principal,
        Err(response) => return response,
    };
    let Some(id) = query.get("id") else {
        return SubsonicError::MissingParameter.into_response();
    };
    let Some(raw_rating) = query.get("rating") else {
        return SubsonicError::MissingParameter.into_response();
    };
    let Ok(rating) = raw_rating.trim().parse::<i64>() else {
        return invalid_parameter_response();
    };
    let user = principal.user_id.as_uuid();

    match service::set_rating(state.library_state(), user, id, rating).await {
        Ok(()) => Json(ok_envelope()).into_response(),
        Err(error) => state_failure_response(&context, error),
    }
}

pub(crate) async fn scrobble(
    State(state): State<SubsonicState>,
    Extension(context): Extension<RequestContext>,
    auth: SubsonicAuth,
    query: RawQuery,
) -> Response {
    let principal = match authenticate(&state, &context, auth).await {
        Ok(principal) => principal,
        Err(response) => return response,
    };
    let user = principal.user_id.as_uuid();
    let ids = query
        .all("id")
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let times = query
        .all("time")
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let submission = query
        .get("submission")
        .is_none_or(|raw| !matches!(raw.trim().to_ascii_lowercase().as_str(), "false" | "0"));

    match service::scrobble(state.library_state(), user, &ids, &times, submission).await {
        Ok(()) => Json(ok_envelope()).into_response(),
        Err(error) => state_failure_response(&context, error),
    }
}

fn state_failure_response(context: &RequestContext, error: service::StateError) -> Response {
    let (status, code, message) = match error {
        service::StateError::InvalidRequest => (
            StatusCode::UNPROCESSABLE_ENTITY,
            models::error::Code::Variant0,
            "Invalid state parameters.",
        ),
        service::StateError::MissingParameter => (
            StatusCode::BAD_REQUEST,
            models::error::Code::Variant10,
            "Required parameter is missing.",
        ),
        service::StateError::NotFound => (
            StatusCode::NOT_FOUND,
            models::error::Code::Variant70,
            "Not found.",
        ),
        service::StateError::State(error) => {
            tracing::error!(
                error = %error,
                request_id = context.request_id().as_str(),
                "library state write failed"
            );
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                models::error::Code::Variant0,
                "Internal error.",
            )
        }
    };

    let mut response = Json(failed_envelope(code, message)).into_response();
    *response.status_mut() = status;
    response
}

fn invalid_parameter_response() -> Response {
    let mut response = Json(failed_envelope(
        models::error::Code::Variant0,
        "Invalid parameter.",
    ))
    .into_response();
    *response.status_mut() = StatusCode::UNPROCESSABLE_ENTITY;
    response
}

async fn unsupported(Extension(context): Extension<RequestContext>, uri: OriginalUri) -> Response {
    tracing::debug!(
        path = %uri.0.path(),
        request_id = context.request_id().as_str(),
        "unsupported subsonic operation"
    );

    Json(failed_envelope(
        models::error::Code::Variant0,
        "Unsupported operation.",
    ))
    .into_response()
}
