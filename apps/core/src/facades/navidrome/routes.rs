use axum::{
    Extension, Json,
    extract::{Path, State, rejection::JsonRejection},
    http::{HeaderValue, StatusCode},
    response::{IntoResponse, Response},
};

use uuid::Uuid;

use crate::{
    facades::navidrome::state::{NavidromeQueueState, NavidromeState},
    features::music_library::domain::{
        AlbumId, AlbumListFilter, ArtistId, ArtistListFilter, CommentPatch, IdSet, MAX_PAGE_LIMIT,
        OccurrenceSelector, PageLimit, Playlist, PlaylistEntryChange, PlaylistEntryError,
        PlaylistMetadataPatch, PlaylistOrder, Range, RoleSelector, RoleSet, SelectorText,
        SortDirection, TrackId, TrackListFilter, TrackOrder,
    },
    features::music_library::library::{LibraryId, LibraryRegistry},
    features::music_library::playlists::PlaylistError,
    features::music_library::queue::RestoreQueueError,
    platform::raw_query::RawQuery,
    platform::request::RequestContext,
};

use super::{
    auth::NdToken,
    dto::{
        PlaylistAdded, PlaylistCreateBody, PlaylistCreated, PlaylistMoveBody, PlaylistMoved,
        PlaylistRemoved, PlaylistTracksBody, PlaylistUpdateBody, QueueSaveBody,
    },
    errors,
    service::{self, SongError},
};

pub(crate) async fn song_list(
    State(state): State<NavidromeState>,
    Extension(context): Extension<RequestContext>,
    token: NdToken,
    query: RawQuery,
) -> Response {
    let request = match song_list_query(&state, &query).await {
        Ok(request) => request,
        Err(error) => return song_error(error, &context),
    };

    match service::song_list(
        state.library(),
        state.completion(),
        state.credential(),
        &token.0,
        request,
        state.source_tags(),
    )
    .await
    {
        Ok((songs, total)) => list_response(&songs, total),
        Err(error) => song_error(error, &context),
    }
}

pub(crate) async fn song_detail(
    State(state): State<NavidromeState>,
    Extension(context): Extension<RequestContext>,
    token: NdToken,
    Path(id): Path<String>,
) -> Response {
    match service::song_detail(
        state.library(),
        state.credential(),
        &id,
        &token.0,
        state.source_tags(),
    )
    .await
    {
        Ok(song) => Json(song).into_response(),
        Err(error) => song_error(error, &context),
    }
}

const ALBUM_LIST_PARAMETERS: &[&str] = &[
    "_start",
    "_end",
    "_sort",
    "_order",
    "artist_id",
    "genre_id",
    "compilation",
    "has_rating",
    "recently_played",
    "starred",
    "name",
    "year",
    "library_id",
    "missing",
];

const ARTIST_LIST_PARAMETERS: &[&str] = &[
    "_start",
    "_end",
    "_sort",
    "_order",
    "library_id",
    "name",
    "starred",
    "role",
    "missing",
];

const SONG_LIST_PARAMETERS: &[&str] = &[
    "_start",
    "_end",
    "_sort",
    "_order",
    "title",
    "album_id",
    "album_artist_id",
    "artists_id",
    "genre_id",
    "library_id",
    "year",
    "starred",
    "has_rating",
    "missing",
    "compilation",
    "recently_played",
    "role",
];

