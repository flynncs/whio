use std::{collections::HashMap, sync::Arc};

use axum::{
    Extension, Json,
    extract::{State, rejection::JsonRejection},
    http::{HeaderValue, StatusCode},
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    features::identity::api::native::AuthenticatedPrincipal,
    features::music_library::{
        api::errors::ApiError,
        api::native::{TrackPageItem, offset, page_limit, reject_unknown},
        catalogue::{CatalogueError, LibraryService, ValidationError},
        domain::{
            RestoreQueueEntry, RestoreQueueSnapshot, RestoreQueueValidationError, TrackId,
            TrackView,
        },
        queue::{RestoreQueueError, RestoreQueueSave, RestoreQueueService},
        storage::contracts::TrackRepositoryError,
    },
    platform::{http_response::error_response, raw_query::RawQuery, request::RequestContext},
};

#[derive(Clone)]
pub(crate) struct QueueState {
    pub(crate) queue: Arc<RestoreQueueService>,
    pub(crate) library: Arc<LibraryService>,
}

const READ_PARAMETERS: &[&str] = &["limit", "offset"];

pub(crate) enum QueueApiError {
    Native(ApiError),
    Unavailable(RequestContext),
    Conflict(RequestContext),
    Bound(RequestContext),
}

impl IntoResponse for QueueApiError {
    fn into_response(self) -> Response {
        match self {
            Self::Native(error) => error.into_response(),
            Self::Unavailable(context) => error_response(
                StatusCode::SERVICE_UNAVAILABLE,
                "queue_unavailable",
                "The restore queue is temporarily unavailable",
                context,
            ),
            Self::Conflict(context) => error_response(
                StatusCode::CONFLICT,
                "conflict",
                "The restore queue has changed",
                context,
            ),
            Self::Bound(context) => error_response(
                StatusCode::BAD_REQUEST,
                "invalid_request",
                "The restore queue holds at most MAX_RESTORE_QUEUE_ENTRIES entries",
                context,
            ),
        }
    }
}

fn invalid(context: &RequestContext, error: ValidationError) -> QueueApiError {
    QueueApiError::Native(ApiError::InvalidRequest {
        context: context.clone(),
        error,
    })
}

fn invalid_field(context: &RequestContext, field: &'static str) -> QueueApiError {
    invalid(context, ValidationError::Invalid { field })
}

fn malformed(context: &RequestContext, error: JsonRejection) -> QueueApiError {
    tracing::debug!(error = %error, "malformed queue request");

    QueueApiError::Native(ApiError::MalformedRequest {
        context: context.clone(),
        error,
    })
}

fn not_found(context: &RequestContext) -> QueueApiError {
    QueueApiError::Native(ApiError::NotFound(context.clone()))
}

fn validation_field(error: &RestoreQueueValidationError) -> &'static str {
    match error {
        RestoreQueueValidationError::TooManyEntries { .. } => "entries",
        RestoreQueueValidationError::UnknownTrack { .. } => "entries",
        RestoreQueueValidationError::CurrentIndexOutOfRange { .. } => "current_index",
        RestoreQueueValidationError::CurrentIndexMissing { .. } => "current_index",
    }
}

fn failure(context: &RequestContext, error: RestoreQueueError) -> QueueApiError {
    match error {
        RestoreQueueError::Disabled => {
            tracing::error!(
                request_id = context.request_id().as_str(),
                "the restore queue is not configured"
            );

            QueueApiError::Unavailable(context.clone())
        }
        RestoreQueueError::Repository(error) => QueueApiError::Native(ApiError::Catalogue {
            context: context.clone(),
            error: CatalogueError::Repository(error),
        }),
        RestoreQueueError::Conflict { current_revision } => {
            tracing::debug!(
                current_revision,
                request_id = context.request_id().as_str(),
                "restore queue revision conflict"
            );

            QueueApiError::Conflict(context.clone())
        }
        RestoreQueueError::Invalid(error) => {
            tracing::debug!(
                error = %error,
                request_id = context.request_id().as_str(),
                "invalid restore queue request"
            );

            if matches!(error, RestoreQueueValidationError::TooManyEntries { .. }) {
                return QueueApiError::Bound(context.clone());
            }

            invalid_field(context, validation_field(&error))
        }
    }
}

#[derive(Serialize)]
pub(crate) struct QueueEntryItem {
    entry_id: String,
    position: u32,
    track: TrackPageItem,
}

#[derive(Serialize)]
pub(crate) struct QueueResponse {
    items: Vec<QueueEntryItem>,
    total: u32,
    revision: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    current_index: Option<u32>,
    position_ms: u64,
    changed_at: String,
}

