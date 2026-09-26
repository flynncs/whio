use std::sync::Arc;

use axum::{
    Json,
    extract::{FromRef, FromRequestParts},
    http::{StatusCode, request::Parts},
    response::{IntoResponse, Response},
};
use serde::Serialize;

use crate::{
    features::identity::api::query_auth,
    features::identity::{domain::Principal, service::AuthError},
    platform::raw_query::RawQuery,
    platform::request::{RequestContext, RequestId},
};

#[derive(Clone)]
pub(crate) struct NativeState(
    pub(crate) Arc<crate::features::identity::service::CredentialService>,
);

/// Native routes use the same application-password forms as Subsonic routes.
pub(crate) struct AuthenticatedPrincipal(pub(crate) Principal);

#[derive(Serialize)]
struct AuthErrorResponse {
    code: &'static str,
    message: &'static str,
    request_id: String,
}

impl<S> FromRequestParts<S> for AuthenticatedPrincipal
where
    S: Send + Sync,
    NativeState: FromRef<S>,
{
    type Rejection = Response;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let context = parts.extensions.get::<RequestContext>();
        let request_id = context
            .map(|context| context.request_id().as_str().to_owned())
            .unwrap_or_else(|| RequestId::generate().into_string());

        let query = RawQuery::from_request_parts(parts, state)
            .await
            .unwrap_or_else(|infallible| match infallible {});
        let auth = query_auth::presented(&query).map_err(|_| {
            auth_error(
                StatusCode::UNAUTHORIZED,
                "Authentication is required",
                request_id.clone(),
            )
        })?;

        match NativeState::from_ref(state).0.authenticate(&auth).await {
            Ok(principal) => Ok(Self(principal)),
            Err(AuthError::InvalidCredentials) => Err(auth_error(
                StatusCode::UNAUTHORIZED,
                "Invalid credentials",
                request_id,
            )),
            Err(AuthError::Storage(error)) => {
                tracing::error!(%error, %request_id, "credential storage failed");
                Err(auth_error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "An unexpected error occurred",
                    request_id,
                ))
            }
        }
    }
}

fn auth_error(status: StatusCode, message: &'static str, request_id: String) -> Response {
    (
        status,
        Json(AuthErrorResponse {
            code: "authentication_failed",
            message,
            request_id,
        }),
    )
        .into_response()
}