async fn song_list_query(
    state: &NavidromeState,
    query: &RawQuery,
) -> Result<service::SongListRequest, SongError> {
    reject_unknown(query, SONG_LIST_PARAMETERS)?;
    for name in [
        "_start",
        "_end",
        "_sort",
        "_order",
        "title",
        "year",
        "starred",
        "has_rating",
        "missing",
        "compilation",
        "recently_played",
    ] {
        scalar(query, name)?;
    }

    let direction = service::sort_direction(query.get("_order"))?;
    let order = service::song_order(query.get("_sort"))?;
    let album_id = id_set(query, "album_id", |raw| raw.parse::<AlbumId>().ok())?;
    let album_artist_id = id_set(query, "album_artist_id", |raw| raw.parse::<ArtistId>().ok())?;
    let artists_id = id_set(query, "artists_id", |raw| raw.parse::<ArtistId>().ok())?;
    let genre = id_set(query, "genre_id", |raw| {
        SelectorText::new(raw).ok().flatten()
    })?;
    let library_id = library_id(state.libraries(), query).await?;
    let filter = TrackListFilter::new()
        .with_title(selector_text(query, "title")?)
        .with_album_id(album_id)
        .with_album_artist_id(album_artist_id)
        .with_artists_id(artists_id)
        .with_genre(genre)
        .with_library_id(library_id)
        .with_year(single_year(query)?)
        .with_starred(flag(query, "starred")?)
        .with_has_rating(flag(query, "has_rating")?)
        .with_missing(flag(query, "missing")?)
        .with_compilation(flag(query, "compilation")?)
        .with_recently_played(flag(query, "recently_played")?)
        .with_role(role_set(query, "role")?)
        .with_sort_direction(direction);
    let album_context = filter.album_id().is_some_and(|ids| ids.len() == 1);
    let order = order.unwrap_or(if album_context {
        TrackOrder::Album
    } else {
        TrackOrder::Title
    });
    let (offset, limit) = song_window(query)?;

    Ok(service::SongListRequest {
        filter,
        order,
        offset,
        limit,
    })
}

fn scalar<'a>(query: &'a RawQuery, name: &str) -> Result<Option<&'a str>, SongError> {
    let values = query.all(name);
    if values.len() > 1 {
        return Err(SongError::InvalidSearch);
    }

    Ok(values.first().copied())
}

fn reject_unknown(query: &RawQuery, allowed: &[&str]) -> Result<(), SongError> {
    for name in query.names() {
        if !allowed.contains(&name) {
            return Err(SongError::InvalidSearch);
        }
    }

    Ok(())
}

fn elements<'a>(query: &'a RawQuery, name: &str) -> Vec<&'a str> {
    query
        .all(name)
        .into_iter()
        .flat_map(|raw| raw.split(','))
        .collect()
}

fn id_set<Id: Clone + Eq + std::hash::Hash>(
    query: &RawQuery,
    name: &str,
    parse: impl Fn(&str) -> Option<Id>,
) -> Result<Option<IdSet<Id>>, SongError> {
    if query.get(name).is_none() {
        return Ok(None);
    }
    let mut ids = Vec::new();

    for element in elements(query, name) {
        ids.push(parse(element).ok_or(SongError::InvalidSearch)?);
    }

    IdSet::new(ids)
        .map(Some)
        .map_err(|_| SongError::InvalidSearch)
}

fn number_set(query: &RawQuery, name: &str) -> Result<Option<Vec<i32>>, SongError> {
    if query.get(name).is_none() {
        return Ok(None);
    }
    let mut numbers = Vec::new();

    for element in elements(query, name) {
        numbers.push(
            element
                .trim()
                .parse::<i32>()
                .map_err(|_| SongError::InvalidSearch)?,
        );
    }

    Ok(Some(numbers))
}

fn selector_text(query: &RawQuery, name: &str) -> Result<Option<SelectorText>, SongError> {
    match query.get(name) {
        Some(raw) => SelectorText::new(raw).map_err(|_| SongError::InvalidSearch),
        None => Ok(None),
    }
}

fn flag(query: &RawQuery, name: &str) -> Result<Option<bool>, SongError> {
    match query.get(name) {
        Some("true") => Ok(Some(true)),
        Some("false") => Ok(Some(false)),
        Some(_) => Err(SongError::InvalidSearch),
        None => Ok(None),
    }
}

fn role_set(query: &RawQuery, name: &str) -> Result<Option<RoleSet>, SongError> {
    let values = query.all(name);
    if values.is_empty() || values.iter().all(|raw| raw.trim().is_empty()) {
        return Ok(None);
    }

    let mut roles = Vec::new();
    for element in elements(query, name) {
        roles.push(RoleSelector::parse(element).ok_or(SongError::InvalidSearch)?);
    }

    RoleSet::new(roles)
        .map(Some)
        .map_err(|_| SongError::InvalidSearch)
}

