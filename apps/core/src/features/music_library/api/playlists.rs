use std::sync::Arc;

use axum::{
    Extension, Json,
    extract::{Path, State, rejection::JsonRejection},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};

use crate::{
    features::identity::api::native::AuthenticatedPrincipal,
    features::music_library::{
        api::errors::ApiError,
        api::native::{TrackPageItem, offset, page_limit, page_response, reject_unknown, rejected},
        catalogue::{CatalogueError, LibraryService, ValidationError},
        domain::{
            CommentPatch, MoveTarget, OccurrenceSelector, Playlist, PlaylistEntryChange,
            PlaylistEntryError, PlaylistEntryId, PlaylistId, PlaylistMetadataPatch, TrackId,
        },
        playlists::{PlaylistDetail, PlaylistEntry, PlaylistError, PlaylistService},
    },
    platform::{http_response::error_response, raw_query::RawQuery, request::RequestContext},
};

#[derive(Clone)]
pub(crate) struct PlaylistsState {
    pub(crate) playlists: Arc<PlaylistService>,
    pub(crate) library: Arc<LibraryService>,
}

const LIST_PARAMETERS: &[&str] = &["limit", "offset"];

pub(crate) enum PlaylistApiError {
    Native(ApiError),
    Unavailable(RequestContext),
    Forbidden(RequestContext),
    Conflict(RequestContext),
}

impl IntoResponse for PlaylistApiError {
    fn into_response(self) -> Response {
        match self {
            Self::Native(error) => error.into_response(),
            Self::Unavailable(context) => error_response(
                StatusCode::SERVICE_UNAVAILABLE,
                "playlists_unavailable",
                "Playlists are temporarily unavailable",
                context,
            ),
            Self::Forbidden(context) => {
                error_response(StatusCode::FORBIDDEN, "forbidden", "Forbidden", context)
            }
            Self::Conflict(context) => {
                tracing::debug!(
                    request_id = context.request_id().as_str(),
                    "playlist revision conflict"
                );

                error_response(
                    StatusCode::CONFLICT,
                    "conflict",
                    "The playlist has changed",
                    context,
                )
            }
        }
    }
}

fn invalid(context: &RequestContext, error: ValidationError) -> PlaylistApiError {
    PlaylistApiError::Native(ApiError::InvalidRequest {
        context: context.clone(),
        error,
    })
}

fn invalid_field(context: &RequestContext, field: &'static str) -> PlaylistApiError {
    invalid(context, ValidationError::Invalid { field })
}

fn malformed(context: &RequestContext, error: JsonRejection) -> PlaylistApiError {
    tracing::debug!(error = %error, "malformed playlist request");

    PlaylistApiError::Native(ApiError::MalformedRequest {
        context: context.clone(),
        error,
    })
}

fn not_found(context: &RequestContext) -> PlaylistApiError {
    PlaylistApiError::Native(ApiError::NotFound(context.clone()))
}

fn forbidden(context: &RequestContext) -> PlaylistApiError {
    PlaylistApiError::Forbidden(context.clone())
}

fn conflict(context: &RequestContext) -> PlaylistApiError {
    PlaylistApiError::Conflict(context.clone())
}

fn entry_field(error: PlaylistEntryError) -> &'static str {
    match error {
        PlaylistEntryError::UnknownTrack { .. } => "additions",
        PlaylistEntryError::UnknownEntry { .. } => "removals",
        PlaylistEntryError::IndexOutOfRange { .. } => "removals",
    }
}

pub(crate) fn failure(context: &RequestContext, error: PlaylistError) -> PlaylistApiError {
    match error {
        PlaylistError::Disabled => {
            tracing::error!(
                request_id = context.request_id().as_str(),
                "playlists are not configured"
            );

            PlaylistApiError::Unavailable(context.clone())
        }
        PlaylistError::Repository(error) => PlaylistApiError::Native(ApiError::Catalogue {
            context: context.clone(),
            error: CatalogueError::Repository(error),
        }),
        PlaylistError::Library(error) => PlaylistApiError::Native(ApiError::Catalogue {
            context: context.clone(),
            error,
        }),
        PlaylistError::Conflict { current_revision } => {
            tracing::debug!(
                current_revision,
                request_id = context.request_id().as_str(),
                "playlist revision conflict"
            );

            conflict(context)
        }
        PlaylistError::Entry(error) => invalid(
            context,
            ValidationError::Invalid {
                field: entry_field(error),
            },
        ),
        PlaylistError::Forbidden => forbidden(context),
        PlaylistError::NotFound => not_found(context),
    }
}

#[derive(Serialize)]
pub(crate) struct PlaylistOwner {
    id: String,
    username: String,
}

