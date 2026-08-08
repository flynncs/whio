use axum::{
    Extension, Json,
    extract::{State, rejection::JsonRejection},
};
use serde::{Deserialize, Serialize};

use crate::{
    AppState,
    catalogue::{CatalogueSearch, SearchLimit, SearchQuery, Track, ValidationError},
    http::errors::ApiError,
    request::RequestContext,
};

#[derive(Deserialize)]
pub(crate) struct SearchRequest {
    query: String,
    limit: u32,
}

impl TryFrom<SearchRequest> for CatalogueSearch {
    type Error = ValidationError;

    fn try_from(value: SearchRequest) -> Result<Self, Self::Error> {
        Ok(Self::new(
            SearchQuery::new(value.query)?,
            SearchLimit::new(value.limit)?,
        ))
    }
}

#[derive(Serialize)]
pub(crate) struct SearchResponse {
    results: Vec<TrackResponse>,
}

#[derive(Serialize)]
pub(crate) struct TrackResponse {
    id: String,
    title: String,
    artists: Vec<String>,
    duration_ms: Option<u64>,
}

impl From<Track> for TrackResponse {
    fn from(value: Track) -> Self {
        let (id, metadata) = value.into_parts();
        let (title, artists, duration_ms) = metadata.into_parts();

        Self {
            id: id.to_string(),
            title,
            artists,
            duration_ms,
        }
    }
}

pub(crate) async fn search(
    State(state): State<AppState>,
    Extension(context): Extension<RequestContext>,
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

    let tracks = state
        .catalogue()
        .search(&search, &context)
        .await
        .map_err(|error| ApiError::Catalogue {
            context: context.clone(),
            error,
        })?;

    Ok(Json(SearchResponse {
        results: tracks.into_iter().map(Into::into).collect(),
    }))
}