fn single_year(query: &RawQuery) -> Result<Option<Range<i32>>, SongError> {
    match query.get("year") {
        Some(raw) => {
            let year = raw
                .trim()
                .parse::<i32>()
                .map_err(|_| SongError::InvalidSearch)?;
            Range::new(Some(year), Some(year))
                .map(Some)
                .map_err(|_| SongError::InvalidSearch)
        }
        None => Ok(None),
    }
}

async fn library_id(
    libraries: &LibraryRegistry,
    query: &RawQuery,
) -> Result<Option<IdSet<LibraryId>>, SongError> {
    match number_set(query, "library_id")? {
        Some(requested) => service::canonical_libraries(libraries, &requested)
            .await
            .map(Some),
        None => Ok(None),
    }
}

pub(crate) async fn album_list(
    State(state): State<NavidromeState>,
    Extension(context): Extension<RequestContext>,
    token: NdToken,
    query: RawQuery,
) -> Response {
    let filter = match album_filter(&state, &query).await {
        Ok(filter) => filter,
        Err(error) => return song_error(error, &context),
    };
    let order = match service::album_order(query.get("_sort")) {
        Ok(order) => order,
        Err(error) => return song_error(error, &context),
    };
    let (start, end) = match window(&query) {
        Ok(window) => window,
        Err(error) => return song_error(error, &context),
    };

    match service::album_list(
        state.completion(),
        state.credential(),
        &token.0,
        order,
        filter,
        start,
        end,
    )
    .await
    {
        Ok((albums, total)) => list_response(&albums, total),
        Err(error) => song_error(error, &context),
    }
}

async fn album_filter(
    state: &NavidromeState,
    query: &RawQuery,
) -> Result<AlbumListFilter, SongError> {
    reject_unknown(query, ALBUM_LIST_PARAMETERS)?;
    let direction = service::sort_direction(query.get("_order"))?;
    let artist_id = id_set(query, "artist_id", |raw| raw.parse::<ArtistId>().ok())?;
    let genre_id = id_set(query, "genre_id", |raw| {
        SelectorText::new(raw).ok().flatten()
    })?;
    let library_id = library_id(state.libraries(), query).await?;

    Ok(AlbumListFilter::new()
        .with_sort_direction(direction)
        .with_artist_id(artist_id)
        .with_genre(genre_id)
        .with_compilation(flag(query, "compilation")?)
        .with_has_rating(flag(query, "has_rating")?)
        .with_recently_played(flag(query, "recently_played")?)
        .with_starred(flag(query, "starred")?)
        .with_missing(flag(query, "missing")?)
        .with_name(selector_text(query, "name")?)
        .with_year(single_year(query)?)
        .with_library_id(library_id))
}

async fn artist_filter(
    state: &NavidromeState,
    query: &RawQuery,
) -> Result<ArtistListFilter, SongError> {
    reject_unknown(query, ARTIST_LIST_PARAMETERS)?;
    let direction = service::sort_direction(query.get("_order"))?;
    let library_id = library_id(state.libraries(), query).await?;

    Ok(ArtistListFilter::new()
        .with_sort_direction(direction)
        .with_name(selector_text(query, "name")?)
        .with_starred(flag(query, "starred")?)
        .with_role(role_set(query, "role")?)
        .with_missing(flag(query, "missing")?)
        .with_library_id(library_id))
}

pub(crate) async fn album_detail(
    State(state): State<NavidromeState>,
    Extension(context): Extension<RequestContext>,
    token: NdToken,
    Path(id): Path<String>,
) -> Response {
    match service::album_detail(state.completion(), state.credential(), &token.0, &id).await {
        Ok(album) => Json(album).into_response(),
        Err(error) => song_error(error, &context),
    }
}

pub(crate) async fn artist_list(
    State(state): State<NavidromeState>,
    Extension(context): Extension<RequestContext>,
    token: NdToken,
    query: RawQuery,
) -> Response {
    let filter = match artist_filter(&state, &query).await {
        Ok(filter) => filter,
        Err(error) => return song_error(error, &context),
    };
    let sort = match service::artist_sort(query.get("_sort")) {
        Ok(sort) => sort,
        Err(error) => return song_error(error, &context),
    };
    let (start, end) = match window(&query) {
        Ok(window) => window,
        Err(error) => return song_error(error, &context),
    };

    match service::artist_list(
        state.library(),
        state.credential(),
        &token.0,
        sort,
        filter,
        start,
        end,
    )
    .await
    {
        Ok((artists, total)) => list_response(&artists, total),
        Err(error) => song_error(error, &context),
    }
}