#[derive(Serialize)]
pub(crate) struct PlaylistItem {
    id: String,
    name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    comment: Option<String>,
    public: bool,
    owner: PlaylistOwner,
    entry_count: u32,
    revision: i64,
    created_at: String,
    updated_at: String,
}

impl PlaylistItem {
    pub(crate) fn from_playlist(playlist: &Playlist) -> Self {
        Self {
            id: playlist.id().to_string(),
            name: playlist.name().to_owned(),
            comment: playlist.comment().map(str::to_owned),
            public: playlist.is_public(),
            owner: PlaylistOwner {
                id: playlist.owner_id().to_string(),
                username: playlist.owner_username().to_owned(),
            },
            entry_count: playlist.track_count(),
            revision: playlist.revision(),
            created_at: playlist.created_at().to_rfc3339(),
            updated_at: playlist.updated_at().to_rfc3339(),
        }
    }
}

#[derive(Serialize)]
pub(crate) struct PlaylistEntryItem {
    entry_id: String,
    position: u32,
    track: TrackPageItem,
}

impl PlaylistEntryItem {
    fn from_entry(library: &LibraryService, entry: &PlaylistEntry) -> Self {
        let view = entry.track();

        Self {
            entry_id: entry.entry_id().to_string(),
            position: entry.position(),
            track: TrackPageItem::from_view(view, library.availability(view.sources())),
        }
    }
}

#[derive(Serialize)]
pub(crate) struct PlaylistDetailResponse {
    #[serde(flatten)]
    playlist: PlaylistItem,
    editable: bool,
    entries: Vec<PlaylistEntryItem>,
}

impl PlaylistDetailResponse {
    fn from_detail(library: &LibraryService, detail: &PlaylistDetail) -> Self {
        Self {
            playlist: PlaylistItem::from_playlist(&detail.playlist),
            editable: detail.editable,
            entries: detail
                .entries
                .iter()
                .map(|entry| PlaylistEntryItem::from_entry(library, entry))
                .collect(),
        }
    }
}

