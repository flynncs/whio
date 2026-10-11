use std::sync::Arc;

use axum::{
    Extension, Json, Router,
    extract::{FromRef, Path, State, rejection::JsonRejection},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde::Serialize;

use crate::features::acquisition::domain::{AcquisitionOrigin, OriginKind, Request};
use crate::features::acquisition::ports::{AcquisitionTarget, MbId};
use crate::features::acquisition::service::{AcquisitionError, AcquisitionService};
use crate::features::identity::api::native::{AuthenticatedPrincipal, NativeState};
use crate::features::identity::service::CredentialService;
use crate::features::music_library::domain::{AlbumId, TrackId};
use crate::platform::{
    http_response::error_response, raw_query::RawQuery, request::RequestContext,
};

const MAX_ORIGINS: usize = 100;
const IDENTITY_PARAMETERS: &[&str] = &["apiKey", "c", "p", "s", "t", "u", "v"];

fn unknown_query(query: &RawQuery, allowed: &[&str]) -> bool {
    query
        .names()
        .any(|name| !IDENTITY_PARAMETERS.contains(&name) && !allowed.contains(&name))
}

#[derive(Clone)]
pub(crate) struct AcquisitionState {
    pub(crate) service: Option<Arc<AcquisitionService>>,
    pub(crate) credential: Arc<CredentialService>,
}

impl FromRef<AcquisitionState> for NativeState {
    fn from_ref(state: &AcquisitionState) -> Self {
        Self(Arc::clone(&state.credential))
    }
}

pub(crate) enum AcquisitionApiError {
    Disabled(RequestContext),
    Forbidden(RequestContext),
    Invalid(RequestContext),
    NotFound(RequestContext),
    Unavailable(RequestContext),
    Failed(RequestContext),
}

impl IntoResponse for AcquisitionApiError {
    fn into_response(self) -> Response {
        match self {
            Self::Disabled(context) => error_response(
                StatusCode::SERVICE_UNAVAILABLE,
                "acquisition_disabled",
                "Acquisition is not configured",
                context,
            ),
            Self::Forbidden(context) => error_response(
                StatusCode::FORBIDDEN,
                "forbidden",
                "Administrator access is required",
                context,
            ),
            Self::Invalid(context) => error_response(
                StatusCode::UNPROCESSABLE_ENTITY,
                "invalid_request",
                "The request is invalid",
                context,
            ),
            Self::NotFound(context) => {
                error_response(StatusCode::NOT_FOUND, "not_found", "Not found", context)
            }
            Self::Unavailable(context) => error_response(
                StatusCode::SERVICE_UNAVAILABLE,
                "acquisition_unavailable",
                "Acquisition is temporarily unavailable",
                context,
            ),
            Self::Failed(context) => error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal_error",
                "An unexpected error occurred",
                context,
            ),
        }
    }
}

fn failure(context: &RequestContext, error: AcquisitionError) -> AcquisitionApiError {
    match error {
        AcquisitionError::NotFound => AcquisitionApiError::NotFound(context.clone()),
        AcquisitionError::Unavailable => AcquisitionApiError::Unavailable(context.clone()),
        AcquisitionError::Failed => AcquisitionApiError::Failed(context.clone()),
        AcquisitionError::Repository(error) => {
            tracing::error!(
                error = %error,
                request_id = context.request_id().as_str(),
                "acquisition storage failed"
            );

            AcquisitionApiError::Failed(context.clone())
        }
    }
}

fn service(
    state: &AcquisitionState,
    context: &RequestContext,
) -> Result<Arc<AcquisitionService>, AcquisitionApiError> {
    state
        .service
        .clone()
        .ok_or_else(|| AcquisitionApiError::Disabled(context.clone()))
}

fn require_admin(
    principal: &crate::features::identity::domain::Principal,
    context: &RequestContext,
) -> Result<(), AcquisitionApiError> {
    if principal.is_admin() {
        Ok(())
    } else {
        Err(AcquisitionApiError::Forbidden(context.clone()))
    }
}

const ORIGIN_KEYS: [(&str, OriginKind); 4] = [
    ("album_id", OriginKind::Album),
    ("track_id", OriginKind::Track),
    ("musicbrainz_release_id", OriginKind::Release),
    ("musicbrainz_recording_id", OriginKind::Recording),
];

fn origin_from_value(value: &serde_json::Value) -> Option<AcquisitionOrigin> {
    let object = value.as_object()?;

    if object
        .keys()
        .any(|key| !ORIGIN_KEYS.iter().any(|(name, _)| name == key))
    {
        return None;
    }

    let supplied = ORIGIN_KEYS
        .iter()
        .filter(|(name, _)| object.contains_key(*name))
        .collect::<Vec<_>>();

    if supplied.len() != 1 {
        return None;
    }

    let (name, kind) = supplied[0];
    let value = object.get(*name)?.as_str()?;

    match kind {
        OriginKind::Album => value.parse::<AlbumId>().ok().map(AcquisitionOrigin::Album),
        OriginKind::Track => value.parse::<TrackId>().ok().map(AcquisitionOrigin::Track),
        OriginKind::Release => MbId::parse(value).map(AcquisitionOrigin::Release),
        OriginKind::Recording => MbId::parse(value).map(AcquisitionOrigin::Recording),
    }
}