pub(crate) async fn artist_detail(
    State(state): State<NavidromeState>,
    Extension(context): Extension<RequestContext>,
    token: NdToken,
    Path(id): Path<String>,
) -> Response {
    match service::artist_detail(state.completion(), state.credential(), &token.0, &id).await {
        Ok(artist) => Json(artist).into_response(),
        Err(error) => song_error(error, &context),
    }
}

pub(crate) async fn genre_list(
    State(state): State<NavidromeState>,
    Extension(context): Extension<RequestContext>,
    token: NdToken,
) -> Response {
    match service::genre_list(state.library(), state.credential(), &token.0).await {
        Ok(genres) => Json(genres).into_response(),
        Err(error) => song_error(error, &context),
    }
}

fn song_window(query: &RawQuery) -> Result<(u32, PageLimit), SongError> {
    let start = page_start(query)?;
    let end = page_end(query)?;
    let span = end.saturating_sub(start).min(MAX_PAGE_LIMIT);
    let limit = PageLimit::new(if span == 0 { MAX_PAGE_LIMIT } else { span })
        .map_err(|_| SongError::InvalidSearch)?;

    Ok((start, limit))
}

fn window(query: &RawQuery) -> Result<(u32, u32), SongError> {
    Ok((page_start(query)?, page_end(query)?))
}

fn page_start(query: &RawQuery) -> Result<u32, SongError> {
    match query.get("_start") {
        Some(raw) => raw.trim().parse().map_err(|_| SongError::InvalidSearch),
        None => Ok(0),
    }
}

fn page_end(query: &RawQuery) -> Result<u32, SongError> {
    match query.get("_end") {
        Some(raw) => match raw.trim().parse::<i64>() {
            Ok(end) if end < 0 => Ok(0),
            Ok(end) => u32::try_from(end).map_err(|_| SongError::InvalidSearch),
            Err(_) => Err(SongError::InvalidSearch),
        },
        None => Ok(0),
    }
}

fn list_response<T: serde::Serialize>(items: &[T], total: u32) -> Response {
    let mut response = Json(items).into_response();
    response.headers_mut().insert(
        "x-total-count",
        HeaderValue::from_str(&total.to_string()).expect("a count is a valid header value"),
    );
    response
}

const PLAYLIST_LIST_PARAMETERS: &[&str] = &["_start", "_end", "_sort", "_order", "smart"];
const PLAYLIST_TRACK_PARAMETERS: &[&str] = &["_start", "_end", "_sort", "_order", "missing"];
const PLAYLIST_ITEM_SORTS: &[&str] = &["id"];
const PLAYLIST_LIST_SORTS: &[(&str, PlaylistOrder)] = &[
    ("name", PlaylistOrder::Name),
    ("updatedAt", PlaylistOrder::UpdatedAt),
    ("duration", PlaylistOrder::Duration),
    ("owner_name", PlaylistOrder::Owner),
    ("public", PlaylistOrder::Public),
    ("songCount", PlaylistOrder::TrackCount),
];

fn ascending(query: &RawQuery) -> Result<(), SongError> {
    match service::sort_direction(query.get("_order"))? {
        Some(SortDirection::Descending) => Err(SongError::InvalidRequest),
        None | Some(SortDirection::Ascending) => Ok(()),
    }
}

fn playlist_order(sort: Option<&str>) -> Result<PlaylistOrder, SongError> {
    match sort {
        None => Ok(PlaylistOrder::Name),
        Some(sort) => PLAYLIST_LIST_SORTS
            .iter()
            .find(|(name, _)| *name == sort)
            .map(|(_, order)| *order)
            .ok_or(SongError::InvalidRequest),
    }
}

