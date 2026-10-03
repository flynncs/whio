use std::sync::Arc;

use axum::{
    Extension, Json, Router,
    extract::{FromRef, Path, State, rejection::JsonRejection},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{delete, get, post},
};
use serde::Deserialize;

use crate::{
    features::identity::{
        api::native::{AuthenticatedPrincipal, NativeState},
        destinations::{DestinationError, DestinationService, DestinationStatus},
        domain::Principal,
        service::CredentialService,
    },
    platform::{
        http_response::{error_response, invalid_request_response},
        raw_query::RawQuery,
        request::RequestContext,
    },
};

#[derive(Clone)]
pub struct DestinationState {
    pub(crate) destinations: Arc<DestinationService>,
    pub(crate) credential: Arc<CredentialService>,
}

impl DestinationState {
    pub fn new(destinations: Arc<DestinationService>, credential: Arc<CredentialService>) -> Self {
        Self {
            destinations,
            credential,
        }
    }
}

impl FromRef<DestinationState> for NativeState {
    fn from_ref(state: &DestinationState) -> Self {
        Self(Arc::clone(&state.credential))
    }
}

#[derive(Deserialize)]
pub(crate) struct ConnectRequest {
    provider_id: String,
    token: String,
}

pub(crate) enum DestinationApiError {
    Native(DestinationError, RequestContext),
    InvalidRequest(RequestContext),
    Malformed(RequestContext),
}

impl IntoResponse for DestinationApiError {
    fn into_response(self) -> Response {
        match self {
            Self::Native(error, context) => {
                let (status, code, message) = match &error {
                    DestinationError::Disabled => (
                        StatusCode::SERVICE_UNAVAILABLE,
                        "destination_credentials_unavailable",
                        "Destination credentials are not configured",
                    ),
                    DestinationError::Unavailable => (
                        StatusCode::SERVICE_UNAVAILABLE,
                        "destination_unavailable",
                        "The destination is temporarily unavailable",
                    ),
                    DestinationError::Unsupported => {
                        return invalid_request_response(context);
                    }
                    DestinationError::InvalidCredential => (
                        StatusCode::UNPROCESSABLE_ENTITY,
                        "invalid_credential",
                        "The destination rejected the credential",
                    ),
                    DestinationError::AlreadyConnected => (
                        StatusCode::CONFLICT,
                        "already_connected",
                        "A destination for this provider is already connected",
                    ),
                    DestinationError::NotFound => (StatusCode::NOT_FOUND, "not_found", "Not found"),
                    DestinationError::Repository(_) => (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "internal_error",
                        "An unexpected error occurred",
                    ),
                };

                if matches!(
                    error,
                    DestinationError::Repository(_) | DestinationError::Unavailable
                ) {
                    tracing::error!(
                        error = %error,
                        request_id = context.request_id().as_str(),
                        "destination operation failed"
                    );
                }

                error_response(status, code, message, context)
            }
            Self::InvalidRequest(context) => invalid_request_response(context),
            Self::Malformed(context) => {
                tracing::debug!(
                    request_id = context.request_id().as_str(),
                    "malformed destination request"
                );
                invalid_request_response(context)
            }
        }
    }
}

fn reject_unknown(query: &RawQuery, context: &RequestContext) -> Result<(), DestinationApiError> {
    crate::features::music_library::api::native::reject_unknown(query, &[])
        .map_err(|_| DestinationApiError::InvalidRequest(context.clone()))
}

pub(crate) async fn connect(
    State(state): State<DestinationState>,
    Extension(context): Extension<RequestContext>,
    AuthenticatedPrincipal(principal): AuthenticatedPrincipal,
    query: RawQuery,
    payload: Result<Json<ConnectRequest>, JsonRejection>,
) -> Result<Json<DestinationStatus>, DestinationApiError> {
    reject_unknown(&query, &context)?;
    let Json(request) = payload.map_err(|_| DestinationApiError::Malformed(context.clone()))?;

    state
        .destinations
        .connect(&principal, &request.provider_id, &request.token)
        .await
        .map(Json)
        .map_err(|error| DestinationApiError::Native(error, context))
}

pub(crate) async fn list(
    State(state): State<DestinationState>,
    Extension(context): Extension<RequestContext>,
    AuthenticatedPrincipal(principal): AuthenticatedPrincipal,
    query: RawQuery,
) -> Result<Json<Vec<DestinationStatus>>, DestinationApiError> {
    reject_unknown(&query, &context)?;

    state
        .destinations
        .status(&principal)
        .await
        .map(Json)
        .map_err(|error| DestinationApiError::Native(error, context))
}

pub(crate) async fn enable(
    State(state): State<DestinationState>,
    Extension(context): Extension<RequestContext>,
    AuthenticatedPrincipal(principal): AuthenticatedPrincipal,
    Path(provider_id): Path<String>,
    query: RawQuery,
) -> Result<Json<DestinationStatus>, DestinationApiError> {
    set_enabled(state, context, principal, provider_id, true, &query).await
}

pub(crate) async fn disable(
    State(state): State<DestinationState>,
    Extension(context): Extension<RequestContext>,
    AuthenticatedPrincipal(principal): AuthenticatedPrincipal,
    Path(provider_id): Path<String>,
    query: RawQuery,
) -> Result<Json<DestinationStatus>, DestinationApiError> {
    set_enabled(state, context, principal, provider_id, false, &query).await
}

async fn set_enabled(
    state: DestinationState,
    context: RequestContext,
    principal: Principal,
    provider_id: String,
    enabled: bool,
    query: &RawQuery,
) -> Result<Json<DestinationStatus>, DestinationApiError> {
    reject_unknown(query, &context)?;

    state
        .destinations
        .set_enabled(&principal, &provider_id, enabled)
        .await
        .map(Json)
        .map_err(|error| DestinationApiError::Native(error, context))
}

pub(crate) async fn disconnect(
    State(state): State<DestinationState>,
    Extension(context): Extension<RequestContext>,
    AuthenticatedPrincipal(principal): AuthenticatedPrincipal,
    Path(provider_id): Path<String>,
    query: RawQuery,
) -> Result<StatusCode, DestinationApiError> {
    reject_unknown(&query, &context)?;

    state
        .destinations
        .disconnect(&principal, &provider_id)
        .await
        .map(|()| StatusCode::NO_CONTENT)
        .map_err(|error| DestinationApiError::Native(error, context))
}

pub fn routes() -> Router<DestinationState> {
    Router::new()
        .route("/api/v1/identity/destinations", get(list).post(connect))
        .route(
            "/api/v1/identity/destinations/{provider_id}/enable",
            post(enable),
        )
        .route(
            "/api/v1/identity/destinations/{provider_id}/disable",
            post(disable),
        )
        .route(
            "/api/v1/identity/destinations/{provider_id}",
            delete(disconnect),
        )
}
