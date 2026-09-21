mod auth;
mod envelope;
mod mapping;
mod service;

use axum::{
    Extension, Json, Router,
    extract::{OriginalUri, State},
    http::{HeaderMap, HeaderValue, StatusCode, header::CONTENT_RANGE},
    response::{IntoResponse, Response},
    routing::get,
};
use whio_subsonic_api::models;

use crate::{
    AppState,
    http::{errors::media_error_details, raw_query::RawQuery},
    identity::{domain::Principal, service::AuthError},
    library::ScanMode,
    playback::{DeliveryError, PlaybackError},
    providers::ProviderError,
    request::RequestContext,
};
pub(crate) use auth::SubsonicAuth;
use auth::SubsonicError;
use envelope::{
    album_envelope, album_list2_envelope, artist_envelope, artists_envelope, extensions_envelope,
    failed_envelope, genres_envelope, ok_envelope, scan_status_envelope, search3_envelope,
    starred2_envelope, start_scan_envelope, user_envelope,
};

pub(crate) fn router() -> Router<AppState> {
    [
        ("/ping", get(ping).post(ping)),
        ("/getUser", get(get_user).post(get_user)),
        ("/search3", get(search3).post(search3)),
        ("/getAlbumList2", get(get_album_list2)),
        ("/getAlbum", get(get_album)),
        ("/getArtists", get(get_artists)),
        ("/getArtist", get(get_artist)),
        ("/getStarred2", get(get_starred2).post(get_starred2)),
        ("/getGenres", get(get_genres).post(get_genres)),
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
    State(state): State<AppState>,
    Extension(context): Extension<RequestContext>,
    auth: SubsonicAuth,
) -> Response {
    match authenticate(&state, &context, auth).await {
        Ok(_) => Json(ok_envelope()).into_response(),
        Err(response) => response,
    }
}

pub(crate) async fn get_user(
    State(state): State<AppState>,
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
        Ok(Some(user)) => Json(user_envelope(&user.username)).into_response(),
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
    State(state): State<AppState>,
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
    match service::search(
        state.search(),
        &context,
        Some(&user_uuid),
        q,
        query.get("songCount"),
        query.get("songOffset"),
        state.source_tags(),
    )
    .await
    {
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

pub(crate) async fn get_album_list2(
    State(state): State<AppState>,
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
    State(state): State<AppState>,
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
    State(state): State<AppState>,
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
    State(state): State<AppState>,
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
    State(state): State<AppState>,
    Extension(context): Extension<RequestContext>,
    auth: SubsonicAuth,
    query: RawQuery,
    headers: HeaderMap,
) -> Response {
    use crate::{delivery::into_response, tracks::TrackId};

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
    State(state): State<AppState>,
    Extension(context): Extension<RequestContext>,
    auth: SubsonicAuth,
    query: RawQuery,
    headers: HeaderMap,
) -> Response {
    use crate::artwork::ArtworkOutcome;
    use crate::http::routes::artwork;
    use axum::http::header::IF_NONE_MATCH;

    if let Err(response) = authenticate(&state, &context, auth).await {
        return response;
    }
    let Some(id) = query.get("id") else {
        return SubsonicError::MissingParameter.into_response();
    };

    let if_none_match = headers
        .get(IF_NONE_MATCH)
        .and_then(|value| value.to_str().ok());
    let size = artwork::requested_size(query.get("size"));

    match artwork::load(&state, id, if_none_match, size).await {
        Ok(ArtworkOutcome::Found(artwork)) => artwork::found_response(artwork),
        Ok(ArtworkOutcome::NotModified(etag)) => artwork::not_modified_response(etag),
        Ok(ArtworkOutcome::Missing) => {
            let mut response = Json(failed_envelope(
                models::error::Code::Variant70,
                "Cover art was not found.",
            ))
            .into_response();
            *response.status_mut() = StatusCode::NOT_FOUND;
            response
        }
        Err(artwork::ArtworkFailure::InvalidId(_)) => {
            let mut response = Json(failed_envelope(
                models::error::Code::Variant0,
                "Invalid cover art ID.",
            ))
            .into_response();
            *response.status_mut() = StatusCode::BAD_REQUEST;
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
        PlaybackError::Repository(_)
        | PlaybackError::Provider(ProviderError::Access)
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
        PlaybackError::Delivery(DeliveryError::Media(media)) => {
            let (status, _, message) = media_error_details(media);
            (status, models::error::Code::Variant0, message)
        }
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
    State(state): State<AppState>,
    Extension(context): Extension<RequestContext>,
    auth: SubsonicAuth,
) -> Response {
    match authenticate(&state, &context, auth).await {
        Ok(_) => Json(extensions_envelope()).into_response(),
        Err(response) => response,
    }
}

async fn authenticate(
    state: &AppState,
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
    State(state): State<AppState>,
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
    State(state): State<AppState>,
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

pub(crate) async fn get_scan_status(
    State(state): State<AppState>,
    Extension(context): Extension<RequestContext>,
    auth: SubsonicAuth,
) -> Response {
    if let Err(response) = authenticate(&state, &context, auth).await {
        return response;
    }

    Json(scan_status_envelope(&state.scanner().status())).into_response()
}

pub(crate) async fn start_scan(
    State(state): State<AppState>,
    Extension(context): Extension<RequestContext>,
    auth: SubsonicAuth,
    query: RawQuery,
) -> Response {
    if let Err(response) = authenticate(&state, &context, auth).await {
        return response;
    }

    let full = query
        .get("fullScan")
        .is_some_and(|raw| matches!(raw.trim().to_ascii_lowercase().as_str(), "true" | "1"));
    state.scanner().trigger(if full {
        ScanMode::Full
    } else {
        ScanMode::Incremental
    });

    let mut status = state.scanner().status();
    if state.scanner().is_configured() {
        status.scanning = true;
    }

    Json(start_scan_envelope(&status)).into_response()
}

pub(crate) async fn star(
    State(state): State<AppState>,
    Extension(context): Extension<RequestContext>,
    auth: SubsonicAuth,
    query: RawQuery,
) -> Response {
    star_request(&state, &context, auth, &query, true).await
}

pub(crate) async fn unstar(
    State(state): State<AppState>,
    Extension(context): Extension<RequestContext>,
    auth: SubsonicAuth,
    query: RawQuery,
) -> Response {
    star_request(&state, &context, auth, &query, false).await
}

async fn star_request(
    state: &AppState,
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
    State(state): State<AppState>,
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
    State(state): State<AppState>,
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