fn playlist_list_query(query: &RawQuery) -> Result<(PlaylistOrder, SortDirection), SongError> {
    reject_unknown(query, PLAYLIST_LIST_PARAMETERS)?;

    let order = playlist_order(query.get("_sort"))?;
    let direction =
        service::sort_direction(query.get("_order"))?.unwrap_or(SortDirection::Ascending);

    match query.get("smart") {
        None | Some("false") => Ok((order, direction)),
        Some(_) => Err(SongError::InvalidRequest),
    }
}

fn playlist_track_query(query: &RawQuery) -> Result<(), SongError> {
    reject_unknown(query, PLAYLIST_TRACK_PARAMETERS)?;

    if query
        .get("_sort")
        .is_some_and(|sort| !PLAYLIST_ITEM_SORTS.contains(&sort))
    {
        return Err(SongError::InvalidRequest);
    }
    ascending(query)?;

    match query.get("missing") {
        None | Some("false") => Ok(()),
        Some(_) => Err(SongError::InvalidRequest),
    }
}

fn track_window(query: &RawQuery) -> Result<(u32, Option<u32>), SongError> {
    let start = page_start(query)?;

    match query.get("_end") {
        None => Ok((start, None)),
        Some(raw) => match raw.trim().parse::<i64>() {
            Ok(end) if end <= i64::from(start) => Ok((start, None)),
            Ok(end) => {
                let end = u32::try_from(end).map_err(|_| SongError::InvalidSearch)?;

                Ok((start, Some(end)))
            }
            Err(_) => Err(SongError::InvalidSearch),
        },
    }
}

fn smart_playlist(rules: Option<&serde_json::Value>, sync: Option<bool>) -> Result<(), SongError> {
    if rules.is_some_and(|rules| !rules.is_null()) || sync == Some(true) {
        return Err(SongError::InvalidRequest);
    }

    Ok(())
}

fn principal(raw: Option<&str>) -> Result<Option<Uuid>, SongError> {
    match raw {
        Some(raw) => raw.parse().map(Some).map_err(|_| SongError::InvalidRequest),
        None => Ok(None),
    }
}

fn track_ids(raw: Option<&[String]>) -> Result<Vec<TrackId>, SongError> {
    match raw {
        Some(ids) => ids
            .iter()
            .map(|raw| raw.parse().map_err(|_| SongError::InvalidRequest))
            .collect(),
        None => Ok(Vec::new()),
    }
}

fn parse_item_index(raw: &str) -> Result<u32, SongError> {
    let item_id = raw
        .trim()
        .parse::<u32>()
        .map_err(|_| SongError::InvalidRequest)?;

    service::item_index(item_id).ok_or(SongError::InvalidRequest)
}

fn playlist_create_request(
    body: &PlaylistCreateBody,
) -> Result<service::PlaylistCreate, SongError> {
    smart_playlist(body.rules.as_ref(), body.sync)?;

    let playlist_id = match body.id.as_deref() {
        Some(raw) => Some(raw.parse().map_err(|_| SongError::InvalidRequest)?),
        None => None,
    };
    let name = match body.name.as_deref() {
        Some(name) => Some(Playlist::validated_name(name).map_err(|_| SongError::InvalidRequest)?),
        None => None,
    };
    if playlist_id.is_none() && name.is_none() {
        return Err(SongError::InvalidRequest);
    }

    Ok(service::PlaylistCreate {
        playlist_id,
        owner_id: principal(body.owner_id.as_deref())?,
        name,
        comment: Playlist::validated_comment(body.comment.as_deref())
            .map_err(|_| SongError::InvalidRequest)?,
        public: body.public,
        additions: track_ids(body.ids.as_deref())?,
    })
}

fn playlist_metadata(
    body: &PlaylistUpdateBody,
) -> Result<(Option<Uuid>, PlaylistMetadataPatch), SongError> {
    smart_playlist(body.rules.as_ref(), body.sync)?;

    let name = match body.name.as_deref() {
        Some(name) => Some(Playlist::validated_name(name).map_err(|_| SongError::InvalidRequest)?),
        None => None,
    };
    let comment = match body.comment.as_deref() {
        Some(comment) => CommentPatch::Set(
            Playlist::validated_comment(Some(comment)).map_err(|_| SongError::InvalidRequest)?,
        ),
        None => CommentPatch::Keep,
    };

    Ok((
        principal(body.owner_id.as_deref())?,
        PlaylistMetadataPatch {
            name,
            comment,
            public: body.public,
        },
    ))
}

