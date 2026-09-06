use axum::{
    Extension, Json, Router,
    extract::{OriginalUri, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::get,
};
use whio_subsonic_api::models;

use crate::{
    AppState,
    http::{
        raw_query::RawQuery,
        subsonic::{self, SubsonicAuth, SubsonicError},
    },
    identity::{domain::Principal, service::AuthError},
    request::RequestContext,
};

// mounted as its own subtree so unknown endpoints answer with a subsonic
// failure envelope instead of the product api's json errors
pub(crate) fn router() -> Router<AppState> {
    Router::new()
        .route("/ping", get(ping).post(ping))
        .route("/getUser", get(get_user).post(get_user))
        .route(
            "/getOpenSubsonicExtensions",
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
        Ok(_) => Json(subsonic::ok_envelope()).into_response(),
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
        Ok(Some(user)) => Json(subsonic::user_envelope(&user.username)).into_response(),
        Ok(None) => Json(subsonic::failed_envelope(
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

            let mut response = Json(subsonic::failed_envelope(
                models::error::Code::Variant0,
                "Internal error.",
            ))
            .into_response();
            *response.status_mut() = StatusCode::INTERNAL_SERVER_ERROR;
            response
        }
    }
}

pub(crate) async fn get_open_subsonic_extensions(
    State(state): State<AppState>,
    Extension(context): Extension<RequestContext>,
    auth: SubsonicAuth,
) -> Response {
    match authenticate(&state, &context, auth).await {
        Ok(_) => Json(subsonic::extensions_envelope()).into_response(),
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
        Err(AuthError::InvalidCredentials) => Err(Json(subsonic::failed_envelope(
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
            let mut response = Json(subsonic::failed_envelope(
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

    Json(subsonic::failed_envelope(
        models::error::Code::Variant0,
        "Unsupported operation.",
    ))
    .into_response()
}
