use std::sync::Arc;

use axum::{
    Extension, Json, Router,
    extract::{FromRef, State, rejection::JsonRejection},
    http::{HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    features::identity::{
        api::native::{AuthenticatedPrincipal, NativeState},
        service::CredentialService,
    },
    features::music_library::{
        api::errors::ApiError,
        api::native::{offset, page_limit, page_response, reject_unknown},
        catalogue::{CatalogueError, ValidationError},
        delivery::{DeliveryError, DeliveryService, DeliveryState},
        domain::{
            ListenCoverage, ListenExtent, ListenOrder, ListenOrigin, ListenOutcome, ListenQuery,
            ListenRecord, ListenSessionId, ListenSubmission, ListenWindow, TrackId,
        },
        history::{ListenError, ListenService},
    },
    features::playback::{
        ActivityError, ActivitySession, PlaybackActivityService, PlaybackClient, PlaybackRate,
        PlaybackReport, PlaybackState,
    },
    platform::{http_response::error_response, raw_query::RawQuery, request::RequestContext},
};

#[derive(Clone)]
pub(crate) struct HistoryState {
    pub(crate) listen: Arc<ListenService>,
    pub(crate) playback_activity: Arc<PlaybackActivityService>,
}

const SUBMISSION_PARAMETERS: &[&str] = &[];
const ACTIVITY_PARAMETERS: &[&str] = &[];
const HISTORY_PARAMETERS: &[&str] = &["limit", "offset", "order", "since", "until"];
const DELIVERY_PARAMETERS: &[&str] = &["limit", "state"];

pub(crate) enum HistoryApiError {
    Native(ApiError),
    Unavailable(RequestContext),
    Internal(RequestContext),
}

impl IntoResponse for HistoryApiError {
    fn into_response(self) -> Response {
        match self {
            Self::Native(error) => error.into_response(),
            Self::Unavailable(context) => error_response(
                StatusCode::SERVICE_UNAVAILABLE,
                "history_unavailable",
                "The listen history is temporarily unavailable",
                context,
            ),
            Self::Internal(context) => error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal_error",
                "An unexpected error occurred",
                context,
            ),
        }
    }
}

fn invalid(context: &RequestContext, error: ValidationError) -> HistoryApiError {
    HistoryApiError::Native(ApiError::InvalidRequest {
        context: context.clone(),
        error,
    })
}

fn invalid_field(context: &RequestContext, field: &'static str) -> HistoryApiError {
    invalid(context, ValidationError::Invalid { field })
}

fn malformed(context: &RequestContext, error: JsonRejection) -> HistoryApiError {
    tracing::debug!(error = %error, "malformed listen request");

    HistoryApiError::Native(ApiError::MalformedRequest {
        context: context.clone(),
        error,
    })
}

fn listen_failure(context: &RequestContext, error: ListenError) -> HistoryApiError {
    match error {
        ListenError::UnknownTrack => invalid_field(context, "track_id"),
        ListenError::Disabled => HistoryApiError::Unavailable(context.clone()),
        ListenError::Repository(error) => HistoryApiError::Native(ApiError::Catalogue {
            context: context.clone(),
            error: CatalogueError::Repository(error),
        }),
    }
}

fn activity_failure(context: &RequestContext, error: ActivityError) -> HistoryApiError {
    match error {
        ActivityError::UnknownTrack => invalid_field(context, "track_id"),
        ActivityError::SessionIdentity => {
            tracing::error!(
                request_id = context.request_id().as_str(),
                "a live session identity could not be minted"
            );

            HistoryApiError::Internal(context.clone())
        }
        ActivityError::Catalogue(error) => HistoryApiError::Native(ApiError::Catalogue {
            context: context.clone(),
            error,
        }),
        ActivityError::Submission(error) => listen_failure(context, error),
    }
}

#[derive(Serialize)]
pub(crate) struct ExtentBody {
    start_position_ms: u64,
    listened_ms: u64,
    qualified: bool,
}

impl ExtentBody {
    fn from_extent(extent: &ListenExtent) -> Self {
        Self {
            start_position_ms: extent.start_position_ms(),
            listened_ms: extent.listened_ms(),
            qualified: extent.qualified(),
        }
    }
}

#[derive(Serialize)]
pub(crate) struct ListenBody {
    track_id: String,
    origin: &'static str,
    listen_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    extent: Option<ExtentBody>,
}