pub(crate) async fn playlist_list(
    State(state): State<NavidromeState>,
    Extension(context): Extension<RequestContext>,
    token: NdToken,
    query: RawQuery,
) -> Response {
    let (order, direction) = match playlist_list_query(&query) {
        Ok(parsed) => parsed,
        Err(error) => return song_error(error, &context),
    };
    let (start, end) = match window(&query) {
        Ok(window) => window,
        Err(error) => return song_error(error, &context),
    };

    match service::playlist_list(
        state.playlists(),
        state.credential(),
        &token.0,
        service::PlaylistListQuery {
            order,
            direction,
            start,
            end,
        },
        state.source_tags(),
    )
    .await
    {
        Ok((playlists, total)) => list_response(&playlists, total),
        Err(error) => song_error(error, &context),
    }
}

pub(crate) async fn playlist_create(
    State(state): State<NavidromeState>,
    Extension(context): Extension<RequestContext>,
    token: NdToken,
    query: RawQuery,
    body: Result<Json<PlaylistCreateBody>, JsonRejection>,
) -> Response {
    if let Err(error) = reject_unknown(&query, &[]) {
        return song_error(error, &context);
    }
    let Json(body) = match body {
        Ok(body) => body,
        Err(_) => return song_error(SongError::InvalidRequest, &context),
    };
    let request = match playlist_create_request(&body) {
        Ok(request) => request,
        Err(error) => return song_error(error, &context),
    };

    match service::playlist_create(state.playlists(), state.credential(), &token.0, request).await {
        Ok(id) => Json(PlaylistCreated { id }).into_response(),
        Err(error) => song_error(error, &context),
    }
}

pub(crate) async fn playlist_detail(
    State(state): State<NavidromeState>,
    Extension(context): Extension<RequestContext>,
    token: NdToken,
    Path(id): Path<String>,
    query: RawQuery,
) -> Response {
    if let Err(error) = reject_unknown(&query, &[]) {
        return song_error(error, &context);
    }

    match service::playlist_detail(
        state.playlists(),
        state.credential(),
        &token.0,
        &id,
        state.source_tags(),
    )
    .await
    {
        Ok(playlist) => Json(playlist).into_response(),
        Err(error) => song_error(error, &context),
    }
}

pub(crate) async fn playlist_update(
    State(state): State<NavidromeState>,
    Extension(context): Extension<RequestContext>,
    token: NdToken,
    Path(id): Path<String>,
    query: RawQuery,
    body: Result<Json<PlaylistUpdateBody>, JsonRejection>,
) -> Response {
    if let Err(error) = reject_unknown(&query, &[]) {
        return song_error(error, &context);
    }
    let Json(body) = match body {
        Ok(body) => body,
        Err(_) => return song_error(SongError::InvalidRequest, &context),
    };
    let (owner_id, metadata) = match playlist_metadata(&body) {
        Ok(metadata) => metadata,
        Err(error) => return song_error(error, &context),
    };

    match service::playlist_edit(
        state.playlists(),
        state.credential(),
        &token.0,
        &id,
        owner_id,
        metadata,
        PlaylistEntryChange::Unchanged,
    )
    .await
    {
        Ok(playlist) => Json(playlist).into_response(),
        Err(error) => song_error(error, &context),
    }
}

pub(crate) async fn playlist_delete(
    State(state): State<NavidromeState>,
    Extension(context): Extension<RequestContext>,
    token: NdToken,
    Path(id): Path<String>,
    query: RawQuery,
) -> Response {
    if let Err(error) = reject_unknown(&query, &[]) {
        return song_error(error, &context);
    }

    match service::playlist_delete(state.playlists(), state.credential(), &token.0, &id).await {
        Ok(()) => Json(()).into_response(),
        Err(error) => song_error(error, &context),
    }
}

