use std::collections::HashMap;

use chrono::{DateTime, Utc};
use thiserror::Error;
use whio_subsonic_api::models;

use uuid::Uuid;

use crate::{
    facades::SourceTags,
    features::identity::{domain::Principal, service::CredentialService},
    features::metadata::MetadataProfile,
    features::music_library::catalogue::{
        CatalogueError, CompletionService, LibraryService, MAX_PAGE_LIMIT, PageLimit,
        ProviderError, SearchHit, SearchQuery, SearchRequest, SearchService, SearchWindows,
        TrackListFilter, TrackOrder,
    },
    features::music_library::discovery::{
        DiscoveryBasis, DiscoveryError, DiscoverySeed, DiscoveryService, RelatedRequest,
        SimilarTracksRequest, TopItem, TopRequest,
    },
    features::music_library::domain::{
        AlbumId, AlbumListFilter, AlbumListScope, AlbumOrder, ArtistId, ArtistInformation,
        ArtistListFilter, ArtistSort, IdSet, Playlist, PlaylistId, RandomSeed, Range,
        RefreshDemand, RestoreQueueValidationError, RoleSelector, RoleSet, SelectorText,
        SortDirection, StateTarget, TrackId, TrackView, ValidationError,
    },
    features::music_library::library::{
        LibraryId, LibraryRegistrationError, LibraryRegistry, ScanService,
    },
    features::music_library::library_state::{LibraryStateError, LibraryStateService},
    features::music_library::playlists::{
        PlaylistDetail, PlaylistEdit, PlaylistError, PlaylistService,
    },
    features::music_library::queue::{RestoreQueueError, RestoreQueueSave, RestoreQueueService},
    features::playback::{
        ActivityError, PlaybackActivityService, PlaybackClient, PlaybackRate, PlaybackReport,
        PlaybackState,
    },
    platform::raw_query::RawQuery,
    platform::request::RequestContext,
};

use super::mapping;

#[derive(Debug, Error)]
pub(crate) enum SearchError {
    #[error("invalid search parameters")]
    InvalidSearch,

    #[error("catalogue search failed")]
    Catalogue(#[source] CatalogueError),
}

pub(crate) struct SearchResult {
    pub(crate) songs: Vec<models::Child>,
    pub(crate) albums: Vec<models::AlbumId3>,
    pub(crate) artists: Vec<models::ArtistId3>,
}

pub(crate) async fn search(
    search_service: &SearchService,
    library: &LibraryService,
    context: &RequestContext,
    user: Option<&uuid::Uuid>,
    query: &str,
    windows: &SearchWindows,
    tags: SourceTags,
) -> Result<SearchResult, SearchError> {
    let request = SearchRequest::new(
        SearchQuery::new(query.to_owned()).map_err(|_| SearchError::InvalidSearch)?,
        *windows,
        album_artist_filter(!query.is_empty()).map_err(|_| SearchError::InvalidSearch)?,
    );

    let outcome = search_service
        .search(&request, user, context)
        .await
        .map_err(SearchError::Catalogue)?;
    let views = views_for_hits(library, &outcome.tracks, user)
        .await
        .map_err(SearchError::Catalogue)?;

    Ok(SearchResult {
        songs: views
            .iter()
            .map(|view| mapping::child_from_view(view, tags))
            .collect(),
        albums: outcome
            .albums
            .iter()
            .map(mapping::album_from_summary)
            .collect(),
        artists: outcome
            .artists
            .iter()
            .map(mapping::artist_from_summary)
            .collect(),
    })
}

async fn views_for_hits(
    library: &LibraryService,
    hits: &[SearchHit],
    user: Option<&Uuid>,
) -> Result<Vec<TrackView>, CatalogueError> {
    let track_ids = hits
        .iter()
        .map(|hit| hit.track().id().clone())
        .collect::<Vec<_>>();

    library.track_views(&track_ids, user).await
}

pub(crate) struct EnumerationWindow {
    pub(crate) count: u32,
    pub(crate) offset: u32,
}

impl EnumerationWindow {
    pub(crate) fn new(count: u32, offset: u32) -> Result<Self, SearchError> {
        if count > MAX_PAGE_LIMIT {
            return Err(SearchError::InvalidSearch);
        }

        Ok(Self { count, offset })
    }
}

pub(crate) struct EnumerationRequest {
    pub(crate) songs: EnumerationWindow,
    pub(crate) albums: EnumerationWindow,
    pub(crate) artists: EnumerationWindow,
}

pub(crate) async fn enumerate(
    library: &LibraryService,
    user: Option<&Uuid>,
    request: &EnumerationRequest,
    tags: SourceTags,
) -> Result<SearchResult, SearchError> {
    let songs = if request.songs.count == 0 {
        Vec::new()
    } else {
        let limit = PageLimit::new(request.songs.count).map_err(|_| SearchError::InvalidSearch)?;

        library
            .track_page(
                request.songs.offset,
                limit,
                TrackOrder::Title,
                &TrackListFilter::new(),
                user,
            )
            .await
            .map_err(SearchError::Catalogue)?
            .items()
            .iter()
            .map(|view| mapping::child_from_view(view, tags))
            .collect()
    };

    let albums = if request.albums.count == 0 {
        Vec::new()
    } else {
        library
            .album_list(
                request.albums.offset,
                request.albums.count,
                AlbumOrder::Name,
                AlbumListFilter::new(),
                user,
            )
            .await
            .map_err(SearchError::Catalogue)?
            .iter()
            .map(mapping::album_from_summary)
            .collect()
    };

    let artists = if request.artists.count == 0 {
        Vec::new()
    } else {
        let filter = album_artist_filter(false).map_err(|_| SearchError::InvalidSearch)?;

        library
            .artist_list(
                request.artists.offset,
                request.artists.count,
                ArtistSort::Name,
                &filter,
                user,
            )
            .await
            .map_err(SearchError::Catalogue)?
            .iter()
            .map(mapping::artist_from_summary)
            .collect()
    };

    Ok(SearchResult {
        songs,
        albums,
        artists,
    })
}

const DEFAULT_BROWSE_SIZE: u32 = 100;
const MAX_BROWSE_SIZE: u32 = 500;

#[derive(Debug, Error)]
pub(crate) enum BrowseError {
    #[error("invalid browse parameters")]
    InvalidRequest,

    #[error("missing browse parameter")]
    MissingParameter,

    #[error("unsupported album list type")]
    UnsupportedType,

    #[error("album not found")]
    AlbumNotFound,

    #[error("artist not found")]
    ArtistNotFound,

    #[error("song not found")]
    SongNotFound,

