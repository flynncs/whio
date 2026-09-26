use axum::{
    Extension, Json,
    extract::{State, rejection::JsonRejection},
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use crate::{
    features::identity::api::native::AuthenticatedPrincipal,
    features::music_library::api::errors::ApiError,
    features::music_library::catalogue::{
        CatalogueSearch, SearchHit, SearchLimit, SearchOffset, SearchQuery, ValidationError,
    },
    features::music_library::domain::Availability,
    platform::request::RequestContext,
};

#[derive(Clone)]
pub(crate) struct NativeState {
    pub(crate) search: Arc<crate::features::music_library::catalogue::SearchService>,
    pub(crate) library: Arc<crate::features::music_library::catalogue::LibraryService>,
    pub(crate) artwork: Arc<crate::features::music_library::artwork::ArtworkService>,
}

#[derive(Deserialize)]
pub(crate) struct SearchRequest {
    query: String,
    limit: u32,
    #[serde(default)]
    offset: u32,
}

impl TryFrom<SearchRequest> for CatalogueSearch {
    type Error = ValidationError;

    fn try_from(value: SearchRequest) -> Result<Self, Self::Error> {
        Ok(Self::new_with_offset(
            SearchQuery::new(value.query)?,
            SearchLimit::new(value.limit)?,
            SearchOffset::new(value.offset)?,
        ))
    }
}

#[derive(Serialize)]
pub(crate) struct SearchResponse {
    results: Vec<TrackResponse>,
    confidence: String,
}

#[derive(Serialize)]
pub(crate) struct TrackResponse {
    id: String,
    title: String,
    artists: Vec<String>,
    duration_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    match_quality: Option<String>,
    ownership: String,
    availability: String,
}

impl TrackResponse {
    fn from_hit(hit: SearchHit, availability: Availability) -> Self {
        let match_quality = hit
            .match_quality()
            .map(|quality| quality.as_str().to_owned());
        let (id, metadata) = hit.into_track().into_parts();
        let (title, artists, duration_ms) = metadata.into_parts();
        let artists = artists
            .iter()
            .map(|artist| artist.name().to_owned())
            .collect();

        Self {
            id: id.to_string(),
            title,
            artists,
            duration_ms,
            match_quality,
            ownership: if availability.is_owned() {
                "owned"
            } else {
                "provider"
            }
            .to_owned(),
            availability: availability.as_str().to_owned(),
        }
    }
}

pub(crate) async fn search(
    State(state): State<NativeState>,
    Extension(context): Extension<RequestContext>,
    AuthenticatedPrincipal(principal): AuthenticatedPrincipal,
    payload: Result<Json<SearchRequest>, JsonRejection>,
) -> Result<Json<SearchResponse>, ApiError> {
    let Json(request) = payload.map_err(|error| ApiError::MalformedRequest {
        context: context.clone(),
        error,
    })?;

    let search = CatalogueSearch::try_from(request).map_err(|error| ApiError::InvalidRequest {
        context: context.clone(),
        error,
    })?;

    let user_uuid = principal.user_id.as_uuid();

    let outcome = state
        .search
        .search(&search, Some(&user_uuid), &context)
        .await
        .map_err(|error| ApiError::Catalogue {
            context: context.clone(),
            error,
        })?;

    let library = &state.library;
    let results = outcome
        .tracks
        .into_iter()
        .map(|hit| {
            let availability = library.availability(hit.sources());

            TrackResponse::from_hit(hit, availability)
        })
        .collect();

    Ok(Json(SearchResponse {
        results,
        confidence: outcome.confidence.as_str().to_owned(),
    }))
}

pub(crate) async fn artwork(
    State(state): State<NativeState>,
    Extension(context): Extension<RequestContext>,
    _principal: AuthenticatedPrincipal,
    axum::extract::Path(cover_id): axum::extract::Path<String>,
    headers: axum::http::HeaderMap,
    query: crate::platform::raw_query::RawQuery,
) -> Result<axum::response::Response, ApiError> {
    use crate::features::music_library::artwork::{
        ArtworkOutcome, CoverArtTarget,
        response::{found_response, not_modified_response, requested_size},
    };
    use axum::http::header::IF_NONE_MATCH;

    let target = CoverArtTarget::parse(&cover_id).map_err(|error| ApiError::InvalidTrackId {
        context: context.clone(),
        error,
    })?;
    let if_none_match = headers
        .get(IF_NONE_MATCH)
        .and_then(|value| value.to_str().ok());
    match state
        .artwork
        .load(target, if_none_match, requested_size(query.get("size")))
        .await
    {
        Ok(ArtworkOutcome::Found(artwork)) => Ok(found_response(artwork)),
        Ok(ArtworkOutcome::NotModified(etag)) => Ok(not_modified_response(etag)),
        Ok(ArtworkOutcome::Missing) => Err(ApiError::NotFound(context)),
        Err(error) => Err(ApiError::Artwork { context, error }),
    }
}
