mod auth;
mod envelope;
mod service;

use axum::{
    Extension, Json, Router,
    extract::{OriginalUri, State},
    http::{HeaderMap, HeaderValue, StatusCode, header::CONTENT_RANGE},
    response::{IntoResponse, Response},
    routing::get,
};
use whio_subsonic_api::models;

use super::super::http::routes::playback::StreamFailure;
use crate::{
    AppState,
    http::{errors::media_error_details, raw_query::RawQuery},
    identity::{domain::Principal, service::AuthError},
    playback::PlaybackError,
    playback_stream::PlaybackStreamError,
    request::RequestContext,
};
use auth::{SubsonicAuth, SubsonicError};
use envelope::{
    extensions_envelope, failed_envelope, ok_envelope, search3_envelope, user_envelope,
};

pub(crate) fn router() -> Router<AppState> {
    Router::new()
        .route("/ping.view", get(ping).post(ping))
        .route("/getUser.view", get(get_user).post(get_user))
        .route("/search3.view", get(search3).post(search3))
        .route("/stream.view", get(stream))
        .route(
            "/getOpenSubsonicExtensions.view",
            get(get_open_subsonic_extensions).post(get_open_subsonic_extensions),
        )
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
    if let Err(response) = authenticate(&state, &context, auth).await {
        return response;
    }
    let Some(q) = query.get("query") else {
        return SubsonicError::MissingParameter.into_response();
    };
    match service::search_songs(state.catalogue(), &context, q, query.get("songCount")).await {
        Ok(songs) => Json(search3_envelope(songs)).into_response(),
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

pub(crate) async fn stream(
    State(state): State<AppState>,
    Extension(context): Extension<RequestContext>,
    auth: SubsonicAuth,
    query: RawQuery,
    headers: HeaderMap,
) -> Response {
    if let Err(response) = authenticate(&state, &context, auth).await {
        return response;
    }
    let Some(id) = query.get("id") else {
        return SubsonicError::MissingParameter.into_response();
    };
    match crate::http::routes::playback::stream_response(&state, &context, id, &headers).await {
        Ok(response) => response,
        Err(StreamFailure::InvalidId(_)) => {
            let mut response = Json(failed_envelope(
                models::error::Code::Variant0,
                "Invalid track ID.",
            ))
            .into_response();
            *response.status_mut() = StatusCode::BAD_REQUEST;
            response
        }
        Err(StreamFailure::Stream(error)) => stream_failure_response(&context, error),
        Err(StreamFailure::Range { content_length, .. }) => {
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
            response
        }
    }
}

fn stream_failure_response(context: &RequestContext, error: PlaybackStreamError) -> Response {
    let (status, code, message) = match &error {
        PlaybackStreamError::Playback(PlaybackError::TrackNotFound) => (
            StatusCode::NOT_FOUND,
            models::error::Code::Variant70,
            "The track was not found.",
        ),
        PlaybackStreamError::Playback(PlaybackError::Resolver(_)) => (
            StatusCode::BAD_GATEWAY,
            models::error::Code::Variant0,
            "The playback resolver failed.",
        ),
        PlaybackStreamError::Playback(PlaybackError::Repository(_)) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            models::error::Code::Variant0,
            "Internal error.",
        ),
        PlaybackStreamError::Media(media) => {
            let (status, _, message) = media_error_details(media);
            (status, models::error::Code::Variant0, message)
        }
        PlaybackStreamError::Spool => (
            StatusCode::INTERNAL_SERVER_ERROR,
            models::error::Code::Variant0,
            "Internal error.",
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