    #[error("library registry read failed")]
    Registry(#[source] LibraryRegistrationError),

    #[error("catalogue read failed")]
    Catalogue(#[source] CatalogueError),
}

#[derive(Debug, Error)]
pub(crate) enum StateError {
    #[error("invalid state parameters")]
    InvalidRequest,

    #[error("missing state parameter")]
    MissingParameter,

    #[error("state target not found")]
    NotFound,

    #[error("catalogue read failed")]
    Catalogue(#[source] CatalogueError),

    #[error("library state write failed")]
    State(#[source] LibraryStateError),
}

pub(crate) struct AlbumListRequest<'a> {
    pub(crate) list_type: &'a str,
    pub(crate) size: Option<&'a str>,
    pub(crate) offset: Option<&'a str>,
    pub(crate) from_year: Option<&'a str>,
    pub(crate) to_year: Option<&'a str>,
    pub(crate) genre: Option<&'a str>,
    pub(crate) library_id: Option<IdSet<LibraryId>>,
}

pub(crate) struct StarredResult {
    pub(crate) songs: Vec<models::Child>,
    pub(crate) albums: Vec<models::AlbumId3>,
    pub(crate) artists: Vec<models::ArtistId3>,
}

pub(crate) async fn album_list(
    library: &LibraryService,
    request: &AlbumListRequest<'_>,
    user: Option<&Uuid>,
) -> Result<Vec<models::AlbumId3>, BrowseError> {
    let size = match request.size {
        None => DEFAULT_BROWSE_SIZE,
        Some(raw) => raw
            .trim()
            .parse::<u32>()
            .map_err(|_| BrowseError::InvalidRequest)?,
    }
    .min(MAX_BROWSE_SIZE);
    let offset = match request.offset {
        None => 0,
        Some(raw) => raw
            .trim()
            .parse::<u32>()
            .map_err(|_| BrowseError::InvalidRequest)?,
    };
    let (order, filter) = album_list_order(request)?;
    let filter = filter.with_library_id(request.library_id.clone());

    library
        .album_list(offset, size, order, filter, user)
        .await
        .map(|albums| albums.iter().map(mapping::album_from_summary).collect())
        .map_err(BrowseError::Catalogue)
}

fn album_list_order(
    request: &AlbumListRequest<'_>,
) -> Result<(AlbumOrder, AlbumListFilter), BrowseError> {
    let filter = AlbumListFilter::new();

    match request.list_type {
        "alphabeticalByName" => Ok((AlbumOrder::Name, filter)),
        "alphabeticalByArtist" => Ok((AlbumOrder::AlbumArtist, filter)),
        "newest" => Ok((
            AlbumOrder::RecentlyAdded,
            filter.with_scope(AlbumListScope::Owned),
        )),
        "random" => Ok((AlbumOrder::Random, filter)),
        "starred" => Ok((AlbumOrder::StarredAt, filter.with_starred(Some(true)))),
        "frequent" => Ok((
            AlbumOrder::PlayCount,
            filter.with_play_count(Some(
                Range::new(Some(1), None).map_err(|_| BrowseError::InvalidRequest)?,
            )),
        )),
        "recent" => Ok((
            AlbumOrder::PlayDate,
            filter.with_recently_played(Some(true)),
        )),
        "highest" => Ok((AlbumOrder::Rating, filter.with_has_rating(Some(true)))),
        "byYear" => {
            let from = request
                .from_year
                .ok_or(BrowseError::MissingParameter)?
                .trim()
                .parse::<i32>()
                .map_err(|_| BrowseError::InvalidRequest)?;
            let to = request
                .to_year
                .ok_or(BrowseError::MissingParameter)?
                .trim()
                .parse::<i32>()
                .map_err(|_| BrowseError::InvalidRequest)?;
            let (from, to, direction) = if from > to {
                (to, from, SortDirection::Descending)
            } else {
                (from, to, SortDirection::Ascending)
            };
            let year = Range::new(Some(from), Some(to)).map_err(|_| BrowseError::InvalidRequest)?;

            Ok((
                AlbumOrder::MaxYear,
                filter
                    .with_year(Some(year))
                    .with_sort_direction(Some(direction)),
            ))
        }
        "byGenre" => {
            let genre = request
                .genre
                .ok_or(BrowseError::MissingParameter)?
                .to_owned();
            let genre = SelectorText::new(&genre)
                .map_err(|_| BrowseError::InvalidRequest)?
                .ok_or(BrowseError::MissingParameter)?;

            Ok((
                AlbumOrder::Name,
                filter.with_genre(Some(
                    IdSet::new([genre]).map_err(|_| BrowseError::InvalidRequest)?,
                )),
            ))
        }
        _ => Err(BrowseError::UnsupportedType),
    }
}

pub(crate) async fn music_folder_scope(
    libraries: &LibraryRegistry,
    requested: &[&str],
) -> Result<Option<IdSet<LibraryId>>, BrowseError> {
    if requested.is_empty() {
        return Ok(None);
    }

    let mut numeric_ids = Vec::with_capacity(requested.len());

    for raw in requested {
        numeric_ids.push(
            raw.trim()
                .parse::<i32>()
                .map_err(|_| BrowseError::InvalidRequest)?,
        );
    }

    match libraries.ids_for_numeric(&numeric_ids).await {
        Ok(Some(ids)) => Ok(Some(ids)),
        Ok(None) | Err(LibraryRegistrationError::Disabled) => Err(BrowseError::InvalidRequest),
        Err(error) => Err(BrowseError::Registry(error)),
    }
}

pub(crate) async fn music_folders(
    libraries: &LibraryRegistry,
) -> Result<Vec<models::MusicFolder>, BrowseError> {
    match libraries.libraries().await {
        Ok(libraries) => Ok(libraries.iter().map(mapping::music_folder).collect()),
        Err(LibraryRegistrationError::Disabled) => Ok(Vec::new()),
        Err(error) => Err(BrowseError::Registry(error)),
    }
}

pub(crate) async fn genres(library: &LibraryService) -> Result<Vec<models::Genre>, BrowseError> {
    library
        .genres()
        .await
        .map(|genres| genres.iter().map(mapping::genre_from_summary).collect())
        .map_err(BrowseError::Catalogue)
}

pub(crate) async fn starred(
    state: &LibraryStateService,
    library: &LibraryService,
    user: Uuid,
    tags: SourceTags,
) -> Result<StarredResult, StateError> {
    let starred = state.starred(user).await.map_err(StateError::State)?;
    let views = views_for_hits(library, starred.tracks(), Some(&user))
        .await
        .map_err(StateError::Catalogue)?;

    Ok(StarredResult {
        songs: views
            .iter()
            .map(|view| mapping::child_from_view(view, tags))
            .collect(),
        albums: starred
            .albums()
            .iter()
            .map(mapping::album_from_summary)
            .collect(),
        artists: starred
            .artists()
            .iter()
            .map(mapping::artist_from_summary)
            .collect(),
    })
}

pub(crate) async fn set_starred(
    state: &LibraryStateService,
    user: Uuid,
    song_ids: &[String],
    album_ids: &[String],
    artist_ids: &[String],
    starred: bool,
) -> Result<(), StateError> {
    if song_ids.is_empty() && album_ids.is_empty() && artist_ids.is_empty() {
        return Err(StateError::MissingParameter);
    }

    for raw in song_ids {
        let Ok(track_id) = raw.parse::<TrackId>() else {
            continue;
        };
        state
            .set_starred(user, StateTarget::Track(&track_id), starred)
            .await
            .map_err(StateError::State)?;
    }
    for raw in album_ids {
        let Ok(album_id) = raw.parse::<AlbumId>() else {
            continue;
        };
        state
            .set_starred(user, StateTarget::Album(&album_id), starred)
            .await
            .map_err(StateError::State)?;
    }
    for raw in artist_ids {
        let Ok(artist_id) = raw.parse::<ArtistId>() else {
            continue;
        };
        state
            .set_starred(user, StateTarget::Artist(&artist_id), starred)
            .await
            .map_err(StateError::State)?;
    }

    Ok(())
}

pub(crate) async fn set_rating(
    state: &LibraryStateService,
    user: Uuid,
    raw_id: &str,
    raw_rating: i64,
) -> Result<(), StateError> {
    let rating = match raw_rating {
        0 => None,
        1..=5 => Some(u8::try_from(raw_rating).expect("bounded rating fits in u8")),
        _ => return Err(StateError::InvalidRequest),
    };

    if state
        .rate_id(user, raw_id, rating)
        .await
        .map_err(StateError::State)?
    {
        Ok(())
    } else {
        Err(StateError::NotFound)
    }
}

const REPORT_PLAYBACK_PARAMETERS: &[&str] = &[
    "u",
    "v",
    "c",
    "p",
    "t",
    "s",
    "apiKey",
    "f",
    "mediaId",
    "mediaType",
    "positionMs",
    "state",
    "playbackRate",
    "ignoreScrobble",
];

#[derive(Debug, Error)]
pub(crate) enum ReportingError {
    #[error("invalid reporting parameters")]
    InvalidRequest,

    #[error("missing reporting parameter")]
    MissingParameter,

    #[error("reported media was not found")]
    NotFound,

    #[error("the reporting principal has no directory entry")]
    UnresolvedUser,

    #[error("playback activity failed")]
    Activity(#[source] ActivityError),

    #[error("catalogue read failed")]
    Catalogue(#[source] CatalogueError),

    #[error("user directory read failed")]
    Directory(#[source] Box<dyn std::error::Error + Send + Sync>),
}

struct ScrobbleEntry {
    track_id: TrackId,
    listened_at: DateTime<Utc>,
}

fn boolean(raw: &str) -> Option<bool> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "true" | "1" => Some(true),
        "false" | "0" => Some(false),
        _ => None,
    }
}

fn reporting_failure(error: ActivityError) -> ReportingError {
    match error {
        ActivityError::UnknownTrack => ReportingError::NotFound,
        other => ReportingError::Activity(other),
    }
}

fn scrobble_entries(
    ids: &[String],
    times: &[String],
    at: DateTime<Utc>,
) -> Result<Vec<ScrobbleEntry>, ReportingError> {
    if ids.is_empty() {
        return Err(ReportingError::MissingParameter);
    }

    if !times.is_empty() && times.len() != ids.len() {
        return Err(ReportingError::InvalidRequest);
    }

    ids.iter()
        .enumerate()
        .map(|(index, raw)| {
            let track_id = raw
                .parse::<TrackId>()
                .map_err(|_| ReportingError::InvalidRequest)?;
            let listened_at = match times.get(index) {
                Some(raw) => raw
                    .trim()
                    .parse::<i64>()
                    .ok()
                    .filter(|millis| *millis >= 0)
                    .and_then(DateTime::from_timestamp_millis)
                    .ok_or(ReportingError::InvalidRequest)?,
                None => at,
            };

            Ok(ScrobbleEntry {
                track_id,
                listened_at,
            })
        })
        .collect()
}

pub(crate) async fn scrobble(
    activity: &PlaybackActivityService,
    user: Uuid,
    client: &str,
    ids: &[String],
    times: &[String],
    submission: bool,
    at: DateTime<Utc>,
) -> Result<(), ReportingError> {
    let entries = scrobble_entries(ids, times, at)?;

    if submission {
        for entry in &entries {
            activity
                .submit_scrobble(user, &entry.track_id, entry.listened_at)
                .await
                .map_err(reporting_failure)?;
        }

        return Ok(());
    }

    let client = playback_client(client)?;

    for entry in &entries {
        let report = PlaybackReport::new(
            user,
            client.clone(),
            entry.track_id.clone(),
            PlaybackState::Playing,
            0,
            PlaybackRate::default(),
            entry.listened_at,
        )
        .with_ignored_scrobble(true);

        activity.report(&report).await.map_err(reporting_failure)?;
    }

    Ok(())
}

pub(crate) fn scrobble_submission(query: &RawQuery) -> Result<bool, ReportingError> {
    match query.get("submission") {
        Some(raw) => boolean(raw).ok_or(ReportingError::InvalidRequest),
        None => Ok(true),
    }
}

pub(crate) struct PlaybackReportRequest {
    media_id: TrackId,
    state: PlaybackState,
    position_ms: u64,
    playback_rate: PlaybackRate,
    ignore_scrobble: bool,
}

impl PlaybackReportRequest {
    pub(crate) fn parse(query: &RawQuery) -> Result<Self, ReportingError> {
        for name in query.names() {
            if !REPORT_PLAYBACK_PARAMETERS.contains(&name) {
                return Err(ReportingError::InvalidRequest);
            }
        }

        let media_id = query
            .get("mediaId")
            .ok_or(ReportingError::MissingParameter)?
            .parse::<TrackId>()
            .map_err(|_| ReportingError::InvalidRequest)?;

        let media_type = query
            .get("mediaType")
            .ok_or(ReportingError::MissingParameter)?;

        if media_type != "song" {
            return Err(ReportingError::InvalidRequest);
        }

        let position_ms = query
            .get("positionMs")
            .ok_or(ReportingError::MissingParameter)?
            .trim()
            .parse::<u64>()
            .map_err(|_| ReportingError::InvalidRequest)?;

        let state = match query.get("state").ok_or(ReportingError::MissingParameter)? {
            "starting" => PlaybackState::Starting,
            "playing" => PlaybackState::Playing,
            "paused" => PlaybackState::Paused,
            "stopped" => PlaybackState::Stopped,
            _ => return Err(ReportingError::InvalidRequest),
        };

        let playback_rate = match query.get("playbackRate") {
            Some(raw) => PlaybackRate::new(
                raw.trim()
                    .parse::<f64>()
                    .map_err(|_| ReportingError::InvalidRequest)?,
            )
            .map_err(|_| ReportingError::InvalidRequest)?,
            None => PlaybackRate::default(),
        };

        let ignore_scrobble = match query.get("ignoreScrobble") {
            Some(raw) => boolean(raw).ok_or(ReportingError::InvalidRequest)?,
            None => false,
        };

        Ok(Self {
            media_id,
            state,
            position_ms,
            playback_rate,
            ignore_scrobble,
        })
    }
}

fn playback_client(raw: &str) -> Result<PlaybackClient, ReportingError> {
    PlaybackClient::new(raw.to_owned()).map_err(|_| ReportingError::InvalidRequest)
}

pub(crate) async fn report_playback(
    activity: &PlaybackActivityService,
    user: Uuid,
    client: &str,
    request: &PlaybackReportRequest,
    at: DateTime<Utc>,
) -> Result<(), ReportingError> {
    let report = PlaybackReport::new(
        user,
        playback_client(client)?,
        request.media_id.clone(),
        request.state,
        request.position_ms,
        request.playback_rate,
        at,
    )
    .with_ignored_scrobble(request.ignore_scrobble);

    activity
        .report(&report)
        .await
        .map(|_| ())
        .map_err(reporting_failure)
}

pub(crate) async fn now_playing(
    activity: &PlaybackActivityService,
    library: &LibraryService,
    credential: &CredentialService,
    principal: &Principal,
    tags: SourceTags,
    at: DateTime<Utc>,
) -> Result<Vec<models::NowPlayingEntry>, ReportingError> {
    let username = reporting_username(credential, principal).await?;
    let sessions = activity.now_playing(principal.user_id.as_uuid(), at);
    let track_ids = sessions
        .iter()
        .map(|session| session.track_id().clone())
        .collect::<Vec<_>>();
    let views = library
        .track_views(&track_ids, Some(&principal.user_id.as_uuid()))
        .await
        .map_err(ReportingError::Catalogue)?;
    let by_id = views
        .into_iter()
        .map(|view| (view.track().id().clone(), view))
        .collect::<HashMap<TrackId, TrackView>>();
    let mut entries = Vec::new();

    for session in sessions {
        let Some(view) = by_id.get(session.track_id()) else {
            continue;
        };

        entries.push(mapping::now_playing_entry(
            view, &session, &username, tags, at,
        ));
    }

    Ok(entries)
}

async fn reporting_username(
    credential: &CredentialService,
    principal: &Principal,
) -> Result<String, ReportingError> {
    credential
        .directory(principal)
        .await
        .map_err(|error| ReportingError::Directory(Box::new(error)))?
        .into_iter()
        .find(|user| user.id == principal.user_id)
        .map(|user| user.username)
        .ok_or(ReportingError::UnresolvedUser)
}

pub(crate) async fn album_detail(
    completion: &CompletionService,
    raw_id: &str,
    user: Option<&Uuid>,
    tags: SourceTags,
) -> Result<models::AlbumId3WithSongs, BrowseError> {
    let album_id: AlbumId = raw_id.parse().map_err(|_| BrowseError::AlbumNotFound)?;
    let detail = completion
        .album_detail(&album_id, user)
        .await
        .map_err(BrowseError::Catalogue)?
        .ok_or(BrowseError::AlbumNotFound)?;

    Ok(mapping::album_detail_with_songs(&detail, tags))
}

pub(crate) fn album_artist_filter(
    provider_identity: bool,
) -> Result<ArtistListFilter, ValidationError> {
    let role = RoleSet::new([RoleSelector::AlbumArtist])?;

    Ok(ArtistListFilter::new()
        .with_role(Some(role))
        .with_provider_identity(provider_identity))
}

pub(crate) async fn artists(
    library: &LibraryService,
    user: Option<&Uuid>,
) -> Result<Vec<models::ArtistId3>, BrowseError> {
    let filter = album_artist_filter(false).map_err(|_| BrowseError::InvalidRequest)?;

    library
        .artist_list(0, u32::MAX, ArtistSort::Name, &filter, user)
        .await
        .map(|artists| artists.iter().map(mapping::artist_from_summary).collect())
        .map_err(BrowseError::Catalogue)
}

pub(crate) async fn song_detail(
    library: &LibraryService,
    raw_id: &str,
    user: Option<&Uuid>,
    tags: SourceTags,
) -> Result<models::Child, BrowseError> {
    let track_id: TrackId = raw_id.parse().map_err(|_| BrowseError::SongNotFound)?;
    let mut views = library
        .track_views(&[track_id], user)
        .await
        .map_err(BrowseError::Catalogue)?;
    let view = views.pop().ok_or(BrowseError::SongNotFound)?;

    Ok(mapping::child_from_view(&view, tags))
}

pub(crate) async fn artist_detail(
    completion: &CompletionService,
    raw_id: &str,
    user: Option<&Uuid>,
) -> Result<models::ArtistWithAlbumsId3, BrowseError> {
    let artist_id: ArtistId = raw_id.parse().map_err(|_| BrowseError::ArtistNotFound)?;
    let detail = completion
        .artist_detail(&artist_id, user)
        .await
        .map_err(BrowseError::Catalogue)?
        .ok_or(BrowseError::ArtistNotFound)?;

    Ok(mapping::artist_with_albums(&detail))
}

const ARTIST_INFO_LANGUAGE: &str = "en";
const DEFAULT_ARTIST_INFO_COUNT: u32 = 20;
const DEFAULT_SONG_COUNT: u32 = 50;

pub(crate) fn artist_info_count(raw: Option<&str>) -> Result<u32, BrowseError> {
    browse_size(raw, DEFAULT_ARTIST_INFO_COUNT)
}

pub(crate) fn song_count(raw: Option<&str>) -> Result<u32, BrowseError> {
    browse_size(raw, DEFAULT_SONG_COUNT)
}

pub(crate) fn include_not_present(raw: Option<&str>) -> Result<bool, BrowseError> {
    match raw {
        None => Ok(false),
        Some(raw) => boolean(raw).ok_or(BrowseError::InvalidRequest),
    }
}

async fn load_artist_information(
    library: &LibraryService,
    raw_id: &str,
    user: Option<&Uuid>,
) -> Result<ArtistInformation, BrowseError> {
    let artist_id: ArtistId = raw_id.parse().map_err(|_| BrowseError::ArtistNotFound)?;

    library
        .artist_information(
            &artist_id,
            user,
            &MetadataProfile::artist_display(),
            ARTIST_INFO_LANGUAGE,
            RefreshDemand::RequestIfDue,
        )
        .await
        .map_err(BrowseError::Catalogue)?
        .ok_or(BrowseError::ArtistNotFound)
}

pub(crate) async fn artist_info(
    library: &LibraryService,
    raw_id: &str,
    user: Option<&Uuid>,
) -> Result<models::ArtistInfo, BrowseError> {
    Ok(mapping::artist_info(
        &load_artist_information(library, raw_id, user).await?,
    ))
}

pub(crate) async fn artist_info2(
    library: &LibraryService,
    discovery: Option<&DiscoveryService>,
    raw_id: &str,
    user: Option<&Uuid>,
    count: u32,
) -> Result<models::ArtistInfo2, BrowseError> {
    let information = load_artist_information(library, raw_id, user).await?;
    let artist_id = information.artist.artist.artist().id().clone();
    let discovery = require_discovery(discovery)?;
    let similar = similar_artists(library, discovery, &artist_id, user, count).await?;

    Ok(mapping::artist_info2(&information, similar))
}

fn require_discovery(
    discovery: Option<&DiscoveryService>,
) -> Result<&DiscoveryService, BrowseError> {
    match discovery {
        Some(discovery) => Ok(discovery),
        None => {
            tracing::error!("the discovery service is not configured");

            Err(BrowseError::Catalogue(CatalogueError::Provider(
                ProviderError::ProviderUnavailable,
            )))
        }
    }
}

fn discovery_failure(error: DiscoveryError) -> BrowseError {
    match error {
        DiscoveryError::UnknownSeed => BrowseError::ArtistNotFound,
        DiscoveryError::UnsupportedBasis { .. } => {
            BrowseError::Catalogue(CatalogueError::Provider(ProviderError::Internal))
        }
        DiscoveryError::Unavailable => {
            BrowseError::Catalogue(CatalogueError::Provider(ProviderError::ProviderUnavailable))
        }
        DiscoveryError::Repository(error) => {
            BrowseError::Catalogue(CatalogueError::Repository(error))
        }
    }
}

fn local_basis_limit(count: u32) -> Result<PageLimit, BrowseError> {
    PageLimit::new(count).map_err(|_| BrowseError::InvalidRequest)
}

async fn songs_from_top(
    library: &LibraryService,
    items: &[TopItem],
    user: Option<&Uuid>,
    tags: SourceTags,
) -> Result<Vec<models::Child>, BrowseError> {
    let track_ids = items
        .iter()
        .filter_map(|item| item.canonical.clone())
        .collect::<Vec<_>>();
    let views = library
        .track_views(&track_ids, user)
        .await
        .map_err(BrowseError::Catalogue)?;
    let mut by_id = views
        .into_iter()
        .map(|view| (view.track().id().clone(), view))
        .collect::<HashMap<TrackId, TrackView>>();
    let mut songs = Vec::new();
    for item in items {
        let Some(track_id) = item.canonical.as_ref() else {
            continue;
        };
        let Some(view) = by_id.remove(track_id) else {
            continue;
        };
        songs.push(mapping::child_from_view(&view, tags));
    }

    Ok(songs)
}

pub(crate) async fn similar_artists(
    library: &LibraryService,
    discovery: &DiscoveryService,
    artist_id: &ArtistId,
    user: Option<&Uuid>,
    count: u32,
) -> Result<Vec<models::ArtistId3>, BrowseError> {
    let result = discovery
        .related(RelatedRequest {
            seed: DiscoverySeed::Artist(artist_id.clone()),
            basis: DiscoveryBasis::Library,
            window: None,
            limit: local_basis_limit(count)?,
            user: user.copied(),
        })
        .await
        .map_err(discovery_failure)?;
    let mut artists = Vec::new();
    for item in result.items {
        let Some(canonical) = item.canonical else {
            continue;
        };
        let Some(detail) = library
            .artist_detail(&canonical, user)
            .await
            .map_err(BrowseError::Catalogue)?
        else {
            continue;
        };
        artists.push(mapping::artist_from_summary(&detail.artist));
    }

    Ok(artists)
}

pub(crate) async fn top_songs(
    library: &LibraryService,
    discovery: Option<&DiscoveryService>,
    artist: &str,
    user: Option<&Uuid>,
    count: u32,
    tags: SourceTags,
) -> Result<models::TopSongs, BrowseError> {
    let Some(artist_id) = library
        .artist_by_exact_name(artist)
        .await
        .map_err(BrowseError::Catalogue)?
    else {
        return Err(BrowseError::ArtistNotFound);
    };
    let discovery = require_discovery(discovery)?;
    let result = discovery
        .top(TopRequest {
            seed: DiscoverySeed::Artist(artist_id),
            basis: DiscoveryBasis::Library,
            window: None,
            limit: local_basis_limit(count)?,
            user: user.copied(),
        })
        .await
        .map_err(discovery_failure)?;
    let songs = songs_from_top(library, &result.items, user, tags).await?;

    Ok(models::TopSongs { song: Some(songs) })
}

async fn similar_seed(
    library: &LibraryService,
    raw_id: &str,
    user: Option<&Uuid>,
) -> Result<DiscoverySeed, BrowseError> {
    let id: Uuid = raw_id.parse().map_err(|_| BrowseError::ArtistNotFound)?;
    let track_id = TrackId::from_uuid(id);
    if library
        .track(&track_id)
        .await
        .map_err(BrowseError::Catalogue)?
        .is_some()
    {
        return Ok(DiscoverySeed::Track(track_id));
    }
    let Some(detail) = library
        .album_detail(&AlbumId::from_uuid(id), user)
        .await
        .map_err(BrowseError::Catalogue)?
    else {
        return Err(BrowseError::ArtistNotFound);
    };
    let Some(album_artist_id) = detail.album.album().album_artist_id().cloned() else {
        return Err(BrowseError::ArtistNotFound);
    };

    Ok(DiscoverySeed::Artist(album_artist_id))
}

async fn similar_songs_for(
    library: &LibraryService,
    discovery: &DiscoveryService,
    seed: DiscoverySeed,
    user: Option<&Uuid>,
    count: u32,
    tags: SourceTags,
) -> Result<Vec<models::Child>, BrowseError> {
    let result = discovery
        .similar_tracks(SimilarTracksRequest {
            seed,
            basis: DiscoveryBasis::Library,
            window: None,
            limit: local_basis_limit(count)?,
            user: user.copied(),
        })
        .await
        .map_err(discovery_failure)?;

    songs_from_top(library, &result.items, user, tags).await
}

pub(crate) async fn similar_songs(
    library: &LibraryService,
    discovery: Option<&DiscoveryService>,
    raw_id: &str,
    user: Option<&Uuid>,
    count: u32,
    tags: SourceTags,
) -> Result<models::SimilarSongs, BrowseError> {
    let seed = similar_seed(library, raw_id, user).await?;
    let discovery = require_discovery(discovery)?;
    let songs = similar_songs_for(library, discovery, seed, user, count, tags).await?;

    Ok(models::SimilarSongs { song: Some(songs) })
}

pub(crate) async fn similar_songs2(
    library: &LibraryService,
    discovery: Option<&DiscoveryService>,
    raw_id: &str,
    user: Option<&Uuid>,
    count: u32,
    tags: SourceTags,
) -> Result<models::SimilarSongs2, BrowseError> {
    let artist_id: ArtistId = raw_id.parse().map_err(|_| BrowseError::ArtistNotFound)?;
    let discovery = require_discovery(discovery)?;
    let songs = similar_songs_for(
        library,
        discovery,
        DiscoverySeed::Artist(artist_id),
        user,
        count,
        tags,
    )
    .await?;

    Ok(models::SimilarSongs2 { song: Some(songs) })
}

pub(crate) fn artist_indexes(artists: Vec<models::ArtistId3>) -> Vec<models::IndexId3> {
    mapping::group_by_index(artists, |artist| artist.name.as_str())
        .into_iter()
        .map(|(name, artists)| models::IndexId3 {
            name,
            artist: Some(artists),
        })
        .collect()
}

pub(crate) struct SongsByGenreRequest<'a> {
    pub(crate) genre: Option<&'a str>,
    pub(crate) count: Option<&'a str>,
    pub(crate) offset: Option<&'a str>,
    pub(crate) library_id: Option<IdSet<LibraryId>>,
}

pub(crate) struct RandomSongsRequest<'a> {
    pub(crate) size: Option<&'a str>,
    pub(crate) genre: Option<&'a str>,
    pub(crate) from_year: Option<&'a str>,
    pub(crate) to_year: Option<&'a str>,
    pub(crate) library_id: Option<IdSet<LibraryId>>,
}

const DEFAULT_GENRE_SIZE: u32 = 10;
const DEFAULT_RANDOM_SIZE: u32 = 10;

pub(crate) async fn songs_by_genre(
    library: &LibraryService,
    user: Option<&Uuid>,
    request: &SongsByGenreRequest<'_>,
    tags: SourceTags,
) -> Result<Vec<models::Child>, BrowseError> {
    let genre = match request.genre {
        None => return Ok(Vec::new()),
        Some(raw) => match SelectorText::new(raw).map_err(|_| BrowseError::InvalidRequest)? {
            Some(genre) => genre,
            None => return Ok(Vec::new()),
        },
    };
    let count = browse_size(request.count, DEFAULT_GENRE_SIZE)?;
    let offset = match request.offset {
        None => 0,
        Some(raw) => raw
            .trim()
            .parse::<u32>()
            .map_err(|_| BrowseError::InvalidRequest)?,
    };

    if count == 0 {
        return Ok(Vec::new());
    }

    let limit = PageLimit::new(count).map_err(|_| BrowseError::InvalidRequest)?;
    let set = IdSet::new([genre]).map_err(|_| BrowseError::InvalidRequest)?;
    let filter = TrackListFilter::new()
        .with_genre(Some(set))
        .with_library_id(request.library_id.clone());

    library
        .track_page(offset, limit, TrackOrder::Title, &filter, user)
        .await
        .map(|page| {
            page.items()
                .iter()
                .map(|view| mapping::child_from_view(view, tags))
                .collect()
        })
        .map_err(BrowseError::Catalogue)
}

pub(crate) async fn random_songs(
    library: &LibraryService,
    user: Option<&Uuid>,
    request: &RandomSongsRequest<'_>,
    seed: RandomSeed,
    tags: SourceTags,
) -> Result<Vec<models::Child>, BrowseError> {
    let size = browse_size(request.size, DEFAULT_RANDOM_SIZE)?;
    let mut filter = TrackListFilter::new().with_random_seed(Some(seed));

    if let Some(raw) = request.genre {
        let genre = SelectorText::new(raw).map_err(|_| BrowseError::InvalidRequest)?;

        if let Some(genre) = genre {
            let set = IdSet::new([genre]).map_err(|_| BrowseError::InvalidRequest)?;
            filter = filter.with_genre(Some(set));
        }
    }

    let from = year_bound(request.from_year)?;
    let to = year_bound(request.to_year)?;

    if from.is_some() || to.is_some() {
        let Ok(year) = Range::new(from, to) else {
            return Ok(Vec::new());
        };

        filter = filter.with_year(Some(year));
    }

    filter = filter.with_library_id(request.library_id.clone());

    if size == 0 {
        return Ok(Vec::new());
    }

    let limit = PageLimit::new(size).map_err(|_| BrowseError::InvalidRequest)?;

    library
        .track_page(0, limit, TrackOrder::Random, &filter, user)
        .await
        .map(|page| {
            page.items()
                .iter()
                .map(|view| mapping::child_from_view(view, tags))
                .collect()
        })
        .map_err(BrowseError::Catalogue)
}

fn browse_size(raw: Option<&str>, default: u32) -> Result<u32, BrowseError> {
    match raw {
        None => Ok(default),
        Some(raw) => raw
            .trim()
            .parse::<u32>()
            .map(|size| size.min(MAX_BROWSE_SIZE))
            .map_err(|_| BrowseError::InvalidRequest),
    }
}

fn year_bound(raw: Option<&str>) -> Result<Option<i32>, BrowseError> {
    match raw {
        None => Ok(None),
        Some(raw) => {
            let year = raw
                .trim()
                .parse::<i32>()
                .map_err(|_| BrowseError::InvalidRequest)?;

            Ok((year != 0).then_some(year))
        }
    }
}

pub(crate) async fn indexes(
    library: &LibraryService,
    scanner: &ScanService,
    user: Option<&Uuid>,
) -> Result<models::Indexes, BrowseError> {
    let filter = album_artist_filter(false).map_err(|_| BrowseError::InvalidRequest)?;
    let artists = library
        .artist_list(0, u32::MAX, ArtistSort::Name, &filter, user)
        .await
        .map_err(BrowseError::Catalogue)?;
    let last_modified = scanner.status().last_scan.unwrap_or_else(chrono::Utc::now);

    Ok(models::Indexes {
        ignored_articles: mapping::IGNORED_ARTICLES.to_owned(),
        last_modified: i32::try_from(last_modified.timestamp_millis()).unwrap_or(i32::MAX),
        shortcut: None,
        child: None,
        index: Some(mapping::artist_index(artists)),
    })
}

pub(crate) async fn music_directory(
    library: &LibraryService,
    raw_id: &str,
    user: Option<&Uuid>,
    tags: SourceTags,
) -> Result<models::Directory, BrowseError> {
    let artist_id: ArtistId = raw_id.parse().map_err(|_| BrowseError::ArtistNotFound)?;

    if let Some(detail) = library
        .artist_detail(&artist_id, user)
        .await
        .map_err(BrowseError::Catalogue)?
    {
        return Ok(mapping::music_directory(
            mapping::DirectoryTarget::Artist(&detail),
            tags,
        ));
    }

    let album_id: AlbumId = raw_id.parse().map_err(|_| BrowseError::AlbumNotFound)?;

    match library
        .album_detail(&album_id, user)
        .await
        .map_err(BrowseError::Catalogue)?
    {
        Some(detail) => Ok(mapping::music_directory(
            mapping::DirectoryTarget::Album(&detail),
            tags,
        )),
        None => Err(BrowseError::AlbumNotFound),
    }
}

#[derive(Debug, Error)]
pub(crate) enum PlaylistRequestError {
    #[error("the playlist id parameter is missing")]
    MissingPlaylistId,

    #[error("the playlist id does not name a playlist")]
    UnknownPlaylist,

    #[error("a playlist name is required")]
    MissingName,

    #[error("invalid playlist name")]
    InvalidName,

    #[error("invalid playlist comment")]
    InvalidComment,

    #[error("invalid track id")]
    InvalidTrackId,

    #[error("invalid song index")]
    InvalidIndex,
}

pub(crate) struct PlaylistCreate {
    pub(crate) playlist_id: Option<PlaylistId>,
    pub(crate) name: String,
    pub(crate) additions: Vec<TrackId>,
}

pub(crate) struct PlaylistUpdate {
    pub(crate) playlist_id: PlaylistId,
    pub(crate) name: Option<String>,
    pub(crate) comment: Option<String>,
    pub(crate) public: Option<bool>,
    pub(crate) additions: Vec<TrackId>,
    pub(crate) removals: Vec<usize>,
}

pub(crate) enum PlaylistCreated {
    Created(PlaylistDetail),
    Forbidden,
    NotFound,
}

pub(crate) enum PlaylistEdited {
    Applied,
    Forbidden,
    NotFound,
}

pub(crate) fn playlist_id(
    query: &RawQuery,
    parameter: &str,
) -> Result<PlaylistId, PlaylistRequestError> {
    let Some(raw) = query.get(parameter) else {
        return Err(PlaylistRequestError::MissingPlaylistId);
    };

    raw.parse()
        .map_err(|_| PlaylistRequestError::UnknownPlaylist)
}

fn track_ids(query: &RawQuery, parameter: &str) -> Result<Vec<TrackId>, PlaylistRequestError> {
    query
        .all(parameter)
        .into_iter()
        .map(|raw| {
            raw.parse()
                .map_err(|_| PlaylistRequestError::InvalidTrackId)
        })
        .collect()
}

pub(crate) fn playlist_create(query: &RawQuery) -> Result<PlaylistCreate, PlaylistRequestError> {
    let playlist_id = query
        .get("playlistId")
        .map(|_| playlist_id(query, "playlistId"))
        .transpose()?;
    let name = query.get("name").map(str::trim).unwrap_or_default();
    let name = if name.is_empty() {
        if playlist_id.is_none() {
            return Err(PlaylistRequestError::MissingName);
        }

        String::new()
    } else {
        Playlist::validated_name(name).map_err(|_| PlaylistRequestError::InvalidName)?
    };

    Ok(PlaylistCreate {
        playlist_id,
        name,
        additions: track_ids(query, "songId")?,
    })
}

pub(crate) fn playlist_update(query: &RawQuery) -> Result<PlaylistUpdate, PlaylistRequestError> {
    let playlist_id = playlist_id(query, "playlistId")?;
    let name = match query.get("name") {
        Some(raw) => {
            Some(Playlist::validated_name(raw).map_err(|_| PlaylistRequestError::InvalidName)?)
        }
        None => None,
    };
    let comment = match query.get("comment") {
        Some(raw) => Some(
            Playlist::validated_comment(Some(raw))
                .map_err(|_| PlaylistRequestError::InvalidComment)?
                .unwrap_or_default(),
        ),
        None => None,
    };
    let removals = query
        .all("songIndexToRemove")
        .into_iter()
        .map(|raw| {
            raw.parse::<usize>()
                .map_err(|_| PlaylistRequestError::InvalidIndex)
        })
        .collect::<Result<Vec<_>, _>>()?;

    Ok(PlaylistUpdate {
        playlist_id,
        name,
        comment,
        public: query
            .get("public")
            .map(|raw| raw.trim().eq_ignore_ascii_case("true")),
        additions: track_ids(query, "songIdToAdd")?,
        removals,
    })
}

async fn forbidden(
    service: &PlaylistService,
    user: Uuid,
    playlist_id: &PlaylistId,
) -> Result<bool, PlaylistError> {
    Ok(service
        .detail(user, playlist_id)
        .await?
        .is_some_and(|detail| !detail.editable))
}

pub(crate) async fn create_playlist(
    service: &PlaylistService,
    user: Uuid,
    request: PlaylistCreate,
) -> Result<PlaylistCreated, PlaylistError> {
    let requested = request.playlist_id.clone();

    match service
        .create_or_append(user, request.playlist_id, &request.name, &request.additions)
        .await?
    {
        Some(detail) => Ok(PlaylistCreated::Created(detail)),
        None => match requested {
            Some(playlist_id) if forbidden(service, user, &playlist_id).await? => {
                Ok(PlaylistCreated::Forbidden)
            }
            _ => Ok(PlaylistCreated::NotFound),
        },
    }
}

pub(crate) async fn update_playlist(
    service: &PlaylistService,
    user: Uuid,
    request: &PlaylistUpdate,
) -> Result<PlaylistEdited, PlaylistError> {
    let applied = service
        .edit(
            user,
            &request.playlist_id,
            PlaylistEdit {
                name: request.name.as_deref(),
                comment: request.comment.as_deref(),
                public: request.public,
                additions: &request.additions,
                removals: request.removals.clone(),
            },
        )
        .await?;

    if applied {
        return Ok(PlaylistEdited::Applied);
    }

    if forbidden(service, user, &request.playlist_id).await? {
        return Ok(PlaylistEdited::Forbidden);
    }

    Ok(PlaylistEdited::NotFound)
}

pub(crate) async fn delete_playlist(
    service: &PlaylistService,
    user: Uuid,
    playlist_id: &PlaylistId,
) -> Result<PlaylistEdited, PlaylistError> {
    if service.delete(user, playlist_id).await? {
        return Ok(PlaylistEdited::Applied);
    }

    if forbidden(service, user, playlist_id).await? {
        return Ok(PlaylistEdited::Forbidden);
    }

    Ok(PlaylistEdited::NotFound)
}

#[derive(Debug, Error)]
pub(crate) enum PlayQueueError {
    #[error("the queue holds at most {max_entries} entries")]
    TooManyEntries { max_entries: usize },

    #[error("a non-empty queue must name its current index")]
    MissingCurrentIndex,

    #[error("the current index is outside the queue")]
    InvalidCurrentIndex,

    #[error("the current parameter does not name an entry in the queue")]
    CurrentNotInQueue,

    #[error("the current parameter is meaningless without a queue")]
    CurrentWithoutQueue,

    #[error("the position is not a count of milliseconds")]
    InvalidPosition,

    #[error("a queue entry is not a track id")]
    InvalidTrackId,

    #[error("the queue names a track that is not in the library")]
    UnknownTrack,

    #[error("a queued entry names a track the catalogue does not resolve")]
    UnresolvedEntry,

    #[error("the caller is not in the user directory")]
    UnresolvedUser,

    #[error("the restore queue is not configured")]
    Disabled,

    #[error("restore queue request failed")]
    Queue(#[source] RestoreQueueError),

    #[error("catalogue read failed")]
    Catalogue(#[source] CatalogueError),

    #[error("user directory read failed")]
    Directory(#[source] Box<dyn std::error::Error + Send + Sync>),
}

impl PlayQueueError {
    fn from_queue(error: RestoreQueueError) -> Self {
        match error {
            RestoreQueueError::Invalid(RestoreQueueValidationError::TooManyEntries {
                max_entries,
            }) => Self::TooManyEntries { max_entries },
            RestoreQueueError::Invalid(RestoreQueueValidationError::CurrentIndexMissing {
                ..
            }) => Self::MissingCurrentIndex,
            RestoreQueueError::Invalid(RestoreQueueValidationError::CurrentIndexOutOfRange {
                ..
            }) => Self::InvalidCurrentIndex,
            RestoreQueueError::Invalid(RestoreQueueValidationError::UnknownTrack { .. }) => {
                Self::UnknownTrack
            }
            RestoreQueueError::Disabled => Self::Disabled,
            error => Self::Queue(error),
        }
    }
}

pub(crate) struct PlayQueueView {
    pub(crate) entries: Vec<models::Child>,
    pub(crate) current_index: Option<u32>,
    pub(crate) position_ms: u64,
    pub(crate) username: String,
    pub(crate) changed_at: chrono::DateTime<chrono::Utc>,
}

pub(crate) enum PlayQueueSave {
    Replace {
        entries: Vec<TrackId>,
        current_index: Option<u32>,
        position_ms: u64,
    },
    Clear,
}

pub(crate) async fn read_play_queue(
    queue: &RestoreQueueService,
    library: &LibraryService,
    user: Uuid,
    tags: SourceTags,
) -> Result<Option<PlayQueueView>, PlayQueueError> {
    let Some(snapshot) = queue.read(user).await.map_err(PlayQueueError::from_queue)? else {
        return Ok(None);
    };
    if snapshot.is_empty() {
        return Ok(None);
    }

    let track_ids = snapshot
        .entries()
        .iter()
        .map(|entry| entry.track_id().clone())
        .collect::<Vec<_>>();
    let views = library
        .track_views(&track_ids, Some(&user))
        .await
        .map_err(PlayQueueError::Catalogue)?;
    let views = views
        .into_iter()
        .map(|view| (view.track().id().clone(), view))
        .collect::<HashMap<_, _>>();
    let mut entries = Vec::with_capacity(snapshot.entries().len());
    for entry in snapshot.entries() {
        let view = views
            .get(entry.track_id())
            .ok_or(PlayQueueError::UnresolvedEntry)?;
        entries.push(mapping::child_from_view(view, tags));
    }

    Ok(Some(PlayQueueView {
        entries,
        current_index: snapshot.current_index(),
        position_ms: snapshot.position_ms(),
        username: snapshot.changed_by_username().to_owned(),
        changed_at: snapshot.changed_at(),
    }))
}

pub(crate) async fn save_play_queue(
    queue: &RestoreQueueService,
    user: Uuid,
    save: PlayQueueSave,
) -> Result<(), PlayQueueError> {
    match save {
        PlayQueueSave::Clear => queue
            .clear(user)
            .await
            .map(|_| ())
            .map_err(PlayQueueError::from_queue),
        PlayQueueSave::Replace {
            entries,
            current_index,
            position_ms,
        } => queue
            .save(
                user,
                RestoreQueueSave {
                    entries,
                    current_index,
                    position_ms,
                },
            )
            .await
            .map(|_| ())
            .map_err(PlayQueueError::from_queue),
    }
}

pub(crate) async fn queue_owner_name(
    credential: &CredentialService,
    principal: &Principal,
) -> Result<Option<String>, PlayQueueError> {
    let users = credential
        .directory(principal)
        .await
        .map_err(|error| PlayQueueError::Directory(Box::new(error)))?;

    Ok(users
        .into_iter()
        .find(|user| user.id == principal.user_id)
        .map(|user| user.username))
}

fn queue_entries(query: &RawQuery) -> Result<Vec<TrackId>, PlayQueueError> {
    query
        .all("id")
        .into_iter()
        .map(|raw| raw.parse().map_err(|_| PlayQueueError::InvalidTrackId))
        .collect()
}

fn queue_position(query: &RawQuery) -> Result<u64, PlayQueueError> {
    match query.get("position") {
        None => Ok(0),
        Some("") => Ok(0),
        Some(raw) => {
            let position_ms = raw
                .trim()
                .parse::<u64>()
                .map_err(|_| PlayQueueError::InvalidPosition)?;

            if !representable(position_ms) {
                return Err(PlayQueueError::InvalidPosition);
            }

            Ok(position_ms)
        }
    }
}

fn representable(position_ms: u64) -> bool {
    i32::try_from(position_ms).is_ok() && i64::try_from(position_ms).is_ok()
}

pub(crate) fn play_queue_save(query: &RawQuery) -> Result<PlayQueueSave, PlayQueueError> {
    let entries = queue_entries(query)?;
    let position_ms = queue_position(query)?;
    let Some(raw_current) = query.get("current") else {
        return if entries.is_empty() {
            Ok(PlayQueueSave::Clear)
        } else {
            Err(PlayQueueError::MissingCurrentIndex)
        };
    };
    if entries.is_empty() {
        return Err(PlayQueueError::CurrentWithoutQueue);
    }

    let current: TrackId = raw_current
        .parse()
        .map_err(|_| PlayQueueError::InvalidTrackId)?;
    let current_index = entries
        .iter()
        .position(|entry| entry == &current)
        .ok_or(PlayQueueError::CurrentNotInQueue)?;

    Ok(PlayQueueSave::Replace {
        entries,
        current_index: Some(current_index as u32),
        position_ms,
    })
}

pub(crate) fn play_queue_save_by_index(query: &RawQuery) -> Result<PlayQueueSave, PlayQueueError> {
    let entries = queue_entries(query)?;
    let position_ms = queue_position(query)?;
    if entries.is_empty() {
        return match query.get("currentIndex") {
            None => Ok(PlayQueueSave::Clear),
            Some(_) => Err(PlayQueueError::CurrentWithoutQueue),
        };
    }

    let Some(raw_index) = query.get("currentIndex") else {
        return Err(PlayQueueError::MissingCurrentIndex);
    };
    let current_index = raw_index
        .trim()
        .parse::<u32>()
        .map_err(|_| PlayQueueError::InvalidCurrentIndex)?;

    Ok(PlayQueueSave::Replace {
        entries,
        current_index: Some(current_index),
        position_ms,
    })
}

#[cfg(all(test, feature = "postgres-integration"))]
#[path = "service_tests.rs"]
mod tests;