pub(crate) async fn playlist_tracks(
    State(state): State<NavidromeState>,
    Extension(context): Extension<RequestContext>,
    token: NdToken,
    Path(id): Path<String>,
    query: RawQuery,
) -> Response {
    if let Err(error) = playlist_track_query(&query) {
        return song_error(error, &context);
    }
    let (start, end) = match track_window(&query) {
        Ok(window) => window,
        Err(error) => return song_error(error, &context),
    };

    match service::playlist_tracks(
        state.playlists(),
        state.credential(),
        &token.0,
        &id,
        start,
        end,
        state.source_tags(),
    )
    .await
    {
        Ok((songs, total)) => list_response(&songs, total),
        Err(error) => song_error(error, &context),
    }
}

pub(crate) async fn playlist_add_tracks(
    State(state): State<NavidromeState>,
    Extension(context): Extension<RequestContext>,
    token: NdToken,
    Path(id): Path<String>,
    query: RawQuery,
    body: Result<Json<PlaylistTracksBody>, JsonRejection>,
) -> Response {
    if let Err(error) = reject_unknown(&query, &[]) {
        return song_error(error, &context);
    }
    let Json(body) = match body {
        Ok(body) => body,
        Err(_) => return song_error(SongError::InvalidRequest, &context),
    };
    let additions = match track_ids(body.ids.as_deref()) {
        Ok(additions) => additions,
        Err(error) => return song_error(error, &context),
    };
    let added = u32::try_from(additions.len()).unwrap_or(u32::MAX);

    match service::playlist_edit(
        state.playlists(),
        state.credential(),
        &token.0,
        &id,
        None,
        PlaylistMetadataPatch::default(),
        service::entry_additions(&additions),
    )
    .await
    {
        Ok(_) => Json(PlaylistAdded { added }).into_response(),
        Err(error) => song_error(error, &context),
    }
}

pub(crate) async fn playlist_remove_tracks(
    State(state): State<NavidromeState>,
    Extension(context): Extension<RequestContext>,
    token: NdToken,
    Path(id): Path<String>,
    query: RawQuery,
) -> Response {
    if let Err(error) = reject_unknown(&query, &["id"]) {
        return song_error(error, &context);
    }
    let raw_ids = query.all("id");
    let mut removals = Vec::with_capacity(raw_ids.len());
    for raw in &raw_ids {
        let index = match parse_item_index(raw) {
            Ok(index) => index,
            Err(error) => return song_error(error, &context),
        };
        removals.push(OccurrenceSelector::Index(index));
    }
    let entries = if removals.is_empty() {
        PlaylistEntryChange::Unchanged
    } else {
        PlaylistEntryChange::Apply {
            removals,
            additions: Vec::new(),
        }
    };

    match service::playlist_edit(
        state.playlists(),
        state.credential(),
        &token.0,
        &id,
        None,
        PlaylistMetadataPatch::default(),
        entries,
    )
    .await
    {
        Ok(_) => Json(PlaylistRemoved {
            ids: raw_ids.iter().map(|raw| raw.trim().to_owned()).collect(),
        })
        .into_response(),
        Err(error) => song_error(error, &context),
    }
}

pub(crate) async fn playlist_move_track(
    State(state): State<NavidromeState>,
    Extension(context): Extension<RequestContext>,
    token: NdToken,
    Path((id, raw_item_id)): Path<(String, String)>,
    query: RawQuery,
    body: Result<Json<PlaylistMoveBody>, JsonRejection>,
) -> Response {
    if let Err(error) = reject_unknown(&query, &[]) {
        return song_error(error, &context);
    }
    let Json(body) = match body {
        Ok(body) => body,
        Err(_) => return song_error(SongError::InvalidRequest, &context),
    };
    let index = match parse_item_index(&raw_item_id) {
        Ok(index) => index,
        Err(error) => return song_error(error, &context),
    };
    let insert_before = match body.insert_before.trim().parse::<i64>() {
        Ok(insert_before) => insert_before,
        Err(_) => return song_error(SongError::InvalidRequest, &context),
    };

    match service::playlist_move(
        state.playlists(),
        state.credential(),
        &token.0,
        &id,
        OccurrenceSelector::Index(index),
        service::move_target(index, insert_before),
    )
    .await
    {
        Ok(()) => Json(PlaylistMoved {
            id: service::item_id(index).to_string(),
        })
        .into_response(),
        Err(error) => song_error(error, &context),
    }
}