impl ListenBody {
    fn from_submission(submission: &ListenSubmission) -> Self {
        Self {
            track_id: submission.track_id().to_string(),
            origin: submission.origin().as_str(),
            listen_at: submission.listen_at().to_rfc3339(),
            session_id: submission.session_id().map(|id| id.as_str().to_owned()),
            extent: submission
                .extent()
                .map(|extent| ExtentBody::from_extent(&extent)),
        }
    }

    fn from_record(record: &ListenRecord) -> Self {
        Self {
            track_id: record.track_id().to_string(),
            origin: record.origin().as_str(),
            listen_at: record.listen_at().to_rfc3339(),
            session_id: record.session_id().map(|id| id.as_str().to_owned()),
            extent: record
                .extent()
                .map(|extent| ExtentBody::from_extent(&extent)),
        }
    }
}

#[derive(Serialize)]
pub(crate) struct CoverageBody {
    since: Option<String>,
}

impl CoverageBody {
    fn from_coverage(coverage: ListenCoverage) -> Self {
        Self {
            since: coverage.since().map(|since| since.to_rfc3339()),
        }
    }
}

#[derive(Serialize)]
pub(crate) struct HistoryResponse {
    items: Vec<ListenBody>,
    total: u32,
    total_kind: &'static str,
    coverage: CoverageBody,
}

#[derive(Serialize)]
pub(crate) struct SubmissionResponse {
    outcome: &'static str,
    item: ListenBody,
}

#[derive(Serialize)]
pub(crate) struct ActivityBody {
    client: String,
    session_id: String,
    track_id: String,
    state: &'static str,
    position_ms: u64,
    playback_rate: f64,
    listened_ms: u64,
    started_at: String,
    last_event_at: String,
    submitted: bool,
}

impl ActivityBody {
    fn from_session(session: &ActivitySession) -> Self {
        Self {
            client: session.client().as_str().to_owned(),
            session_id: session.session_id().as_str().to_owned(),
            track_id: session.track_id().to_string(),
            state: state_name(session.state()),
            position_ms: session.position_ms(),
            playback_rate: session.playback_rate().get(),
            listened_ms: session.listened_ms(),
            started_at: session.started_at().to_rfc3339(),
            last_event_at: session.last_event_at().to_rfc3339(),
            submitted: session.submitted(),
        }
    }
}

fn state_name(state: PlaybackState) -> &'static str {
    match state {
        PlaybackState::Starting => "starting",
        PlaybackState::Playing => "playing",
        PlaybackState::Paused => "paused",
        PlaybackState::Stopped => "stopped",
    }
}

fn parse_state(raw: &str) -> Option<PlaybackState> {
    match raw {
        "starting" => Some(PlaybackState::Starting),
        "playing" => Some(PlaybackState::Playing),
        "paused" => Some(PlaybackState::Paused),
        "stopped" => Some(PlaybackState::Stopped),
        _ => None,
    }
}

fn outcome_name(outcome: ListenOutcome) -> &'static str {
    match outcome {
        ListenOutcome::Recorded => "recorded",
        ListenOutcome::Duplicate => "duplicate",
    }
}

fn instant(raw: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(raw)
        .ok()
        .map(|value| value.with_timezone(&Utc))
}

fn instant_parameter(
    query: &RawQuery,
    field: &'static str,
) -> Result<Option<DateTime<Utc>>, ValidationError> {
    match query.get(field) {
        Some(raw) => instant(raw)
            .map(Some)
            .ok_or(ValidationError::Invalid { field }),
        None => Ok(None),
    }
}

fn listen_order(query: &RawQuery) -> Result<ListenOrder, ValidationError> {
    match query.get("order") {
        Some("newest") => Ok(ListenOrder::NewestFirst),
        Some("oldest") => Ok(ListenOrder::OldestFirst),
        Some(_) => Err(ValidationError::Invalid { field: "order" }),
        None => Ok(ListenOrder::default()),
    }
}

fn history_query(query: &RawQuery) -> Result<ListenQuery, ValidationError> {
    let window = ListenWindow::new(
        instant_parameter(query, "since")?,
        instant_parameter(query, "until")?,
    );

    Ok(ListenQuery::new(
        window,
        listen_order(query)?,
        page_limit(query)?,
        offset(query)?,
    ))
}

