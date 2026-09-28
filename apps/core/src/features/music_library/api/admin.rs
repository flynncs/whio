use std::sync::Arc;

use axum::{
    Extension, Json, Router,
    extract::{FromRef, Path, State, rejection::JsonRejection},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{patch, put},
};
use serde::Deserialize;

use crate::{
    features::identity::api::native::{AuthenticatedPrincipal, NativeState},
    features::identity::domain::Principal,
    features::identity::service::CredentialService,
    features::music_library::{
        api::errors::ApiError,
        api::native::rejected,
        api::playlists::{PlaylistApiError, PlaylistItem, failure as playlist_failure},
        catalogue::ValidationError,
        domain::{
            CommentPatch, OccurrenceSelector, Playlist, PlaylistEntryChange, PlaylistEntryId,
            PlaylistId, PlaylistMetadataPatch, TrackId,
        },
        playlists::PlaylistService,
    },
    platform::{http_response::error_response, request::RequestContext},
};

#[derive(Clone)]
pub(crate) struct AdminState {
    pub(crate) playlists: Arc<PlaylistService>,
    pub(crate) credential: Arc<CredentialService>,
}

impl FromRef<AdminState> for NativeState {
    fn from_ref(state: &AdminState) -> Self {
        Self(Arc::clone(&state.credential))
    }
}

pub(crate) enum AdminApiError {
    Playlist(PlaylistApiError),
    Internal(RequestContext),
}

impl From<PlaylistApiError> for AdminApiError {
    fn from(error: PlaylistApiError) -> Self {
        Self::Playlist(error)
    }
}

impl IntoResponse for AdminApiError {
    fn into_response(self) -> Response {
        match self {
            Self::Playlist(error) => error.into_response(),
            Self::Internal(context) => error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal_error",
                "An unexpected error occurred",
                context,
            ),
        }
    }
}

fn not_found(context: &RequestContext) -> AdminApiError {
    AdminApiError::Playlist(PlaylistApiError::Native(ApiError::NotFound(
        context.clone(),
    )))
}

fn forbidden(context: &RequestContext) -> AdminApiError {
    AdminApiError::Playlist(PlaylistApiError::Forbidden(context.clone()))
}

fn invalid(context: &RequestContext, error: ValidationError) -> AdminApiError {
    AdminApiError::Playlist(PlaylistApiError::Native(ApiError::InvalidRequest {
        context: context.clone(),
        error,
    }))
}

fn invalid_field(context: &RequestContext, field: &'static str) -> AdminApiError {
    invalid(context, ValidationError::Invalid { field })
}

