use thiserror::Error;
use uuid::Uuid;

use crate::{
    facades::SourceTags,
    features::identity::service::{AuthError, CredentialService},
    features::music_library::artwork::CoverArtTarget,
    features::music_library::catalogue::{
        CatalogueError, CompletionService, LibraryService, SearchHit,
    },
    features::music_library::domain::{
        AlbumEntry, AlbumId, AlbumListFilter, AlbumOrder, AlbumSummary, ArtistId, ArtistSummary,
        GenreSummary, MAX_PAGE_LIMIT, PageLimit, PlaylistId, TrackId, TrackListFilter, TrackOrder,
        TrackView,
    },
    features::music_library::playlists::{PlaylistDetail, PlaylistError, PlaylistService},
};

use super::{
    dto::{Album, Artist, Genre, Playlist, Song, SongListQuery},
    session,
};

#[derive(Debug, Error)]
pub(crate) enum SongError {
    #[error("not authenticated")]
    Unauthorized,

    #[error("authentication storage failed")]
    Authentication(#[source] AuthError),

    #[error("track not found")]
    NotFound,

    #[error("invalid search parameters")]
    InvalidSearch,

    #[error("catalogue search failed")]
    Catalogue(#[source] CatalogueError),

    #[error("playlist request failed")]
    Playlist(#[source] PlaylistError),
}

fn song_from(hit: &SearchHit, tags: SourceTags) -> Song {
    let track = hit.track();
    let album = hit.album();
    let state = hit.state();

    Song {
        id: track.id().to_string(),
        title: track.title().to_owned(),
        artist: tags.artist(track.artists(), hit.sources()),
        album: album.map(|album| album.title().to_owned()),
        album_id: album.map(|album| album.id().to_string()),
        album_artist: None,
        track_number: None,
        disc_number: None,
        cover_art: Some(CoverArtTarget::song(track.id()).encode()),
        duration: track
            .duration_ms()
            .map(|ms| ms as f32 / 1000.0)
            .unwrap_or(0.0),
        starred: state.is_starred(),
        starred_at: state.starred_at().map(|at| at.to_rfc3339()),
        rating: state.rating().unwrap_or(0),
        play_count: u64::from(state.play_count()),
        play_date: state.last_played_at().map(|at| at.to_rfc3339()),
    }
}

fn song_from_view(view: &TrackView, tags: SourceTags) -> Song {
    let track = view.track();
    let album = view.album();
    let membership = view.membership();
    let state = view.state();

    Song {
        id: track.id().to_string(),
        title: track.title().to_owned(),
        artist: tags.artist(track.artists(), view.sources()),
        album: album.map(|album| album.title().to_owned()),
        album_id: album.map(|album| album.id().to_string()),
        album_artist: view.album_artist().map(str::to_owned),
        track_number: membership
            .and_then(|membership| membership.track_number())
            .map(i32::from),
        disc_number: membership
            .and_then(|membership| membership.disc_number())
            .map(i32::from),
        cover_art: Some(CoverArtTarget::song(track.id()).encode()),
        duration: track
            .duration_ms()
            .map(|ms| ms as f32 / 1000.0)
            .unwrap_or(0.0),
        starred: state.is_starred(),
        starred_at: state.starred_at().map(|at| at.to_rfc3339()),
        rating: state.rating().unwrap_or(0),
        play_count: u64::from(state.play_count()),
        play_date: state.last_played_at().map(|at| at.to_rfc3339()),
    }
}

fn song_from_entry(entry: &AlbumEntry, album: &AlbumSummary, tags: SourceTags) -> Song {
    let track = entry.track();
    let membership = entry.membership();
    let state = entry.state();

    Song {
        id: track.id().to_string(),
        title: track.title().to_owned(),
        artist: tags.artist(track.artists(), entry.sources()),
        album: Some(album.album().title().to_owned()),
        album_id: Some(album.album().id().to_string()),
        album_artist: album.album_artist().map(|artist| artist.name().to_owned()),
        track_number: membership.track_number().map(i32::from),
        disc_number: membership.disc_number().map(i32::from),
        cover_art: Some(track.id().to_string()),
        duration: track
            .duration_ms()
            .map(|ms| ms as f32 / 1000.0)
            .unwrap_or(0.0),
        starred: state.is_starred(),
        starred_at: state.starred_at().map(|at| at.to_rfc3339()),
        rating: state.rating().unwrap_or(0),
        play_count: u64::from(state.play_count()),
        play_date: state.last_played_at().map(|at| at.to_rfc3339()),
    }
}

fn album_from_summary(summary: &AlbumSummary) -> Album {
    let album = summary.album();
    let artist = summary.album_artist();
    let created_at = summary.created_at().to_rfc3339();
    let state = summary.state();

    Album {
        id: album.id().to_string(),
        name: album.title().to_owned(),
        album_artist: artist.map_or_else(String::new, |artist| artist.name().to_owned()),
        album_artist_id: artist.map_or_else(String::new, |artist| artist.id().to_string()),
        song_count: summary.track_count(),
        duration: summary.duration_ms() as f32 / 1000.0,
        size: 0,
        cover_art: Some(CoverArtTarget::album(album.id()).encode()),
        year: album.year(),
        min_year: album.year().unwrap_or(0),
        max_year: album.year().unwrap_or(0),
        compilation: false,
        created_at: created_at.clone(),
        updated_at: created_at,
        genres: None,
        library_id: 1,
        library_name: "whio".to_owned(),
        library_path: String::new(),
        order_album_name: album.title().to_lowercase(),
        order_album_artist_name: artist
            .map_or_else(String::new, |artist| artist.name().to_lowercase()),
        external_info_updated_at: None,
        starred: state.is_starred(),
        starred_at: state.starred_at().map(|at| at.to_rfc3339()),
        rating: state.rating().unwrap_or(0),
        play_count: u64::from(state.play_count()),
        play_date: state.last_played_at().map(|at| at.to_rfc3339()),
    }
}

fn artist_from_summary(summary: &ArtistSummary) -> Artist {
    let artist = summary.artist();
    let state = summary.state();

    Artist {
        id: artist.id().to_string(),
        name: artist.name().to_owned(),
        album_count: summary.album_count(),
        song_count: 0,
        size: 0,
        missing: false,
        cover_art: Some(CoverArtTarget::artist(artist.id()).encode()),
        sort_artist_name: artist
            .sort_name()
            .map_or_else(|| artist.name().to_lowercase(), str::to_owned),
        order_artist_name: artist.name().to_lowercase(),
        mbz_artist_id: artist.musicbrainz_artist_id().map(str::to_owned),
        starred: state.is_starred(),
        starred_at: state.starred_at().map(|at| at.to_rfc3339()),
        rating: state.rating().unwrap_or(0),
        play_count: u64::from(state.play_count()),
        play_date: state.last_played_at().map(|at| at.to_rfc3339()),
    }
}

fn genre_from_summary(summary: &GenreSummary) -> Genre {
    Genre {
        id: summary.genre().to_owned(),
        name: summary.genre().to_owned(),
        song_count: u64::from(summary.song_count()),
        album_count: u64::from(summary.album_count()),
    }
}

pub(crate) async fn song_list(
    library: &LibraryService,
    credential: &CredentialService,
    token: &str,
    query: &SongListQuery,
    tags: SourceTags,
) -> Result<(Vec<Song>, u32), SongError> {
    let user = authorize(credential, token).await?;
    let (offset, limit) = song_window(query)?;
    let filter = TrackListFilter::new().with_title(query.title.clone());
    let page = library
        .track_page(offset, limit, TrackOrder::Title, &filter, Some(&user))
        .await
        .map_err(SongError::Catalogue)?;
    let (views, total) = page.into_parts();

    Ok((
        views
            .iter()
            .map(|view| song_from_view(view, tags))
            .collect(),
        total,
    ))
}

fn song_window(query: &SongListQuery) -> Result<(u32, PageLimit), SongError> {
    let start = u64::from(query.start.unwrap_or(0));
    let end = match query.end {
        Some(end) if end >= 0 => end as u64,
        _ => start.saturating_add(24),
    };
    if end < start {
        return Err(SongError::InvalidSearch);
    }

    let window = (end - start)
        .saturating_add(1)
        .min(u64::from(MAX_PAGE_LIMIT));
    let limit = u32::try_from(window).map_err(|_| SongError::InvalidSearch)?;
    let limit = PageLimit::new(limit).map_err(|_| SongError::InvalidSearch)?;

    Ok((query.start.unwrap_or(0), limit))
}

pub(crate) async fn song_detail(
    library: &LibraryService,
    credential: &CredentialService,
    raw_id: &str,
    token: &str,
    tags: SourceTags,
) -> Result<Song, SongError> {
    let user = authorize(credential, token).await?;

    let track_id: TrackId = raw_id.parse().map_err(|_| SongError::NotFound)?;
    let hit = library
        .track_detail(&track_id, Some(&user))
        .await
        .map_err(SongError::Catalogue)?
        .ok_or(SongError::NotFound)?;

    Ok(song_from(&hit, tags))
}

const DEFAULT_PAGE_SIZE: u32 = 500;

pub(crate) async fn album_songs(
    completion: &CompletionService,
    credential: &CredentialService,
    token: &str,
    raw_album_id: &str,
    tags: SourceTags,
) -> Result<Vec<Song>, SongError> {
    let user = authorize(credential, token).await?;
    let album_id: AlbumId = raw_album_id.parse().map_err(|_| SongError::NotFound)?;
    let detail = completion
        .album_detail(&album_id, Some(&user))
        .await
        .map_err(SongError::Catalogue)?
        .ok_or(SongError::NotFound)?;

    Ok(detail
        .entries
        .iter()
        .map(|entry| song_from_entry(entry, &detail.album, tags))
        .collect())
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn album_list(
    library: &LibraryService,
    credential: &CredentialService,
    token: &str,
    order: AlbumOrder,
    filter: AlbumListFilter,
    artist_id: Option<&str>,
    start: u32,
    end: u32,
) -> Result<(Vec<Album>, u32), SongError> {
    let user = authorize(credential, token).await?;
    let (offset, limit) = page(start, end);

    match artist_id {
        Some(raw) => {
            let artist_id: ArtistId = raw.parse().map_err(|_| SongError::InvalidSearch)?;
            let detail = library
                .artist_detail(&artist_id, Some(&user))
                .await
                .map_err(SongError::Catalogue)?
                .ok_or(SongError::NotFound)?;
            let total = detail.albums.len() as u32;
            let albums = detail
                .albums
                .iter()
                .skip(offset as usize)
                .take(limit as usize)
                .map(album_from_summary)
                .collect();

            Ok((albums, total))
        }
        None => {
            let total = library
                .album_count(Some(&user), filter.clone())
                .await
                .map_err(SongError::Catalogue)?;
            let albums = library
                .album_list(offset, limit, order, filter, Some(&user))
                .await
                .map_err(SongError::Catalogue)?;

            Ok((albums.iter().map(album_from_summary).collect(), total))
        }
    }
}

pub(crate) async fn album_detail(
    completion: &CompletionService,
    credential: &CredentialService,
    token: &str,
    raw_id: &str,
) -> Result<Album, SongError> {
    let user = authorize(credential, token).await?;
    let album_id: AlbumId = raw_id.parse().map_err(|_| SongError::NotFound)?;
    let detail = completion
        .album_detail(&album_id, Some(&user))
        .await
        .map_err(SongError::Catalogue)?
        .ok_or(SongError::NotFound)?;

    Ok(album_from_summary(&detail.album))
}

fn playlist_from(detail: &PlaylistDetail, owner: Uuid, tags: SourceTags) -> Playlist {
    let playlist = &detail.playlist;

    Playlist {
        id: playlist.id().to_string(),
        name: playlist.name().to_owned(),
        comment: playlist.comment().map(str::to_owned),
        owner: owner.to_string(),
        public: playlist.is_public(),
        song_count: playlist.track_count(),
        duration: playlist.duration_ms() as f32 / 1000.0,
        created_at: playlist.created_at().to_rfc3339(),
        updated_at: playlist.updated_at().to_rfc3339(),
        songs: detail
            .entries
            .iter()
            .map(|hit| song_from(hit, tags))
            .collect(),
    }
}

pub(crate) async fn playlist_list(
    playlists: &PlaylistService,
    credential: &CredentialService,
    token: &str,
    tags: SourceTags,
) -> Result<Vec<Playlist>, SongError> {
    let user = authorize(credential, token).await?;
    let summaries = playlists.list(user).await.map_err(SongError::Playlist)?;
    let mut result = Vec::with_capacity(summaries.len());

    for summary in summaries {
        if let Some(detail) = playlists
            .detail(user, summary.id())
            .await
            .map_err(SongError::Playlist)?
        {
            result.push(playlist_from(&detail, user, tags));
        }
    }

    Ok(result)
}

pub(crate) async fn playlist_detail(
    playlists: &PlaylistService,
    credential: &CredentialService,
    token: &str,
    raw_id: &str,
    tags: SourceTags,
) -> Result<Playlist, SongError> {
    let user = authorize(credential, token).await?;
    let playlist_id: PlaylistId = raw_id.parse().map_err(|_| SongError::NotFound)?;
    let detail = playlists
        .detail(user, &playlist_id)
        .await
        .map_err(SongError::Playlist)?
        .ok_or(SongError::NotFound)?;

    Ok(playlist_from(&detail, user, tags))
}

pub(crate) async fn artist_list(
    library: &LibraryService,
    credential: &CredentialService,
    token: &str,
    start: u32,
    end: u32,
) -> Result<(Vec<Artist>, u32), SongError> {
    let user = authorize(credential, token).await?;
    let (offset, limit) = page(start, end);
    let total = library
        .artist_count(Some(&user))
        .await
        .map_err(SongError::Catalogue)?;
    let artists = library
        .artist_list(offset, limit, Some(&user))
        .await
        .map_err(SongError::Catalogue)?;

    Ok((artists.iter().map(artist_from_summary).collect(), total))
}

pub(crate) async fn artist_detail(
    library: &LibraryService,
    credential: &CredentialService,
    token: &str,
    raw_id: &str,
) -> Result<Artist, SongError> {
    let user = authorize(credential, token).await?;
    let artist_id: ArtistId = raw_id.parse().map_err(|_| SongError::NotFound)?;
    let detail = library
        .artist_detail(&artist_id, Some(&user))
        .await
        .map_err(SongError::Catalogue)?
        .ok_or(SongError::NotFound)?;

    Ok(artist_from_summary(&detail.artist))
}

pub(crate) fn album_order(sort: Option<&str>) -> AlbumOrder {
    let sort = sort.unwrap_or_default().to_ascii_lowercase();
    let sort = sort.replace('_', "");

    match sort.as_str() {
        "albumartist" | "artist" => AlbumOrder::Artist,
        "recentlyadded" | "createdat" => AlbumOrder::RecentlyAdded,
        "year" => AlbumOrder::YearAscending,
        "playcount" => AlbumOrder::Frequent,
        "playdate" | "lastplayed" => AlbumOrder::Recent,
        "rating" => AlbumOrder::HighestRated,
        "starredat" | "starred" => AlbumOrder::Starred,
        _ => AlbumOrder::Title,
    }
}

pub(crate) async fn genre_list(
    library: &LibraryService,
    credential: &CredentialService,
    token: &str,
) -> Result<Vec<Genre>, SongError> {
    authorize(credential, token).await?;

    library
        .genres()
        .await
        .map(|genres| genres.iter().map(genre_from_summary).collect())
        .map_err(SongError::Catalogue)
}

async fn authorize(credential: &CredentialService, token: &str) -> Result<Uuid, SongError> {
    match session::authorize(credential, token).await {
        Ok(Some(user_id)) => Ok(user_id.as_uuid()),
        Ok(None) => Err(SongError::Unauthorized),
        Err(error) => Err(SongError::Authentication(error)),
    }
}

fn page(start: u32, end: u32) -> (u32, u32) {
    if end > start {
        (start, end - start)
    } else {
        (start, DEFAULT_PAGE_SIZE)
    }
}

#[cfg(all(test, feature = "postgres-integration"))]
#[path = "service_tests.rs"]
mod tests;