fn total_kind(window: ListenWindow) -> &'static str {
    if window.is_unbounded() {
        "all_time_plays"
    } else {
        "windowed_plays"
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ExtentRequest {
    start_position_ms: u64,
    listened_ms: u64,
    qualified: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SubmissionRequest {
    track_id: String,
    origin: String,
    at: String,
    #[serde(default)]
    session_id: Option<String>,
    #[serde(default)]
    extent: Option<ExtentRequest>,
}

impl SubmissionRequest {
    fn submission(
        &self,
        context: &RequestContext,
        principal: Uuid,
    ) -> Result<ListenSubmission, HistoryApiError> {
        let track_id = self
            .track_id
            .parse::<TrackId>()
            .map_err(|_| invalid_field(context, "track_id"))?;
        let origin =
            ListenOrigin::parse(&self.origin).ok_or_else(|| invalid_field(context, "origin"))?;
        let listen_at = instant(&self.at).ok_or_else(|| invalid_field(context, "at"))?;
        let session_id = self
            .session_id
            .as_ref()
            .map(|raw| {
                ListenSessionId::new(raw.clone()).map_err(|_| invalid_field(context, "session_id"))
            })
            .transpose()?;
        let extent = self.extent.as_ref().map(|extent| {
            ListenExtent::new(
                extent.start_position_ms,
                extent.listened_ms,
                extent.qualified,
            )
        });

        Ok(ListenSubmission::new(
            principal, origin, track_id, listen_at, session_id, extent,
        ))
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ReportRequest {
    client: String,
    track_id: String,
    state: String,
    position_ms: u64,
    at: String,
    #[serde(default)]
    playback_rate: Option<f64>,
}

impl ReportRequest {
    fn report(
        &self,
        context: &RequestContext,
        principal: Uuid,
    ) -> Result<PlaybackReport, HistoryApiError> {
        let client = PlaybackClient::new(self.client.clone())
            .map_err(|_| invalid_field(context, "client"))?;
        let track_id = self
            .track_id
            .parse::<TrackId>()
            .map_err(|_| invalid_field(context, "track_id"))?;
        let state = parse_state(&self.state).ok_or_else(|| invalid_field(context, "state"))?;
        let listen_at = instant(&self.at).ok_or_else(|| invalid_field(context, "at"))?;
        let playback_rate = match self.playback_rate {
            Some(rate) => {
                PlaybackRate::new(rate).map_err(|_| invalid_field(context, "playback_rate"))?
            }
            None => PlaybackRate::default(),
        };

        Ok(PlaybackReport::new(
            principal,
            client,
            track_id,
            state,
            self.position_ms,
            playback_rate,
            listen_at,
        ))
    }
}

pub(crate) async fn submit(
    State(state): State<HistoryState>,
    Extension(context): Extension<RequestContext>,
    AuthenticatedPrincipal(principal): AuthenticatedPrincipal,
    query: RawQuery,
    payload: Result<Json<SubmissionRequest>, JsonRejection>,
) -> Result<Json<SubmissionResponse>, HistoryApiError> {
    reject_unknown(&query, SUBMISSION_PARAMETERS).map_err(|error| invalid(&context, error))?;
    let Json(request) = payload.map_err(|error| malformed(&context, error))?;
    let submission = request.submission(&context, principal.user_id.as_uuid())?;
    let outcome = state
        .listen
        .submit_listen(&submission)
        .await
        .map_err(|error| listen_failure(&context, error))?;

    Ok(Json(SubmissionResponse {
        outcome: outcome_name(outcome),
        item: ListenBody::from_submission(&submission),
    }))
}

pub(crate) async fn read(
    State(state): State<HistoryState>,
    Extension(context): Extension<RequestContext>,
    AuthenticatedPrincipal(principal): AuthenticatedPrincipal,
    query: RawQuery,
) -> Result<Response, HistoryApiError> {
    reject_unknown(&query, HISTORY_PARAMETERS).map_err(|error| invalid(&context, error))?;
    let request = history_query(&query).map_err(|error| invalid(&context, error))?;
    let user = principal.user_id.as_uuid();
    let page = state
        .listen
        .history(user, &request)
        .await
        .map_err(|error| listen_failure(&context, error))?;
    let count = state
        .listen
        .count(user, request.window(), None)
        .await
        .map_err(|error| listen_failure(&context, error))?;
    let total = u32::try_from(count.count()).unwrap_or(u32::MAX);
    let items = page
        .entries()
        .iter()
        .map(ListenBody::from_record)
        .collect::<Vec<_>>();

    let mut response = Json(HistoryResponse {
        items,
        total,
        total_kind: total_kind(request.window()),
        coverage: CoverageBody::from_coverage(page.coverage()),
    })
    .into_response();
    response.headers_mut().insert(
        "x-total-count",
        HeaderValue::from_str(&total.to_string()).expect("a count is a valid header value"),
    );

    Ok(response)
}

#[derive(Clone)]
pub struct ListenDeliveryState {
    pub delivery: Option<Arc<DeliveryService>>,
    pub credential: Arc<CredentialService>,
}

impl FromRef<ListenDeliveryState> for NativeState {
    fn from_ref(state: &ListenDeliveryState) -> Self {
        Self(Arc::clone(&state.credential))
    }
}

pub fn delivery_routes() -> Router<ListenDeliveryState> {
    Router::new().route("/api/v1/library/listen-deliveries", get(delivery_status))
}

fn delivery_failure(context: &RequestContext, error: DeliveryError) -> HistoryApiError {
    match error {
        DeliveryError::Disabled | DeliveryError::Unavailable | DeliveryError::CredentialMissing => {
            HistoryApiError::Unavailable(context.clone())
        }
        DeliveryError::Repository(error) => HistoryApiError::Native(ApiError::Catalogue {
            context: context.clone(),
            error: CatalogueError::Repository(error),
        }),
        DeliveryError::InvalidData
        | DeliveryError::CredentialRejected
        | DeliveryError::ProviderUnavailable
        | DeliveryError::RateLimited { .. } => HistoryApiError::Internal(context.clone()),
    }
}

fn delivery_state(
    context: &RequestContext,
    query: &RawQuery,
) -> Result<Option<DeliveryState>, HistoryApiError> {
    match query.get("state") {
        Some(raw) => DeliveryState::parse(raw)
            .map(Some)
            .ok_or_else(|| invalid_field(context, "state")),
        None => Ok(None),
    }
}

pub(crate) async fn delivery_status(
    State(state): State<ListenDeliveryState>,
    Extension(context): Extension<RequestContext>,
    AuthenticatedPrincipal(principal): AuthenticatedPrincipal,
    query: RawQuery,
) -> Result<Response, HistoryApiError> {
    reject_unknown(&query, DELIVERY_PARAMETERS).map_err(|error| invalid(&context, error))?;
    let delivery = state
        .delivery
        .as_ref()
        .ok_or_else(|| HistoryApiError::Unavailable(context.clone()))?;
    let selected = delivery_state(&context, &query)?;
    let limit = page_limit(&query).map_err(|error| invalid(&context, error))?;
    let rows = delivery
        .status(principal.user_id.as_uuid(), selected, limit.get())
        .await
        .map_err(|error| delivery_failure(&context, error))?;
    let total = u32::try_from(rows.len()).unwrap_or(u32::MAX);

    Ok(page_response(rows, total, None))
}

pub(crate) async fn activity(
    State(state): State<HistoryState>,
    Extension(context): Extension<RequestContext>,
    AuthenticatedPrincipal(principal): AuthenticatedPrincipal,
    query: RawQuery,
) -> Result<Response, HistoryApiError> {
    reject_unknown(&query, ACTIVITY_PARAMETERS).map_err(|error| invalid(&context, error))?;
    let live = state
        .playback_activity
        .now_playing(principal.user_id.as_uuid(), Utc::now());
    let total = u32::try_from(live.len()).unwrap_or(u32::MAX);
    let items = live
        .iter()
        .map(ActivityBody::from_session)
        .collect::<Vec<_>>();

    Ok(page_response(items, total, None))
}

pub(crate) async fn report(
    State(state): State<HistoryState>,
    Extension(context): Extension<RequestContext>,
    AuthenticatedPrincipal(principal): AuthenticatedPrincipal,
    query: RawQuery,
    payload: Result<Json<ReportRequest>, JsonRejection>,
) -> Result<Json<ActivityBody>, HistoryApiError> {
    reject_unknown(&query, ACTIVITY_PARAMETERS).map_err(|error| invalid(&context, error))?;
    let Json(request) = payload.map_err(|error| malformed(&context, error))?;
    let report = request.report(&context, principal.user_id.as_uuid())?;
    let session = state
        .playback_activity
        .report(&report)
        .await
        .map_err(|error| activity_failure(&context, error))?;

    Ok(Json(ActivityBody::from_session(&session)))
}