fn malformed(context: &RequestContext, error: JsonRejection) -> AdminApiError {
    tracing::debug!(error = %error, "malformed admin request");

    AdminApiError::Playlist(PlaylistApiError::Native(ApiError::MalformedRequest {
        context: context.clone(),
        error,
    }))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct TransferOwnerRequest {
    username: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct AdminEditRequest {
    #[serde(default)]
    revision: Option<i64>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    comment: Option<Option<String>>,
    #[serde(default)]
    public: Option<bool>,
    #[serde(default)]
    removals: Vec<String>,
    #[serde(default)]
    additions: Vec<String>,
}

impl AdminEditRequest {
    fn metadata(&self, context: &RequestContext) -> Result<PlaylistMetadataPatch, AdminApiError> {
        let name = match &self.name {
            Some(name) => Some(
                Playlist::validated_name(name)
                    .map_err(|error| invalid(context, rejected(error, "name")))?,
            ),
            None => None,
        };
        let comment = match &self.comment {
            Some(comment) => CommentPatch::Set(
                Playlist::validated_comment(comment.as_deref())
                    .map_err(|error| invalid(context, rejected(error, "comment")))?,
            ),
            None => CommentPatch::Keep,
        };

        Ok(PlaylistMetadataPatch {
            name,
            comment,
            public: self.public,
        })
    }

    fn entries(&self, context: &RequestContext) -> Result<PlaylistEntryChange, AdminApiError> {
        let removals = self
            .removals
            .iter()
            .map(|raw| {
                raw.parse::<PlaylistEntryId>()
                    .map_err(|_| invalid_field(context, "removals"))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let additions = self
            .additions
            .iter()
            .map(|raw| {
                raw.parse::<TrackId>()
                    .map_err(|_| invalid_field(context, "additions"))
            })
            .collect::<Result<Vec<_>, _>>()?;

        if removals.is_empty() && additions.is_empty() {
            return Ok(PlaylistEntryChange::Unchanged);
        }

        Ok(PlaylistEntryChange::Apply {
            removals: removals
                .into_iter()
                .map(OccurrenceSelector::Entry)
                .collect(),
            additions,
        })
    }
}

fn playlist_id(raw: &str, context: &RequestContext) -> Result<PlaylistId, AdminApiError> {
    raw.parse::<PlaylistId>()
        .map_err(|_| invalid_field(context, "playlist_id"))
}

async fn refusal(
    state: &AdminState,
    caller: &Principal,
    playlist_id: &PlaylistId,
    context: &RequestContext,
) -> AdminApiError {
    match state
        .playlists
        .detail(caller.user_id.as_uuid(), playlist_id)
        .await
    {
        Ok(Some(detail)) if !detail.editable => forbidden(context),
        Ok(Some(_)) | Ok(None) => not_found(context),
        Err(error) => AdminApiError::Playlist(playlist_failure(context, error)),
    }
}

pub(crate) async fn transfer_owner(
    State(state): State<AdminState>,
    Extension(context): Extension<RequestContext>,
    AuthenticatedPrincipal(principal): AuthenticatedPrincipal,
    Path(raw_id): Path<String>,
    payload: Result<Json<TransferOwnerRequest>, JsonRejection>,
) -> Result<Json<PlaylistItem>, AdminApiError> {
    let Json(request) = payload.map_err(|error| malformed(&context, error))?;
    let playlist_id = playlist_id(&raw_id, &context)?;
    let username = request.username.trim();
    if username.is_empty() {
        return Err(invalid_field(&context, "username"));
    }

    let Some(owner) = state
        .credential
        .visible_user(&principal, username)
        .await
        .map_err(|error| {
            tracing::error!(
                error = %error,
                request_id = context.request_id().as_str(),
                "user directory read failed"
            );

            AdminApiError::Internal(context.clone())
        })?
    else {
        return Err(not_found(&context));
    };

    let playlist = state
        .playlists
        .transfer_owner(&principal, &playlist_id, owner.id)
        .await
        .map_err(|error| playlist_failure(&context, error))?;

    Ok(Json(PlaylistItem::from_playlist(&playlist)))
}

pub(crate) async fn edit(
    State(state): State<AdminState>,
    Extension(context): Extension<RequestContext>,
    AuthenticatedPrincipal(principal): AuthenticatedPrincipal,
    Path(raw_id): Path<String>,
    payload: Result<Json<AdminEditRequest>, JsonRejection>,
) -> Result<Json<PlaylistItem>, AdminApiError> {
    let Json(request) = payload.map_err(|error| malformed(&context, error))?;
    let playlist_id = playlist_id(&raw_id, &context)?;
    let metadata = request.metadata(&context)?;
    let entries = request.entries(&context)?;
    let applied = state
        .playlists
        .write_at_revision_as(
            &principal,
            &playlist_id,
            request.revision,
            metadata,
            entries,
        )
        .await
        .map_err(|error| playlist_failure(&context, error))?;

    match applied {
        Some(playlist) => Ok(Json(PlaylistItem::from_playlist(&playlist))),
        None => Err(refusal(&state, &principal, &playlist_id, &context).await),
    }
}

pub(crate) fn routes() -> Router<AdminState> {
    Router::new()
        .route("/api/v1/admin/playlists/{id}/owner", put(transfer_owner))
        .route("/api/v1/admin/playlists/{id}", patch(edit))
}

#[cfg(all(test, feature = "postgres-integration"))]
#[path = "admin_tests.rs"]
mod tests;