async fn items(
    state: &QueueState,
    context: &RequestContext,
    user: &Uuid,
    entries: &[&RestoreQueueEntry],
) -> Result<Vec<QueueEntryItem>, QueueApiError> {
    let ids = entries
        .iter()
        .map(|entry| entry.track_id().clone())
        .collect::<Vec<_>>();
    let views = state
        .library
        .track_views(&ids, Some(user))
        .await
        .map_err(|error| {
            QueueApiError::Native(ApiError::Catalogue {
                context: context.clone(),
                error,
            })
        })?;
    let by_id = views
        .into_iter()
        .map(|view| (view.track().id().clone(), view))
        .collect::<HashMap<TrackId, TrackView>>();

    entries
        .iter()
        .map(|entry| {
            let track_id = entry.track_id();
            let view = by_id.get(track_id).ok_or_else(|| {
                tracing::error!(
                    track_id = %track_id,
                    request_id = context.request_id().as_str(),
                    "a restore queue entry has no track"
                );

                QueueApiError::Native(ApiError::Catalogue {
                    context: context.clone(),
                    error: CatalogueError::Repository(TrackRepositoryError::InvalidData),
                })
            })?;

            Ok(QueueEntryItem {
                entry_id: entry.entry_id().to_string(),
                position: entry.position(),
                track: TrackPageItem::from_view(view, state.library.availability(view.sources())),
            })
        })
        .collect()
}

fn snapshot_response(snapshot: &RestoreQueueSnapshot, items: Vec<QueueEntryItem>) -> Response {
    let total = u32::try_from(snapshot.entries().len()).unwrap_or(u32::MAX);
    let mut response = Json(QueueResponse {
        items,
        total,
        revision: snapshot.revision(),
        current_index: snapshot.current_index(),
        position_ms: snapshot.position_ms(),
        changed_at: snapshot.changed_at().to_rfc3339(),
    })
    .into_response();
    response.headers_mut().insert(
        "x-total-count",
        HeaderValue::from_str(&total.to_string()).expect("a count is a valid header value"),
    );

    response
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SaveRequest {
    entries: Vec<String>,
    #[serde(default)]
    current_index: Option<u32>,
    position_ms: u64,
    #[serde(default)]
    revision: Option<i64>,
}

impl SaveRequest {
    fn save(&self, context: &RequestContext) -> Result<RestoreQueueSave, QueueApiError> {
        let entries = self
            .entries
            .iter()
            .map(|raw| {
                raw.parse::<TrackId>()
                    .map_err(|_| invalid_field(context, "entries"))
            })
            .collect::<Result<Vec<_>, _>>()?;

        Ok(RestoreQueueSave {
            entries,
            current_index: self.current_index,
            position_ms: self.position_ms,
        })
    }
}

pub(crate) async fn read(
    State(state): State<QueueState>,
    Extension(context): Extension<RequestContext>,
    AuthenticatedPrincipal(principal): AuthenticatedPrincipal,
    query: RawQuery,
) -> Result<Response, QueueApiError> {
    reject_unknown(&query, READ_PARAMETERS).map_err(|error| invalid(&context, error))?;
    let offset = offset(&query).map_err(|error| invalid(&context, error))?;
    let limit = page_limit(&query).map_err(|error| invalid(&context, error))?;
    let user = principal.user_id.as_uuid();
    let snapshot = state
        .queue
        .read(user)
        .await
        .map_err(|error| failure(&context, error))?
        .ok_or_else(|| not_found(&context))?;
    let page = snapshot
        .entries()
        .iter()
        .skip(offset as usize)
        .take(limit.get() as usize)
        .collect::<Vec<_>>();
    let items = items(&state, &context, &user, &page).await?;

    Ok(snapshot_response(&snapshot, items))
}

pub(crate) async fn save(
    State(state): State<QueueState>,
    Extension(context): Extension<RequestContext>,
    AuthenticatedPrincipal(principal): AuthenticatedPrincipal,
    query: RawQuery,
    payload: Result<Json<SaveRequest>, JsonRejection>,
) -> Result<Response, QueueApiError> {
    reject_unknown(&query, &[]).map_err(|error| invalid(&context, error))?;
    let Json(request) = payload.map_err(|error| malformed(&context, error))?;
    let user = principal.user_id.as_uuid();
    let saved = state
        .queue
        .save_at_revision(user, request.save(&context)?, request.revision)
        .await
        .map_err(|error| failure(&context, error))?;
    let entries = saved.entries().iter().collect::<Vec<_>>();
    let items = items(&state, &context, &user, &entries).await?;

    Ok(snapshot_response(&saved, items))
}

pub(crate) async fn clear(
    State(state): State<QueueState>,
    Extension(context): Extension<RequestContext>,
    AuthenticatedPrincipal(principal): AuthenticatedPrincipal,
    query: RawQuery,
) -> Result<StatusCode, QueueApiError> {
    reject_unknown(&query, &[]).map_err(|error| invalid(&context, error))?;
    let cleared = state
        .queue
        .clear(principal.user_id.as_uuid())
        .await
        .map_err(|error| failure(&context, error))?;

    if !cleared {
        return Err(not_found(&context));
    }

    Ok(StatusCode::NO_CONTENT)
}
