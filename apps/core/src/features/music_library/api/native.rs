use axum::{
    Extension, Json,
    extract::{State, rejection::JsonRejection},
    http::HeaderValue,
    response::{IntoResponse, Response},
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::hash::Hash;
use std::str::FromStr;
use std::sync::Arc;
use uuid::Uuid;

use crate::{
    features::identity::api::native::AuthenticatedPrincipal,
    features::music_library::api::errors::ApiError,
    features::music_library::catalogue::{
        AlbumListFilter, AlbumListScope, AlbumOrder, ArtistListFilter, ArtistSort, MAX_PAGE_LIMIT,
        PageLimit, SearchHit, SearchLimit, SearchOffset, SearchQuery, SearchWindow,
        TrackListFilter, TrackListScope, TrackOrder, ValidationError,
    },
    features::music_library::domain::{
        AlbumId, AlbumSummary, AlbumTrack, ArtistId, ArtistSummary, Availability, IdSet,
        RandomSeed, Range, Resource, RoleSelector, RoleSet, SelectorText, SortDirection, TrackView,
    },
    features::music_library::library::LibraryId,
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

impl SearchRequest {
    fn track_search(self) -> Result<(SearchQuery, SearchWindow), ValidationError> {
        let window = SearchWindow::new(
            SearchLimit::new(self.limit)?,
            SearchOffset::new(self.offset)?,
        );

        Ok((SearchQuery::new(self.query)?, window))
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
            ownership: ownership(availability).to_owned(),
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

    let (query, window) = request
        .track_search()
        .map_err(|error| ApiError::InvalidRequest {
            context: context.clone(),
            error,
        })?;

    let user_uuid = principal.user_id.as_uuid();

    let track_search = state
        .search
        .search_tracks(&query, window, Some(&user_uuid), &context)
        .await
        .map_err(|error| ApiError::Catalogue {
            context: context.clone(),
            error,
        })?;

    let library = &state.library;
    let results = track_search
        .tracks
        .into_iter()
        .map(|hit| {
            let availability = library.availability(hit.sources());

            TrackResponse::from_hit(hit, availability)
        })
        .collect();

    Ok(Json(SearchResponse {
        results,
        confidence: track_search.confidence.as_str().to_owned(),
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

const IDENTITY_PARAMETERS: &[&str] = &["apiKey", "c", "p", "s", "t", "u", "v"];

const TRACK_PARAMETERS: &[&str] = &[
    "album_artist_id",
    "album_id",
    "artists_id",
    "bitdepth_from",
    "bitdepth_to",
    "bitrate_from",
    "bitrate_to",
    "channels_from",
    "channels_to",
    "codec",
    "compilation",
    "date_added_from",
    "date_added_to",
    "direction",
    "disc_number_from",
    "disc_number_to",
    "disc_total_from",
    "disc_total_to",
    "duration_from",
    "duration_to",
    "filetype",
    "genre",
    "has_rating",
    "library_id",
    "limit",
    "missing",
    "offset",
    "order",
    "path",
    "play_count_from",
    "play_count_to",
    "rating_from",
    "rating_to",
    "recently_played",
    "role",
    "samplerate_from",
    "samplerate_to",
    "scope",
    "seed",
    "size_from",
    "size_to",
    "starred",
    "title",
    "track_number_from",
    "track_number_to",
    "track_total_from",
    "track_total_to",
    "year_from",
    "year_to",
];

const ALBUM_PARAMETERS: &[&str] = &[
    "album_artist_id",
    "artist_id",
    "artists_id",
    "compilation",
    "date_added_from",
    "date_added_to",
    "direction",
    "genre",
    "has_rating",
    "library_id",
    "limit",
    "missing",
    "name",
    "offset",
    "order",
    "rating_from",
    "rating_to",
    "recently_played",
    "role",
    "scope",
    "seed",
    "starred",
    "year_from",
    "year_to",
];

const ARTIST_PARAMETERS: &[&str] = &[
    "direction",
    "has_rating",
    "library_id",
    "limit",
    "missing",
    "name",
    "offset",
    "order",
    "rating_from",
    "rating_to",
    "role",
    "starred",
];

pub(crate) fn reject_unknown(query: &RawQuery, allowed: &[&str]) -> Result<(), ValidationError> {
    for name in query.names() {
        if !IDENTITY_PARAMETERS.contains(&name) && !allowed.contains(&name) {
            return Err(ValidationError::Invalid { field: "parameter" });
        }
    }

    Ok(())
}

pub(crate) fn rejected(
    error: crate::features::music_library::domain::ValidationError,
    field: &'static str,
) -> ValidationError {
    use crate::features::music_library::domain::ValidationError as DomainError;

    match error {
        DomainError::Empty { .. } => ValidationError::Empty { field },
        DomainError::TooLong { .. } => ValidationError::TooLong { field },
        DomainError::OutOfRange { min, max, .. } => ValidationError::OutOfRange {
            field,
            min: u32::try_from(min).unwrap_or(u32::MAX),
            max: u32::try_from(max).unwrap_or(u32::MAX),
        },
        _ => ValidationError::Invalid { field },
    }
}

fn value<T: FromStr>(raw: &str) -> Option<T> {
    raw.parse().ok()
}

fn genre(raw: &str) -> Option<SelectorText> {
    SelectorText::new(raw).ok().flatten()
}

fn timestamp(raw: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(raw)
        .ok()
        .map(|value| value.with_timezone(&Utc))
}

pub(crate) fn offset(query: &RawQuery) -> Result<u32, ValidationError> {
    match query.get("offset") {
        Some(raw) => raw
            .parse()
            .map_err(|_| ValidationError::Invalid { field: "offset" }),
        None => Ok(0),
    }
}

pub(crate) fn page_limit(query: &RawQuery) -> Result<PageLimit, ValidationError> {
    let raw = match query.get("limit") {
        Some(raw) => raw
            .parse()
            .map_err(|_| ValidationError::Invalid { field: "limit" })?,
        None => 50,
    };

    PageLimit::new(raw).map_err(|_| ValidationError::OutOfRange {
        field: "limit",
        min: 1,
        max: MAX_PAGE_LIMIT,
    })
}

fn order<O: Default>(query: &RawQuery, parse: fn(&str) -> Option<O>) -> Result<O, ValidationError> {
    match query.get("order") {
        Some(raw) => parse(raw).ok_or(ValidationError::Invalid { field: "order" }),
        None => Ok(O::default()),
    }
}

fn selector_text(
    query: &RawQuery,
    name: &'static str,
) -> Result<Option<SelectorText>, ValidationError> {
    match query.get(name) {
        Some(raw) => SelectorText::new(raw).map_err(|error| rejected(error, name)),
        None => Ok(None),
    }
}

fn flag(query: &RawQuery, name: &'static str) -> Result<Option<bool>, ValidationError> {
    match query.get(name) {
        Some("true") => Ok(Some(true)),
        Some("false") => Ok(Some(false)),
        Some(_) => Err(ValidationError::Invalid { field: name }),
        None => Ok(None),
    }
}

fn id_set<Id, F>(
    query: &RawQuery,
    name: &'static str,
    parse: F,
) -> Result<Option<IdSet<Id>>, ValidationError>
where
    Id: Clone + Eq + Hash,
    F: Fn(&str) -> Option<Id>,
{
    let Some(raw) = query.get(name) else {
        return Ok(None);
    };
    let mut values = Vec::new();

    for element in raw.split(',') {
        values.push(parse(element).ok_or(ValidationError::Invalid { field: name })?);
    }

    IdSet::new(values)
        .map(Some)
        .map_err(|error| rejected(error, name))
}

fn role_set(query: &RawQuery, name: &'static str) -> Result<Option<RoleSet>, ValidationError> {
    let Some(raw) = query.get(name) else {
        return Ok(None);
    };

    if raw.trim().is_empty() {
        return Ok(None);
    }

    let mut roles = Vec::new();

    for element in raw.split(',') {
        roles.push(RoleSelector::parse(element).ok_or(ValidationError::Invalid { field: name })?);
    }

    RoleSet::new(roles)
        .map(Some)
        .map_err(|error| rejected(error, name))
}

fn range<T, F>(
    query: &RawQuery,
    field: &'static str,
    parse: F,
) -> Result<Option<Range<T>>, ValidationError>
where
    T: Ord + Copy,
    F: Fn(&str) -> Option<T>,
{
    let from = bound(query, field, "from", &parse)?;
    let to = bound(query, field, "to", &parse)?;

    match (from, to) {
        (None, None) => Ok(None),
        (from, to) => Range::new(from, to)
            .map(Some)
            .map_err(|error| rejected(error, field)),
    }
}

fn bound<T, F>(
    query: &RawQuery,
    field: &'static str,
    edge: &str,
    parse: &F,
) -> Result<Option<T>, ValidationError>
where
    F: Fn(&str) -> Option<T>,
{
    match query.get(&format!("{field}_{edge}")) {
        Some(raw) => parse(raw)
            .map(Some)
            .ok_or(ValidationError::Invalid { field }),
        None => Ok(None),
    }
}

fn direction(query: &RawQuery) -> Result<Option<SortDirection>, ValidationError> {
    match query.get("direction") {
        Some(raw) => SortDirection::parse(raw)
            .map(Some)
            .ok_or(ValidationError::Invalid { field: "direction" }),
        None => Ok(None),
    }
}

fn random_seed(query: &RawQuery) -> Result<Option<RandomSeed>, ValidationError> {
    match query.get("seed") {
        Some(raw) => value::<u64>(raw)
            .map(|seed| Some(RandomSeed::new(seed)))
            .ok_or(ValidationError::Invalid { field: "seed" }),
        None => Ok(None),
    }
}

fn track_scope(query: &RawQuery) -> Result<TrackListScope, ValidationError> {
    match query.get("scope") {
        Some(raw) => TrackListScope::parse(raw).ok_or(ValidationError::Invalid { field: "scope" }),
        None => Ok(TrackListScope::default()),
    }
}

fn album_scope(query: &RawQuery) -> Result<AlbumListScope, ValidationError> {
    match query.get("scope") {
        Some(raw) => AlbumListScope::parse(raw).ok_or(ValidationError::Invalid { field: "scope" }),
        None => Ok(AlbumListScope::default()),
    }
}

pub(crate) struct LibraryTracksQuery {
    offset: u32,
    limit: PageLimit,
    order: TrackOrder,
    filter: TrackListFilter,
    seed: Option<RandomSeed>,
}

impl LibraryTracksQuery {
    fn parse(query: &RawQuery, user: Option<Uuid>) -> Result<Self, ValidationError> {
        reject_unknown(query, TRACK_PARAMETERS)?;

        let offset = offset(query)?;
        let limit = page_limit(query)?;
        let order = order(query, TrackOrder::parse)?;
        let requested_seed = random_seed(query)?;
        let filter = TrackListFilter::new()
            .with_scope(track_scope(query)?)
            .with_title(selector_text(query, "title")?)
            .with_album_id(id_set(query, "album_id", value::<AlbumId>)?)
            .with_album_artist_id(id_set(query, "album_artist_id", value::<ArtistId>)?)
            .with_artists_id(id_set(query, "artists_id", value::<ArtistId>)?)
            .with_role(role_set(query, "role")?)
            .with_genre(id_set(query, "genre", genre)?)
            .with_year(range(query, "year", value::<i32>)?)
            .with_compilation(flag(query, "compilation")?)
            .with_starred(flag(query, "starred")?)
            .with_has_rating(flag(query, "has_rating")?)
            .with_rating(range(query, "rating", value::<u8>)?)
            .with_play_count(range(query, "play_count", value::<u32>)?)
            .with_recently_played(flag(query, "recently_played")?)
            .with_date_added(range(query, "date_added", timestamp)?)
            .with_missing(flag(query, "missing")?)
            .with_path(selector_text(query, "path")?)
            .with_duration(range(query, "duration", value::<u64>)?)
            .with_track_number(range(query, "track_number", value::<u16>)?)
            .with_disc_number(range(query, "disc_number", value::<u16>)?)
            .with_disc_total(range(query, "disc_total", value::<u16>)?)
            .with_track_total(range(query, "track_total", value::<u16>)?)
            .with_bitrate(range(query, "bitrate", value::<i64>)?)
            .with_bitdepth(range(query, "bitdepth", value::<i64>)?)
            .with_samplerate(range(query, "samplerate", value::<i64>)?)
            .with_size(range(query, "size", value::<i64>)?)
            .with_channels(range(query, "channels", value::<u16>)?)
            .with_codec(selector_text(query, "codec")?)
            .with_filetype(selector_text(query, "filetype")?)
            .with_library_id(id_set(query, "library_id", value::<LibraryId>)?)
            .with_sort_direction(direction(query)?)
            .with_random_seed(requested_seed);

        filter
            .resolved_seed(order)
            .map_err(|error| rejected(error, "seed"))?;

        let seed = match (order, requested_seed) {
            (TrackOrder::Random, Some(seed)) => Some(seed),
            (TrackOrder::Random, None) => Some(RandomSeed::for_request(
                user,
                Resource::Track,
                Utc::now().date_naive(),
            )),
            _ => None,
        };
        let filter = filter.with_random_seed(seed);

        Ok(Self {
            offset,
            limit,
            order,
            filter,
            seed,
        })
    }
}

pub(crate) struct LibraryAlbumsQuery {
    offset: u32,
    limit: PageLimit,
    order: AlbumOrder,
    filter: AlbumListFilter,
    seed: Option<RandomSeed>,
}

impl LibraryAlbumsQuery {
    fn parse(query: &RawQuery, user: Option<Uuid>) -> Result<Self, ValidationError> {
        reject_unknown(query, ALBUM_PARAMETERS)?;

        let offset = offset(query)?;
        let limit = page_limit(query)?;
        let order = order(query, AlbumOrder::parse)?;
        let requested_seed = random_seed(query)?;
        let filter = AlbumListFilter::new()
            .with_scope(album_scope(query)?)
            .with_name(selector_text(query, "name")?)
            .with_artist_id(id_set(query, "artist_id", value::<ArtistId>)?)
            .with_album_artist_id(id_set(query, "album_artist_id", value::<ArtistId>)?)
            .with_artists_id(id_set(query, "artists_id", value::<ArtistId>)?)
            .with_role(role_set(query, "role")?)
            .with_genre(id_set(query, "genre", genre)?)
            .with_year(range(query, "year", value::<i32>)?)
            .with_compilation(flag(query, "compilation")?)
            .with_starred(flag(query, "starred")?)
            .with_has_rating(flag(query, "has_rating")?)
            .with_rating(range(query, "rating", value::<u8>)?)
            .with_recently_played(flag(query, "recently_played")?)
            .with_date_added(range(query, "date_added", timestamp)?)
            .with_missing(flag(query, "missing")?)
            .with_library_id(id_set(query, "library_id", value::<LibraryId>)?)
            .with_sort_direction(direction(query)?)
            .with_random_seed(requested_seed);

        filter
            .resolved_seed(order)
            .map_err(|error| rejected(error, "seed"))?;

        let seed = match (order, requested_seed) {
            (AlbumOrder::Random, Some(seed)) => Some(seed),
            (AlbumOrder::Random, None) => Some(RandomSeed::for_request(
                user,
                Resource::Album,
                Utc::now().date_naive(),
            )),
            _ => None,
        };
        let filter = filter.with_random_seed(seed);

        Ok(Self {
            offset,
            limit,
            order,
            filter,
            seed,
        })
    }
}

pub(crate) struct LibraryArtistsQuery {
    offset: u32,
    limit: PageLimit,
    order: ArtistSort,
    filter: ArtistListFilter,
}

impl LibraryArtistsQuery {
    fn parse(query: &RawQuery) -> Result<Self, ValidationError> {
        reject_unknown(query, ARTIST_PARAMETERS)?;

        let offset = offset(query)?;
        let limit = page_limit(query)?;
        let order = order(query, ArtistSort::parse)?;
        let filter = ArtistListFilter::new()
            .with_name(selector_text(query, "name")?)
            .with_role(role_set(query, "role")?)
            .with_starred(flag(query, "starred")?)
            .with_has_rating(flag(query, "has_rating")?)
            .with_rating(range(query, "rating", value::<u8>)?)
            .with_library_id(id_set(query, "library_id", value::<LibraryId>)?)
            .with_missing(flag(query, "missing")?)
            .with_sort_direction(direction(query)?);

        Ok(Self {
            offset,
            limit,
            order,
            filter,
        })
    }
}

#[derive(Serialize)]
struct PageResponse<T> {
    items: Vec<T>,
    total: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    seed: Option<u64>,
}

fn ownership(availability: Availability) -> &'static str {
    if availability.is_owned() {
        "owned"
    } else {
        "provider"
    }
}

pub(crate) fn page_response<T: Serialize>(
    items: Vec<T>,
    total: u32,
    seed: Option<u64>,
) -> Response {
    let mut response = Json(PageResponse { items, total, seed }).into_response();
    response.headers_mut().insert(
        "x-total-count",
        HeaderValue::from_str(&total.to_string()).expect("a count is a valid header value"),
    );

    response
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
    pub(crate) fn from_view(view: &TrackView, availability: Availability) -> Self {
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
            ownership: ownership(availability).to_owned(),
            availability: availability.as_str().to_owned(),
        }
    }
}

#[derive(Serialize)]
pub(crate) struct AlbumPageItem {
    id: String,
    name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    album_artist: Option<AlbumPageArtist>,
    #[serde(skip_serializing_if = "Option::is_none")]
    year: Option<i32>,
    song_count: u32,
    duration_ms: u64,
    starred: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    rating: Option<u8>,
    play_count: u32,
}

#[derive(Serialize)]
pub(crate) struct AlbumPageArtist {
    id: String,
    name: String,
}

impl AlbumPageItem {
    fn from_summary(summary: AlbumSummary) -> Self {
        let state = summary.state();

        Self {
            id: summary.album().id().to_string(),
            name: summary.album().title().to_owned(),
            album_artist: summary.album_artist().map(|artist| AlbumPageArtist {
                id: artist.id().to_string(),
                name: artist.name().to_owned(),
            }),
            year: summary.album().year(),
            song_count: summary.track_count(),
            duration_ms: summary.duration_ms(),
            starred: state.is_starred(),
            rating: state.rating(),
            play_count: state.play_count(),
        }
    }
}

#[derive(Serialize)]
pub(crate) struct ArtistPageItem {
    id: String,
    name: String,
    album_count: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    song_count: Option<u32>,
    starred: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    rating: Option<u8>,
    play_count: u32,
}

impl ArtistPageItem {
    fn from_summary(summary: ArtistSummary) -> Self {
        let state = summary.state();

        Self {
            id: summary.artist().id().to_string(),
            name: summary.artist().name().to_owned(),
            album_count: summary.album_count(),
            song_count: summary.song_count(),
            starred: state.is_starred(),
            rating: state.rating(),
            play_count: state.play_count(),
        }
    }
}

pub(crate) async fn library_tracks(
    State(state): State<NativeState>,
    Extension(context): Extension<RequestContext>,
    AuthenticatedPrincipal(principal): AuthenticatedPrincipal,
    query: RawQuery,
) -> Result<Response, ApiError> {
    let user_uuid = principal.user_id.as_uuid();
    let request = LibraryTracksQuery::parse(&query, Some(user_uuid)).map_err(|error| {
        ApiError::InvalidRequest {
            context: context.clone(),
            error,
        }
    })?;

    let page = state
        .library
        .track_page(
            request.offset,
            request.limit,
            request.order,
            &request.filter,
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

            TrackPageItem::from_view(&view, availability)
        })
        .collect();

    Ok(page_response(
        items,
        total,
        request.seed.map(RandomSeed::get),
    ))
}

pub(crate) async fn library_albums(
    State(state): State<NativeState>,
    Extension(context): Extension<RequestContext>,
    AuthenticatedPrincipal(principal): AuthenticatedPrincipal,
    query: RawQuery,
) -> Result<Response, ApiError> {
    let user_uuid = principal.user_id.as_uuid();
    let request = LibraryAlbumsQuery::parse(&query, Some(user_uuid)).map_err(|error| {
        ApiError::InvalidRequest {
            context: context.clone(),
            error,
        }
    })?;

    let summaries = state
        .library
        .album_list(
            request.offset,
            request.limit.get(),
            request.order,
            request.filter.clone(),
            Some(&user_uuid),
        )
        .await
        .map_err(|error| ApiError::Catalogue {
            context: context.clone(),
            error,
        })?;
    let total = state
        .library
        .album_count(Some(&user_uuid), request.filter)
        .await
        .map_err(|error| ApiError::Catalogue {
            context: context.clone(),
            error,
        })?;
    let items = summaries
        .into_iter()
        .map(AlbumPageItem::from_summary)
        .collect();

    Ok(page_response(
        items,
        total,
        request.seed.map(RandomSeed::get),
    ))
}

pub(crate) async fn library_artists(
    State(state): State<NativeState>,
    Extension(context): Extension<RequestContext>,
    AuthenticatedPrincipal(principal): AuthenticatedPrincipal,
    query: RawQuery,
) -> Result<Response, ApiError> {
    let request = LibraryArtistsQuery::parse(&query).map_err(|error| ApiError::InvalidRequest {
        context: context.clone(),
        error,
    })?;
    let user_uuid = principal.user_id.as_uuid();

    let summaries = state
        .library
        .artist_list(
            request.offset,
            request.limit.get(),
            request.order,
            &request.filter,
            Some(&user_uuid),
        )
        .await
        .map_err(|error| ApiError::Catalogue {
            context: context.clone(),
            error,
        })?;
    let total = state
        .library
        .artist_count(Some(&user_uuid), &request.filter)
        .await
        .map_err(|error| ApiError::Catalogue {
            context: context.clone(),
            error,
        })?;
    let items = summaries
        .into_iter()
        .map(ArtistPageItem::from_summary)
        .collect();

    Ok(page_response(items, total, None))
}
