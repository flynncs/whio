use thiserror::Error;
use whio_subsonic_api::models;

use uuid::Uuid;

use crate::{
    facades::SourceTags,
    features::music_library::catalogue::{
        CatalogueError, CompletionService, LibraryService, MAX_PAGE_LIMIT, PageLimit, SearchQuery,
        SearchRequest, SearchService, SearchWindows, TrackListFilter, TrackOrder,
    },
    features::music_library::domain::{
        AlbumId, AlbumListFilter, AlbumListScope, AlbumOrder, ArtistId, ArtistListFilter,
        ArtistSort, IdSet, RandomSeed, Range, RoleSelector, RoleSet, SelectorText, SortDirection,
        StateTarget, TrackId, ValidationError,
    },
    features::music_library::library::ScanService,
    features::music_library::library_state::{LibraryStateError, LibraryStateService},
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
    context: &RequestContext,
    user: Option<&uuid::Uuid>,
    query: &str,
    windows: &SearchWindows,
    tags: SourceTags,
) -> Result<SearchResult, SearchError> {
    if query.is_empty() {
        return Ok(SearchResult {
            songs: Vec::new(),
            albums: Vec::new(),
            artists: Vec::new(),
        });
    }

    let request = SearchRequest::new(
        SearchQuery::new(query.to_owned()).map_err(|_| SearchError::InvalidSearch)?,
        *windows,
        ArtistListFilter::new().with_role(Some(
            RoleSet::new([RoleSelector::AlbumArtist]).map_err(|_| SearchError::InvalidSearch)?,
        )),
    );

    search_service
        .search(&request, user, context)
        .await
        .map(|outcome| SearchResult {
            songs: outcome
                .tracks
                .iter()
                .map(|hit| mapping::child_from_hit(hit, tags))
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
        .map_err(SearchError::Catalogue)
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
        let filter = album_artist_filter().map_err(|_| SearchError::InvalidSearch)?;

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

pub(crate) async fn genres(library: &LibraryService) -> Result<Vec<models::Genre>, BrowseError> {
    library
        .genres()
        .await
        .map(|genres| genres.iter().map(mapping::genre_from_summary).collect())
        .map_err(BrowseError::Catalogue)
}

pub(crate) async fn starred(
    state: &LibraryStateService,
    user: Uuid,
    tags: SourceTags,
) -> Result<StarredResult, StateError> {
    let starred = state.starred(user).await.map_err(StateError::State)?;

    Ok(StarredResult {
        songs: starred
            .tracks()
            .iter()
            .map(|hit| mapping::child_from_hit(hit, tags))
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

pub(crate) async fn scrobble(
    state: &LibraryStateService,
    user: Uuid,
    ids: &[String],
    times: &[String],
    submission: bool,
) -> Result<(), StateError> {
    if ids.is_empty() {
        return Err(StateError::MissingParameter);
    }
    if !times.is_empty() && times.len() != ids.len() {
        return Err(StateError::InvalidRequest);
    }
    if !submission {
        return Ok(());
    }

    let now = chrono::Utc::now();
    for (index, raw) in ids.iter().enumerate() {
        let Ok(track_id) = raw.parse::<TrackId>() else {
            continue;
        };
        let played_at = times
            .get(index)
            .and_then(|raw| raw.trim().parse::<i64>().ok())
            .and_then(chrono::DateTime::from_timestamp_millis)
            .unwrap_or(now);

        state
            .record_play(user, &track_id, played_at)
            .await
            .map_err(StateError::State)?;
    }

    Ok(())
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

fn album_artist_filter() -> Result<ArtistListFilter, ValidationError> {
    let role = RoleSet::new([RoleSelector::AlbumArtist])?;

    Ok(ArtistListFilter::new().with_role(Some(role)))
}

pub(crate) async fn artists(
    library: &LibraryService,
    user: Option<&Uuid>,
) -> Result<Vec<models::ArtistId3>, BrowseError> {
    let filter = album_artist_filter().map_err(|_| BrowseError::InvalidRequest)?;

    library
        .artist_list(0, u32::MAX, ArtistSort::Name, &filter, user)
        .await
        .map(|artists| artists.iter().map(mapping::artist_from_summary).collect())
        .map_err(BrowseError::Catalogue)
}

pub(crate) async fn artist_detail(
    library: &LibraryService,
    raw_id: &str,
    user: Option<&Uuid>,
) -> Result<models::ArtistWithAlbumsId3, BrowseError> {
    let artist_id: ArtistId = raw_id.parse().map_err(|_| BrowseError::ArtistNotFound)?;
    let detail = library
        .artist_detail(&artist_id, user)
        .await
        .map_err(BrowseError::Catalogue)?
        .ok_or(BrowseError::ArtistNotFound)?;

    Ok(mapping::artist_with_albums(&detail))
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
}

pub(crate) struct RandomSongsRequest<'a> {
    pub(crate) size: Option<&'a str>,
    pub(crate) genre: Option<&'a str>,
    pub(crate) from_year: Option<&'a str>,
    pub(crate) to_year: Option<&'a str>,
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
    let filter = TrackListFilter::new().with_genre(Some(set));

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
    let artists = library
        .artist_list(
            0,
            u32::MAX,
            ArtistSort::Name,
            &ArtistListFilter::default(),
            user,
        )
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

#[cfg(all(test, feature = "postgres-integration"))]
#[path = "service_tests.rs"]
mod tests;
