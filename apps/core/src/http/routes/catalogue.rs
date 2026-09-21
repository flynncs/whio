use axum::{
    Extension, Json,
    extract::{State, rejection::JsonRejection},
};
use serde::{Deserialize, Serialize};

use crate::{
    AppState,
    catalogue::{
        CatalogueSearch, SearchHit, SearchLimit, SearchOffset, SearchQuery, ValidationError,
    },
    http::auth::AuthenticatedPrincipal,
    http::errors::ApiError,
    request::RequestContext,
    tracks::Track,
};

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
}

impl From<Track> for TrackResponse {
    fn from(value: Track) -> Self {
        let (id, metadata) = value.into_parts();
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
            match_quality: None,
        }
    }
}

impl From<SearchHit> for TrackResponse {
    fn from(hit: SearchHit) -> Self {
        let match_quality = hit
            .match_quality()
            .map(|quality| quality.as_str().to_owned());
        let mut response = Self::from(hit.into_track());
        response.match_quality = match_quality;
        response
    }
}

pub(crate) async fn search(
    State(state): State<AppState>,
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
        .catalogue()
        .search(&search, Some(&user_uuid), &context)
        .await
        .map_err(|error| ApiError::Catalogue {
            context: context.clone(),
            error,
        })?;

    Ok(Json(SearchResponse {
        results: outcome.tracks.into_iter().map(Into::into).collect(),
        confidence: outcome.confidence.as_str().to_owned(),
    }))
}