pub(crate) async fn queue_read(
    State(state): State<NavidromeQueueState>,
    Extension(context): Extension<RequestContext>,
    token: NdToken,
) -> Response {
    match service::queue_read(
        state.navidrome().library(),
        state.navidrome().credential(),
        &token.0,
        state.queue(),
        state.navidrome().source_tags(),
    )
    .await
    {
        Ok(queue) => Json(queue).into_response(),
        Err(error) => song_error(error, &context),
    }
}

pub(crate) async fn queue_save(
    State(state): State<NavidromeQueueState>,
    Extension(context): Extension<RequestContext>,
    token: NdToken,
    body: Result<Json<QueueSaveBody>, JsonRejection>,
) -> Response {
    let Json(body) = match body {
        Ok(body) => body,
        Err(_) => return song_error(SongError::InvalidRequest, &context),
    };

    match service::queue_save(
        state.navidrome().credential(),
        &token.0,
        state.queue(),
        body,
    )
    .await
    {
        Ok(()) => Json(()).into_response(),
        Err(error) => song_error(error, &context),
    }
}

fn playlist_entry_error(error: PlaylistEntryError) -> Response {
    match error {
        PlaylistEntryError::UnknownEntry { .. } | PlaylistEntryError::IndexOutOfRange { .. } => {
            errors::error(StatusCode::NOT_FOUND, "Not found.")
        }
        PlaylistEntryError::UnknownTrack { .. } => {
            errors::error(StatusCode::UNPROCESSABLE_ENTITY, "Invalid request.")
        }
    }
}

fn song_error(error: SongError, context: &RequestContext) -> Response {
    match error {
        SongError::Unauthorized => errors::error(StatusCode::UNAUTHORIZED, "Not authenticated"),
        SongError::NotFound => errors::error(StatusCode::NOT_FOUND, "Not found."),
        SongError::InvalidSearch => errors::error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "Invalid search parameters.",
        ),
        SongError::InvalidRequest => {
            errors::error(StatusCode::UNPROCESSABLE_ENTITY, "Invalid request.")
        }
        SongError::Forbidden => errors::error(StatusCode::FORBIDDEN, "Forbidden."),
        SongError::Authentication(error) => {
            tracing::error!(error = %error, request_id = context.request_id().as_str(), "session storage failed");
            errors::error(StatusCode::INTERNAL_SERVER_ERROR, "Internal error.")
        }
        SongError::Catalogue(error) => {
            tracing::error!(error = %error, request_id = context.request_id().as_str(), "catalogue read failed");
            errors::error(StatusCode::INTERNAL_SERVER_ERROR, "Internal error.")
        }
        SongError::Playlist(PlaylistError::Entry(error)) => playlist_entry_error(error),
        SongError::Playlist(error) => {
            tracing::error!(error = %error, request_id = context.request_id().as_str(), "playlist request failed");
            errors::error(StatusCode::INTERNAL_SERVER_ERROR, "Internal error.")
        }
        SongError::Registry(error) => {
            tracing::error!(error = %error, request_id = context.request_id().as_str(), "library registry read failed");
            errors::error(StatusCode::INTERNAL_SERVER_ERROR, "Internal error.")
        }
        SongError::Queue(error) => queue_error(error, context),
    }
}

fn queue_error(error: RestoreQueueError, context: &RequestContext) -> Response {
    match error {
        RestoreQueueError::Invalid(_) => {
            errors::error(StatusCode::UNPROCESSABLE_ENTITY, "Invalid request.")
        }
        RestoreQueueError::Conflict { .. } => errors::error(StatusCode::CONFLICT, "Conflict."),
        RestoreQueueError::Disabled => {
            tracing::error!(
                request_id = context.request_id().as_str(),
                "restore queue is not configured"
            );
            errors::error(StatusCode::INTERNAL_SERVER_ERROR, "Internal error.")
        }
        RestoreQueueError::Repository(error) => {
            tracing::error!(error = %error, request_id = context.request_id().as_str(), "restore queue request failed");
            errors::error(StatusCode::INTERNAL_SERVER_ERROR, "Internal error.")
        }
    }
}