#[derive(Serialize)]
struct OriginBody {
    kind: &'static str,
    id: String,
}

#[derive(Serialize)]
struct TargetBody {
    kind: &'static str,
    musicbrainz_id: String,
}

#[derive(Serialize)]
pub(crate) struct RequestBody {
    id: String,
    origin: OriginBody,
    target: Option<TargetBody>,
    state: &'static str,
    reason: Option<String>,
    attempts: u16,
    created_at: String,
    updated_at: String,
}

impl From<&Request> for RequestBody {
    fn from(request: &Request) -> Self {
        Self {
            id: request.id.to_string(),
            origin: OriginBody {
                kind: request.origin.kind().as_str(),
                id: request.origin.id(),
            },
            target: request.target.as_ref().map(|target| match target {
                AcquisitionTarget::Release(id) => TargetBody {
                    kind: "release",
                    musicbrainz_id: id.to_string(),
                },
                AcquisitionTarget::Recording(id) => TargetBody {
                    kind: "recording",
                    musicbrainz_id: id.to_string(),
                },
            }),
            state: request.state.as_str(),
            reason: request.reason.clone(),
            attempts: request.attempts,
            created_at: request.created_at.to_rfc3339(),
            updated_at: request.updated_at.to_rfc3339(),
        }
    }
}

pub(crate) fn routes() -> Router<AcquisitionState> {
    Router::new()
        .route("/api/v1/acquisition/requests", post(create).get(list))
        .route("/api/v1/acquisition/requests/{id}", get(detail))
}

pub(crate) async fn create(
    State(state): State<AcquisitionState>,
    Extension(context): Extension<RequestContext>,
    AuthenticatedPrincipal(principal): AuthenticatedPrincipal,
    payload: Result<Json<serde_json::Value>, JsonRejection>,
) -> Result<Response, AcquisitionApiError> {
    require_admin(&principal, &context)?;
    let service = service(&state, &context)?;
    let Json(body) = payload.map_err(|_| AcquisitionApiError::Invalid(context.clone()))?;
    let origin =
        origin_from_value(&body).ok_or_else(|| AcquisitionApiError::Invalid(context.clone()))?;
    let outcome = service
        .request(origin)
        .await
        .map_err(|error| failure(&context, error))?;
    let body = RequestBody::from(&outcome.request);

    if outcome.created {
        Ok((StatusCode::CREATED, Json(body)).into_response())
    } else {
        Ok((StatusCode::OK, Json(body)).into_response())
    }
}

pub(crate) async fn list(
    State(state): State<AcquisitionState>,
    Extension(context): Extension<RequestContext>,
    AuthenticatedPrincipal(principal): AuthenticatedPrincipal,
    query: RawQuery,
) -> Result<Json<Vec<RequestBody>>, AcquisitionApiError> {
    require_admin(&principal, &context)?;
    let service = service(&state, &context)?;

    if unknown_query(&query, &["origin"]) {
        return Err(AcquisitionApiError::Invalid(context));
    }

    let raw = query.all("origin");
    if raw.is_empty() || raw.len() > MAX_ORIGINS {
        return Err(AcquisitionApiError::Invalid(context));
    }

    let mut origins = Vec::with_capacity(raw.len());
    for id in raw {
        let Ok(id) = uuid::Uuid::parse_str(id) else {
            return Err(AcquisitionApiError::Invalid(context));
        };

        origins.push(id.to_string());
    }

    let requests = service
        .list_by_origin(&origins)
        .await
        .map_err(|error| failure(&context, error))?;

    Ok(Json(
        requests.iter().map(RequestBody::from).collect::<Vec<_>>(),
    ))
}

pub(crate) async fn detail(
    State(state): State<AcquisitionState>,
    Extension(context): Extension<RequestContext>,
    AuthenticatedPrincipal(principal): AuthenticatedPrincipal,
    Path(raw_id): Path<String>,
    query: RawQuery,
) -> Result<Json<RequestBody>, AcquisitionApiError> {
    require_admin(&principal, &context)?;
    let service = service(&state, &context)?;

    if unknown_query(&query, &[]) {
        return Err(AcquisitionApiError::Invalid(context));
    }

    let id = uuid::Uuid::parse_str(&raw_id)
        .map_err(|_| AcquisitionApiError::NotFound(context.clone()))?;
    let request = service
        .get(id)
        .await
        .map_err(|error| failure(&context, error))?
        .ok_or_else(|| AcquisitionApiError::NotFound(context.clone()))?;

    Ok(Json(RequestBody::from(&request)))
}