#[derive(Serialize)]
pub(crate) struct RevisionResponse {
    revision: i64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CreateRequest {
    name: String,
    #[serde(default)]
    comment: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct EditRequest {
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

impl EditRequest {
    fn metadata(
        &self,
        context: &RequestContext,
    ) -> Result<PlaylistMetadataPatch, PlaylistApiError> {
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

    fn entries(&self, context: &RequestContext) -> Result<PlaylistEntryChange, PlaylistApiError> {
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

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct MoveRequest {
    before: u32,
    #[serde(default)]
    revision: Option<i64>,
}

fn list_window(query: &RawQuery) -> Result<(u32, u32), ValidationError> {
    reject_unknown(query, LIST_PARAMETERS)?;

    Ok((offset(query)?, page_limit(query)?.get()))
}

fn playlist_id(raw: &str, context: &RequestContext) -> Result<PlaylistId, PlaylistApiError> {
    raw.parse::<PlaylistId>()
        .map_err(|_| invalid_field(context, "playlist_id"))
}

fn entry_id(raw: &str, context: &RequestContext) -> Result<PlaylistEntryId, PlaylistApiError> {
    raw.parse::<PlaylistEntryId>()
        .map_err(|_| invalid_field(context, "entry_id"))
}

async fn refusal(
    state: &PlaylistsState,
    user: uuid::Uuid,
    playlist_id: &PlaylistId,
    context: &RequestContext,
) -> PlaylistApiError {
    match state.playlists.detail(user, playlist_id).await {
        Ok(Some(detail)) if !detail.editable => forbidden(context),
        Ok(Some(_)) | Ok(None) => not_found(context),
        Err(error) => failure(context, error),
    }
}

pub(crate) async fn list(
    State(state): State<PlaylistsState>,
    Extension(context): Extension<RequestContext>,
    AuthenticatedPrincipal(principal): AuthenticatedPrincipal,
    query: RawQuery,
) -> Result<Response, PlaylistApiError> {
    let (offset, limit) = list_window(&query).map_err(|error| invalid(&context, error))?;
    let playlists = state
        .playlists
        .list(principal.user_id.as_uuid())
        .await
        .map_err(|error| failure(&context, error))?;
    let total = u32::try_from(playlists.len()).unwrap_or(u32::MAX);
    let items = playlists
        .into_iter()
        .skip(offset as usize)
        .take(limit as usize)
        .map(|playlist| PlaylistItem::from_playlist(&playlist))
        .collect();

    Ok(page_response(items, total, None))
}

pub(crate) async fn create(
    State(state): State<PlaylistsState>,
    Extension(context): Extension<RequestContext>,
    AuthenticatedPrincipal(principal): AuthenticatedPrincipal,
    payload: Result<Json<CreateRequest>, JsonRejection>,
) -> Result<Response, PlaylistApiError> {
    let Json(request) = payload.map_err(|error| malformed(&context, error))?;
    let name = Playlist::validated_name(&request.name)
        .map_err(|error| invalid(&context, rejected(error, "name")))?;
    let comment = Playlist::validated_comment(request.comment.as_deref())
        .map_err(|error| invalid(&context, rejected(error, "comment")))?;
    let playlist = state
        .playlists
        .create(principal.user_id.as_uuid(), &name, comment.as_deref())
        .await
        .map_err(|error| failure(&context, error))?;

    Ok((
        StatusCode::CREATED,
        Json(PlaylistItem::from_playlist(&playlist)),
    )
        .into_response())
}

pub(crate) async fn detail(
    State(state): State<PlaylistsState>,
    Extension(context): Extension<RequestContext>,
    AuthenticatedPrincipal(principal): AuthenticatedPrincipal,
    Path(raw_id): Path<String>,
    query: RawQuery,
) -> Result<Json<PlaylistDetailResponse>, PlaylistApiError> {
    reject_unknown(&query, &[]).map_err(|error| invalid(&context, error))?;
    let playlist_id = playlist_id(&raw_id, &context)?;
    let detail = state
        .playlists
        .detail(principal.user_id.as_uuid(), &playlist_id)
        .await
        .map_err(|error| failure(&context, error))?
        .ok_or_else(|| not_found(&context))?;

    Ok(Json(PlaylistDetailResponse::from_detail(
        &state.library,
        &detail,
    )))
}

pub(crate) async fn update(
    State(state): State<PlaylistsState>,
    Extension(context): Extension<RequestContext>,
    AuthenticatedPrincipal(principal): AuthenticatedPrincipal,
    Path(raw_id): Path<String>,
    payload: Result<Json<EditRequest>, JsonRejection>,
) -> Result<Json<PlaylistItem>, PlaylistApiError> {
    let Json(request) = payload.map_err(|error| malformed(&context, error))?;
    let playlist_id = playlist_id(&raw_id, &context)?;
    let user = principal.user_id.as_uuid();
    let metadata = request.metadata(&context)?;
    let entries = request.entries(&context)?;
    let applied = state
        .playlists
        .write_at_revision(user, &playlist_id, request.revision, metadata, entries)
        .await
        .map_err(|error| failure(&context, error))?;

    match applied {
        Some(playlist) => Ok(Json(PlaylistItem::from_playlist(&playlist))),
        None => Err(refusal(&state, user, &playlist_id, &context).await),
    }
}

pub(crate) async fn move_entry(
    State(state): State<PlaylistsState>,
    Extension(context): Extension<RequestContext>,
    AuthenticatedPrincipal(principal): AuthenticatedPrincipal,
    Path((raw_id, raw_entry_id)): Path<(String, String)>,
    payload: Result<Json<MoveRequest>, JsonRejection>,
) -> Result<Json<RevisionResponse>, PlaylistApiError> {
    let Json(request) = payload.map_err(|error| malformed(&context, error))?;
    let playlist_id = playlist_id(&raw_id, &context)?;
    let entry_id = entry_id(&raw_entry_id, &context)?;
    let user = principal.user_id.as_uuid();
    let moved = state
        .playlists
        .move_entry(
            user,
            &playlist_id,
            OccurrenceSelector::Entry(entry_id),
            MoveTarget::Before(request.before),
            request.revision,
        )
        .await
        .map_err(|error| failure(&context, error))?;
    if !moved {
        return Err(refusal(&state, user, &playlist_id, &context).await);
    }

    let detail = state
        .playlists
        .detail(user, &playlist_id)
        .await
        .map_err(|error| failure(&context, error))?
        .ok_or_else(|| not_found(&context))?;

    Ok(Json(RevisionResponse {
        revision: detail.playlist.revision(),
    }))
}

pub(crate) async fn remove(
    State(state): State<PlaylistsState>,
    Extension(context): Extension<RequestContext>,
    AuthenticatedPrincipal(principal): AuthenticatedPrincipal,
    Path(raw_id): Path<String>,
) -> Result<StatusCode, PlaylistApiError> {
    let playlist_id = playlist_id(&raw_id, &context)?;
    let user = principal.user_id.as_uuid();
    let deleted = state
        .playlists
        .delete(user, &playlist_id)
        .await
        .map_err(|error| failure(&context, error))?;
    if !deleted {
        return Err(refusal(&state, user, &playlist_id, &context).await);
    }

    Ok(StatusCode::NO_CONTENT)
}
