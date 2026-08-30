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
    http::subsonic::{self, SubsonicAuth},
    identity::service::AuthError,
    request::RequestContext,
};

// mounted as its own subtree so unknown endpoints answer with a subsonic
// failure envelope instead of the product api's json errors
pub(crate) fn router() -> Router<AppState> {
    Router::new()
        .route("/ping", get(ping).post(ping))
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
        Ok(()) => Json(subsonic::ok_envelope()).into_response(),
        Err(response) => response,
    }
}

pub(crate) async fn get_open_subsonic_extensions(
    State(state): State<AppState>,
    Extension(context): Extension<RequestContext>,
    auth: SubsonicAuth,
) -> Response {
    match authenticate(&state, &context, auth).await {
        Ok(()) => Json(subsonic::extensions_envelope()).into_response(),
        Err(response) => response,
    }
}

async fn authenticate(
    state: &AppState,
    context: &RequestContext,
    auth: SubsonicAuth,
) -> Result<(), Response> {
    match state.credential().authenticate(&auth.0).await {
        Ok(_) => Ok(()),
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
