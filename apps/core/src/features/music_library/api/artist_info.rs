use std::sync::Arc;

use axum::{
    Extension, Json, Router,
    extract::{FromRef, Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::get,
};
use serde::Serialize;
use serde_json::{Value, json};
use uuid::Uuid;

use crate::{
    features::identity::api::native::{AuthenticatedPrincipal, NativeState},
    features::identity::service::CredentialService,
    features::metadata::domain::{
        AbsenceReason, Attribution, BytesState, CandidateItem, Disagreement, FieldEntry,
        FieldValue, Freshness, Presence, StoredFact,
    },
    features::metadata::licences::LicenceTerms,
    features::metadata::{MetadataProfile, RefreshOutcome, RefreshPhase},
    features::music_library::{
        api::errors::ApiError,
        api::native::{page_limit, reject_unknown},
        artwork::ArtworkAssetDescriptor,
        catalogue::{CatalogueError, LibraryService, ProviderError, ValidationError},
        discovery::{
            DiscoveryBasis, DiscoveryError, DiscoveryResult, DiscoverySeed, DiscoveryService,
            FallbackReason, RelatedItem, RelatedRequest, TopItem, TopRequest,
        },
        domain::{ArtistId, ArtistInformation, RefreshDemand, RefreshStatus},
    },
    platform::{http_response::error_response, raw_query::RawQuery, request::RequestContext},
};

#[derive(Clone)]
pub(crate) struct ArtistInfoState {
    pub(crate) library: Arc<LibraryService>,
    pub(crate) credential: Arc<CredentialService>,
    pub(crate) profile: MetadataProfile,
    pub(crate) language: String,
    pub(crate) discovery: Option<Arc<DiscoveryService>>,
}

impl ArtistInfoState {
    pub(crate) fn with_defaults(
        library: Arc<LibraryService>,
        credential: Arc<CredentialService>,
        profile: MetadataProfile,
        language: String,
        discovery: Option<Arc<DiscoveryService>>,
    ) -> Self {
        Self {
            library,
            credential,
            profile,
            language,
            discovery,
        }
    }
}

impl FromRef<ArtistInfoState> for NativeState {
    fn from_ref(state: &ArtistInfoState) -> Self {
        Self(Arc::clone(&state.credential))
    }
}

const INFO_PARAMETERS: &[&str] = &["language", "profile"];
const REFRESH_PARAMETERS: &[&str] = &[];
const RELATED_PARAMETERS: &[&str] = &["limit"];
const TOP_PARAMETERS: &[&str] = &["basis", "limit"];

const MAX_LANGUAGE: usize = 35;

const DEFAULT_BASIS: DiscoveryBasis = DiscoveryBasis::Library;
const UNSUPPORTED_BASIS: &str = "server";

pub(crate) enum ArtistInfoApiError {
    Native(ApiError),
    MetadataDisabled(RequestContext),
    MetadataUnavailable(RequestContext),
    DiscoveryUnavailable(RequestContext),
    UnsupportedBasis(RequestContext),
    Internal(RequestContext),
}

impl IntoResponse for ArtistInfoApiError {
    fn into_response(self) -> Response {
        match self {
            Self::Native(error) => error.into_response(),
            Self::MetadataDisabled(context) => error_response(
                StatusCode::SERVICE_UNAVAILABLE,
                "metadata_disabled",
                "The artist metadata feature is disabled",
                context,
            ),
            Self::MetadataUnavailable(context) => error_response(
                StatusCode::SERVICE_UNAVAILABLE,
                "metadata_unavailable",
                "The artist metadata is temporarily unavailable",
                context,
            ),
            Self::DiscoveryUnavailable(context) => error_response(
                StatusCode::SERVICE_UNAVAILABLE,
                "discovery_unavailable",
                "The discovery feature is unavailable",
                context,
            ),
            Self::UnsupportedBasis(context) => error_response(
                StatusCode::NOT_IMPLEMENTED,
                "unsupported_basis",
                "The requested discovery basis is not offered",
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

fn invalid(context: &RequestContext, error: ValidationError) -> ArtistInfoApiError {
    ArtistInfoApiError::Native(ApiError::InvalidRequest {
        context: context.clone(),
        error,
    })
}

fn invalid_field(context: &RequestContext, field: &'static str) -> ArtistInfoApiError {
    invalid(context, ValidationError::Invalid { field })
}

fn not_found(context: &RequestContext) -> ArtistInfoApiError {
    ArtistInfoApiError::Native(ApiError::NotFound(context.clone()))
}

fn catalogue_failure(context: &RequestContext, error: CatalogueError) -> ArtistInfoApiError {
    match error {
        CatalogueError::Provider(ProviderError::Disabled) => {
            ArtistInfoApiError::MetadataDisabled(context.clone())
        }
        CatalogueError::Provider(ProviderError::ProviderUnavailable) => {
            ArtistInfoApiError::MetadataUnavailable(context.clone())
        }
        CatalogueError::Provider(ProviderError::MalformedResponse(_)) => {
            ArtistInfoApiError::Internal(context.clone())
        }
        error => ArtistInfoApiError::Native(ApiError::Catalogue {
            context: context.clone(),
            error,
        }),
    }
}

fn artist_id(raw: &str, context: &RequestContext) -> Result<ArtistId, ArtistInfoApiError> {
    raw.parse::<ArtistId>()
        .map_err(|_| invalid_field(context, "artist_id"))
}

fn discovery_error(context: &RequestContext, error: DiscoveryError) -> ArtistInfoApiError {
    match error {
        DiscoveryError::UnsupportedBasis { .. } => {
            ArtistInfoApiError::UnsupportedBasis(context.clone())
        }
        DiscoveryError::UnknownSeed => not_found(context),
        DiscoveryError::Unavailable => ArtistInfoApiError::MetadataUnavailable(context.clone()),
        DiscoveryError::Repository(error) => {
            catalogue_failure(context, CatalogueError::Repository(error))
        }
    }
}

fn discovery<'a>(
    state: &'a ArtistInfoState,
    context: &RequestContext,
) -> Result<&'a DiscoveryService, ArtistInfoApiError> {
    state
        .discovery
        .as_deref()
        .ok_or_else(|| ArtistInfoApiError::DiscoveryUnavailable(context.clone()))
}

fn profile(
    state: &ArtistInfoState,
    query: &RawQuery,
    context: &RequestContext,
) -> Result<MetadataProfile, ArtistInfoApiError> {
    match query.get("profile") {
        Some(raw) => MetadataProfile::parse(raw).ok_or_else(|| invalid_field(context, "profile")),
        None => Ok(state.profile.clone()),
    }
}

fn language(
    state: &ArtistInfoState,
    query: &RawQuery,
    context: &RequestContext,
) -> Result<String, ArtistInfoApiError> {
    match query.get("language") {
        Some(raw) if valid_language(raw) => Ok(raw.to_owned()),
        Some(_) => Err(invalid_field(context, "language")),
        None => Ok(state.language.clone()),
    }
}

fn valid_language(value: &str) -> bool {
    value.len() <= MAX_LANGUAGE
        && value
            .chars()
            .next()
            .is_some_and(|first| first.is_ascii_alphabetic())
        && value
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || character == '-')
}

fn requested_basis(
    query: &RawQuery,
    context: &RequestContext,
) -> Result<DiscoveryBasis, ArtistInfoApiError> {
    match query.get("basis") {
        None => Ok(DEFAULT_BASIS),
        Some(UNSUPPORTED_BASIS) => Err(ArtistInfoApiError::UnsupportedBasis(context.clone())),
        Some(value) => DiscoveryBasis::parse(value).ok_or_else(|| invalid_field(context, "basis")),
    }
}

async fn load_information(
    state: &ArtistInfoState,
    artist_id: &ArtistId,
    user: Option<&Uuid>,
    profile: &MetadataProfile,
    language: &str,
    refresh: RefreshDemand,
    context: &RequestContext,
) -> Result<ArtistInformation, ArtistInfoApiError> {
    match state
        .library
        .artist_information(artist_id, user, profile, language, refresh)
        .await
    {
        Ok(Some(information)) => Ok(information),
        Ok(None) => Err(not_found(context)),
        Err(error) => Err(catalogue_failure(context, error)),
    }
}

async fn require_artist(
    state: &ArtistInfoState,
    artist_id: &ArtistId,
    user: Option<&Uuid>,
    context: &RequestContext,
) -> Result<(), ArtistInfoApiError> {
    let known = state
        .library
        .artist_detail(artist_id, user)
        .await
        .map_err(|error| {
            ArtistInfoApiError::Native(ApiError::Catalogue {
                context: context.clone(),
                error,
            })
        })?
        .is_some();

    if known {
        Ok(())
    } else {
        Err(not_found(context))
    }
}

#[derive(Serialize)]
pub(crate) struct ArtistInfoBody {
    id: String,
    name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    sort_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    music_brainz_id: Option<String>,
    cover_art: String,
    images: Vec<ImageBody>,
    fields: Vec<FieldBody>,
    disagreements: Vec<DisagreementBody>,
    refresh: RefreshBody,
    contested: bool,
}

impl ArtistInfoBody {
    fn from_information(information: ArtistInformation) -> Self {
        let summary = &information.artist.artist;
        let id = summary.artist().id().to_string();
        let name = summary.artist().name().to_owned();
        let sort_name = summary.artist().sort_name().map(str::to_owned);
        let music_brainz_id = summary.artist().musicbrainz_artist_id().map(str::to_owned);

        Self {
            id,
            name,
            sort_name,
            music_brainz_id,
            cover_art: information.cover_art,
            images: information
                .portraits
                .iter()
                .map(ImageBody::from_descriptor)
                .collect(),
            fields: information
                .metadata
                .fields
                .iter()
                .map(FieldBody::from_entry)
                .collect(),
            disagreements: information
                .metadata
                .disagreements
                .iter()
                .map(DisagreementBody::from_disagreement)
                .collect(),
            refresh: RefreshBody::from_status(&information.refresh),
            contested: information.metadata.contested,
        }
    }
}

#[derive(Serialize)]
pub(crate) struct ImageBody {
    role: &'static str,
    size: Option<u32>,
    width: u32,
    height: u32,
    content_type: String,
    revision: i64,
    etag: String,
}

impl ImageBody {
    fn from_descriptor(descriptor: &ArtworkAssetDescriptor) -> Self {
        Self {
            role: descriptor.role.as_str(),
            size: descriptor.variant.size(),
            width: descriptor.width,
            height: descriptor.height,
            content_type: descriptor.content_type.clone(),
            revision: descriptor.revision,
            etag: format!("\"{}\"", descriptor.content_digest),
        }
    }
}

#[derive(Serialize)]
pub(crate) struct FieldBody {
    field: &'static str,
    presence: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    value: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    source: Option<SourceBody>,
    #[serde(skip_serializing_if = "Option::is_none")]
    retrieved_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    licence: Option<LicenceBody>,
    #[serde(skip_serializing_if = "Option::is_none")]
    attribution: Option<AttributionBody>,
    freshness: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    absence: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    bytes: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    unit: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    population: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    period: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    as_of: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    rank: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    score: Option<f64>,
}

impl FieldBody {
    fn from_entry(entry: &FieldEntry) -> Self {
        let winner = winner_fact(entry);
        let metric = winner.and_then(|fact| fact.metric.as_ref());

        Self {
            field: entry.field.as_str(),
            presence: presence_name(entry.presence),
            value: entry.value.as_ref().map(field_value),
            source: entry.source.as_ref().map(|source| SourceBody {
                provider_id: source.provider_id.clone(),
                external_id: source.external_id.clone(),
                match_basis: source.match_basis.as_str(),
                operation: winner.map(|fact| fact.operation.clone()),
            }),
            retrieved_at: entry.retrieved_at.map(timestamp),
            licence: entry.licence.as_ref().map(LicenceBody::from_terms),
            attribution: entry
                .attribution
                .as_ref()
                .map(AttributionBody::from_attribution),
            freshness: freshness_name(&entry.freshness),
            absence: entry.absence.as_ref().map(AbsenceReason::as_str),
            bytes: entry.bytes.as_ref().map(bytes_name),
            unit: metric.map(|metric| metric.unit.clone()),
            population: metric.and_then(|metric| metric.population.clone()),
            period: metric.and_then(|metric| metric.period.clone()),
            as_of: metric.and_then(|metric| metric.as_of).map(timestamp),
            rank: winner.and_then(|fact| fact.rank),
            score: winner.and_then(|fact| fact.score),
        }
    }
}

#[derive(Serialize)]
pub(crate) struct SourceBody {
    provider_id: String,
    external_id: String,
    match_basis: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    operation: Option<String>,
}

#[derive(Serialize)]
pub(crate) struct DisagreementBody {
    field: &'static str,
    sources: Vec<SourceBody>,
}

impl DisagreementBody {
    fn from_disagreement(disagreement: &Disagreement) -> Self {
        Self {
            field: disagreement.field.as_str(),
            sources: disagreement
                .sources
                .iter()
                .map(|source| SourceBody {
                    provider_id: source.provider_id.clone(),
                    external_id: source.external_id.clone(),
                    match_basis: source.match_basis.as_str(),
                    operation: None,
                })
                .collect(),
        }
    }
}

#[derive(Serialize)]
pub(crate) struct LicenceBody {
    id: String,
    name: String,
    url: Option<String>,
    attribution_required: bool,
    redistribution_allowed: bool,
    cache_allowed: bool,
    share_alike: bool,
    non_commercial: bool,
    storage_cap_bytes: Option<u64>,
}

impl LicenceBody {
    fn from_terms(terms: &LicenceTerms) -> Self {
        Self {
            id: terms.id.clone(),
            name: terms.name.clone(),
            url: terms.url.clone(),
            attribution_required: terms.attribution_required,
            redistribution_allowed: terms.redistribution_allowed,
            cache_allowed: terms.cache_allowed,
            share_alike: terms.share_alike,
            non_commercial: terms.non_commercial,
            storage_cap_bytes: terms.storage_cap_bytes,
        }
    }
}

#[derive(Serialize)]
pub(crate) struct AttributionBody {
    text: String,
    url: Option<String>,
}

impl AttributionBody {
    fn from_attribution(attribution: &Attribution) -> Self {
        Self {
            text: attribution.text.clone(),
            url: attribution.url.clone(),
        }
    }
}

#[derive(Serialize)]
pub(crate) struct RefreshBody {
    state: &'static str,
    revision: String,
}

impl RefreshBody {
    fn from_status(status: &RefreshStatus) -> Self {
        Self {
            state: phase_name(status.state.phase),
            revision: status.state.identity_revision.clone(),
        }
    }
}

#[derive(Serialize)]
pub(crate) struct RefreshOutcomeBody {
    outcome: &'static str,
}

#[derive(Serialize)]
pub(crate) struct RelatedBody {
    basis: &'static str,
    ranking_scope: Option<String>,
    fallback: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    fallback_reason: Option<&'static str>,
    items: Vec<RelatedItemBody>,
}

impl RelatedBody {
    fn from_result(result: DiscoveryResult<RelatedItem>) -> Self {
        Self {
            basis: result.basis.as_str(),
            ranking_scope: result.ranking_scope,
            fallback: result.fallback,
            fallback_reason: result.fallback_reason.map(FallbackReason::as_str),
            items: result
                .items
                .into_iter()
                .map(RelatedItemBody::from_item)
                .collect(),
        }
    }
}

#[derive(Serialize)]
pub(crate) struct RelatedItemBody {
    #[serde(skip_serializing_if = "Option::is_none")]
    id: Option<String>,
    name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    provider: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    external_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    retrieved_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    score: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    rank_in_basis: Option<u32>,
}

impl RelatedItemBody {
    fn from_item(item: RelatedItem) -> Self {
        Self {
            id: item.canonical.map(|id| id.to_string()),
            name: item.name,
            provider: item.provider,
            external_id: item.external_id,
            retrieved_at: item.retrieved_at.map(timestamp),
            score: item.score,
            rank_in_basis: Some(item.rank_in_basis),
        }
    }
}

#[derive(Serialize)]
pub(crate) struct TopBody {
    basis: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    window: Option<u64>,
    ranking_scope: Option<String>,
    fallback: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    fallback_reason: Option<&'static str>,
    items: Vec<TopItemBody>,
}

impl TopBody {
    fn from_result(result: DiscoveryResult<TopItem>) -> Self {
        Self {
            basis: result.basis.as_str(),
            window: result.window.map(|extent| extent.listened_ms()),
            ranking_scope: result.ranking_scope,
            fallback: result.fallback,
            fallback_reason: result.fallback_reason.map(FallbackReason::as_str),
            items: result
                .items
                .into_iter()
                .map(TopItemBody::from_item)
                .collect(),
        }
    }
}

#[derive(Serialize)]
pub(crate) struct TopItemBody {
    #[serde(skip_serializing_if = "Option::is_none")]
    track_id: Option<String>,
    title: String,
    artist: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    provider: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    external_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    rank_in_basis: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    score: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    listeners: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    length_ms: Option<u64>,
}

impl TopItemBody {
    fn from_item(item: TopItem) -> Self {
        Self {
            track_id: item.canonical.map(|id| id.to_string()),
            title: item.title,
            artist: item.artist,
            provider: item.provider,
            external_id: item.external_id,
            rank_in_basis: Some(item.rank_in_basis),
            score: item.score,
            listeners: None,
            length_ms: None,
        }
    }
}

pub(crate) async fn info(
    State(state): State<ArtistInfoState>,
    Extension(context): Extension<RequestContext>,
    AuthenticatedPrincipal(principal): AuthenticatedPrincipal,
    Path(raw_id): Path<String>,
    query: RawQuery,
) -> Result<Json<ArtistInfoBody>, ArtistInfoApiError> {
    reject_unknown(&query, INFO_PARAMETERS).map_err(|error| invalid(&context, error))?;
    let profile = profile(&state, &query, &context)?;
    let language = language(&state, &query, &context)?;
    let artist_id = artist_id(&raw_id, &context)?;
    let user = principal.user_id.as_uuid();
    let information = load_information(
        &state,
        &artist_id,
        Some(&user),
        &profile,
        &language,
        RefreshDemand::RequestIfDue,
        &context,
    )
    .await?;

    Ok(Json(ArtistInfoBody::from_information(information)))
}

pub(crate) async fn refresh(
    State(state): State<ArtistInfoState>,
    Extension(context): Extension<RequestContext>,
    AuthenticatedPrincipal(principal): AuthenticatedPrincipal,
    Path(raw_id): Path<String>,
    query: RawQuery,
) -> Result<Json<RefreshBody>, ArtistInfoApiError> {
    reject_unknown(&query, REFRESH_PARAMETERS).map_err(|error| invalid(&context, error))?;
    let profile = state.profile.clone();
    let language = state.language.clone();
    let artist_id = artist_id(&raw_id, &context)?;
    let user = principal.user_id.as_uuid();
    let information = load_information(
        &state,
        &artist_id,
        Some(&user),
        &profile,
        &language,
        RefreshDemand::ReadOnly,
        &context,
    )
    .await?;

    Ok(Json(RefreshBody::from_status(&information.refresh)))
}

pub(crate) async fn request_refresh(
    State(state): State<ArtistInfoState>,
    Extension(context): Extension<RequestContext>,
    AuthenticatedPrincipal(principal): AuthenticatedPrincipal,
    Path(raw_id): Path<String>,
    query: RawQuery,
) -> Result<Json<RefreshOutcomeBody>, ArtistInfoApiError> {
    reject_unknown(&query, REFRESH_PARAMETERS).map_err(|error| invalid(&context, error))?;
    let profile = state.profile.clone();
    let language = state.language.clone();
    let artist_id = artist_id(&raw_id, &context)?;
    let user = principal.user_id.as_uuid();
    let information = load_information(
        &state,
        &artist_id,
        Some(&user),
        &profile,
        &language,
        RefreshDemand::Force,
        &context,
    )
    .await?;
    let outcome = information
        .refresh
        .outcome
        .ok_or_else(|| ArtistInfoApiError::Internal(context.clone()))?;

    Ok(Json(RefreshOutcomeBody {
        outcome: outcome_name(outcome),
    }))
}

pub(crate) async fn related(
    State(state): State<ArtistInfoState>,
    Extension(context): Extension<RequestContext>,
    AuthenticatedPrincipal(principal): AuthenticatedPrincipal,
    Path(raw_id): Path<String>,
    query: RawQuery,
) -> Result<Json<RelatedBody>, ArtistInfoApiError> {
    reject_unknown(&query, RELATED_PARAMETERS).map_err(|error| invalid(&context, error))?;
    let limit = page_limit(&query).map_err(|error| invalid(&context, error))?;
    let artist_id = artist_id(&raw_id, &context)?;
    let user = principal.user_id.as_uuid();
    require_artist(&state, &artist_id, Some(&user), &context).await?;
    let discovery = discovery(&state, &context)?;
    let request = RelatedRequest {
        seed: DiscoverySeed::Artist(artist_id),
        basis: DEFAULT_BASIS,
        window: None,
        limit,
        user: Some(user),
    };
    let result = discovery
        .related(request)
        .await
        .map_err(|error| discovery_error(&context, error))?;

    Ok(Json(RelatedBody::from_result(result)))
}

pub(crate) async fn top(
    State(state): State<ArtistInfoState>,
    Extension(context): Extension<RequestContext>,
    AuthenticatedPrincipal(principal): AuthenticatedPrincipal,
    Path(raw_id): Path<String>,
    query: RawQuery,
) -> Result<Json<TopBody>, ArtistInfoApiError> {
    reject_unknown(&query, TOP_PARAMETERS).map_err(|error| invalid(&context, error))?;
    let limit = page_limit(&query).map_err(|error| invalid(&context, error))?;
    let basis = requested_basis(&query, &context)?;
    let artist_id = artist_id(&raw_id, &context)?;
    let user = principal.user_id.as_uuid();
    require_artist(&state, &artist_id, Some(&user), &context).await?;
    let discovery = discovery(&state, &context)?;
    let request = TopRequest {
        seed: DiscoverySeed::Artist(artist_id),
        basis,
        window: None,
        limit,
        user: Some(user),
    };
    let result = discovery
        .top(request)
        .await
        .map_err(|error| discovery_error(&context, error))?;

    Ok(Json(TopBody::from_result(result)))
}

pub(crate) fn routes() -> Router<ArtistInfoState> {
    Router::new()
        .route("/api/v1/library/artists/{id}/info", get(info))
        .route(
            "/api/v1/library/artists/{id}/refresh",
            get(refresh).post(request_refresh),
        )
        .route("/api/v1/library/artists/{id}/related", get(related))
        .route("/api/v1/library/artists/{id}/top", get(top))
}

fn winner_fact(entry: &FieldEntry) -> Option<&StoredFact> {
    let source = entry.source.as_ref()?;

    entry.facts.iter().find(|fact| &fact.source == source)
}

fn field_value(value: &FieldValue) -> Value {
    match value {
        FieldValue::Text(text) => Value::String(text.clone()),
        FieldValue::Count(count) => Value::from(*count),
        FieldValue::Reference(reference) => Value::String(reference.clone()),
        FieldValue::Candidates(candidates) => {
            Value::Array(candidates.iter().map(candidate_value).collect())
        }
    }
}

fn candidate_value(candidate: &CandidateItem) -> Value {
    json!({
        "name": candidate.name,
        "external_id": candidate.external_id,
        "url": candidate.url,
        "rank": candidate.rank,
        "score": candidate.score,
    })
}

fn presence_name(presence: Presence) -> &'static str {
    match presence {
        Presence::Present => "present",
        Presence::Absent => "absent",
    }
}

fn freshness_name(freshness: &Freshness) -> &'static str {
    match freshness {
        Freshness::Fresh => "fresh",
        Freshness::Stale => "stale",
        Freshness::Failed(_) => "failed",
        Freshness::Unrequested => "unrequested",
    }
}

fn bytes_name(bytes: &BytesState) -> &'static str {
    match bytes {
        BytesState::Servable => "servable",
        BytesState::ReferenceOnly => "reference_only",
        BytesState::Unavailable => "unavailable",
    }
}

fn phase_name(phase: RefreshPhase) -> &'static str {
    match phase {
        RefreshPhase::Idle => "idle",
        RefreshPhase::Pending => "pending",
        RefreshPhase::Running => "running",
        RefreshPhase::Deferred => "deferred",
        RefreshPhase::Unavailable => "unavailable",
    }
}

fn outcome_name(outcome: RefreshOutcome) -> &'static str {
    match outcome {
        RefreshOutcome::Inserted => "inserted",
        RefreshOutcome::Coalesced => "coalesced",
        RefreshOutcome::Deferred => "deferred",
        RefreshOutcome::Unavailable => "unavailable",
    }
}

fn timestamp(at: chrono::DateTime<chrono::Utc>) -> String {
    at.to_rfc3339()
}

#[cfg(test)]
#[path = "artist_info_tests.rs"]
mod tests;
