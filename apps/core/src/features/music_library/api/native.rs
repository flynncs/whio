use axum::{
    Extension, Json,
    extract::{State, rejection::JsonRejection},
    http::HeaderValue,
    response::IntoResponse,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use crate::{
    features::identity::api::native::AuthenticatedPrincipal,
    features::music_library::api::errors::ApiError,
    features::music_library::catalogue::{
        CatalogueSearch, MAX_PAGE_LIMIT, PageLimit, SearchHit, SearchLimit, SearchOffset,
        SearchQuery, TrackListFilter, TrackListScope, TrackOrder, ValidationError,
    },
    features::music_library::domain::{AlbumTrack, Availability, TrackView},
    platform::raw_query::RawQuery,
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

pub(crate) struct LibraryTracksQuery {
    offset: u32,
    limit: PageLimit,
    order: TrackOrder,
    scope: TrackListScope,
}

impl LibraryTracksQuery {
    const DEFAULT_OFFSET: u32 = 0;
    const DEFAULT_LIMIT: u32 = 50;
    const MIN_LIMIT: u32 = 1;

    fn parse(query: &RawQuery) -> Result<Self, ValidationError> {
        let offset = match query.get("offset") {
            Some(raw) => raw
                .parse()
                .map_err(|_| ValidationError::Invalid { field: "offset" })?,
            None => Self::DEFAULT_OFFSET,
        };
        let raw_limit = match query.get("limit") {
            Some(raw) => raw
                .parse()
                .map_err(|_| ValidationError::Invalid { field: "limit" })?,
            None => Self::DEFAULT_LIMIT,
        };
        let limit = PageLimit::new(raw_limit).map_err(|_| ValidationError::OutOfRange {
            field: "limit",
            min: Self::MIN_LIMIT,
            max: MAX_PAGE_LIMIT,
        })?;
        let order = match query.get("order") {
            Some(raw) => {
                TrackOrder::parse(raw).ok_or(ValidationError::Invalid { field: "order" })?
            }
            None => TrackOrder::default(),
        };
        let scope = match query.get("scope") {
            Some(raw) => {
                TrackListScope::parse(raw).ok_or(ValidationError::Invalid { field: "scope" })?
            }
            None => TrackListScope::default(),
        };

        Ok(Self {
            offset,
            limit,
            order,
            scope,
        })
    }
}

#[derive(Serialize)]
pub(crate) struct TrackPageResponse {
    items: Vec<TrackPageItem>,
    total: u32,
}

#[derive(Serialize)]
pub(crate) struct TrackPageItem {
    id: String,
    title: String,
    artists: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    duration_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    album: Option<TrackPageAlbum>,
    #[serde(skip_serializing_if = "Option::is_none")]
    disc_number: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    track_number: Option<u16>,
    starred: bool,
    play_count: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    rating: Option<u8>,
    ownership: String,
    availability: String,
}

#[derive(Serialize)]
pub(crate) struct TrackPageAlbum {
    id: String,
    title: String,
}

impl TrackPageItem {
    fn from_view(view: TrackView, availability: Availability) -> Self {
        let state = view.state();

        Self {
            id: view.track().id().to_string(),
            title: view.track().title().to_owned(),
            artists: view
                .track()
                .artists()
                .iter()
                .map(|artist| artist.name().to_owned())
                .collect(),
            duration_ms: view.track().duration_ms(),
            album: view.album().map(|album| TrackPageAlbum {
                id: album.id().to_string(),
                title: album.title().to_owned(),
            }),
            disc_number: view.membership().and_then(AlbumTrack::disc_number),
            track_number: view.membership().and_then(AlbumTrack::track_number),
            starred: state.is_starred(),
            play_count: state.play_count(),
            rating: state.rating(),
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

pub(crate) async fn library_tracks(
    State(state): State<NativeState>,
    Extension(context): Extension<RequestContext>,
    AuthenticatedPrincipal(principal): AuthenticatedPrincipal,
    query: RawQuery,
) -> Result<axum::response::Response, ApiError> {
    let request = LibraryTracksQuery::parse(&query).map_err(|error| ApiError::InvalidRequest {
        context: context.clone(),
        error,
    })?;
    let filter = TrackListFilter::new().with_scope(request.scope);
    let user_uuid = principal.user_id.as_uuid();

    let page = state
        .library
        .track_page(
            request.offset,
            request.limit,
            request.order,
            &filter,
            Some(&user_uuid),
        )
        .await
        .map_err(|error| ApiError::Catalogue {
            context: context.clone(),
            error,
        })?;

    let (views, total) = page.into_parts();
    let items = views
        .into_iter()
        .map(|view| {
            let availability = state.library.availability(view.sources());

            TrackPageItem::from_view(view, availability)
        })
        .collect();

    let mut response = Json(TrackPageResponse { items, total }).into_response();
    response.headers_mut().insert(
        "x-total-count",
        HeaderValue::from_str(&total.to_string()).expect("a count is a valid header value"),
    );

    Ok(response)
}
