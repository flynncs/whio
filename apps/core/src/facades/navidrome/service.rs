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
        AlbumEntry, AlbumId, AlbumListFilter, AlbumOrder, AlbumSummary, ArtistId, ArtistListFilter,
        ArtistSort, ArtistSummary, GenreSummary, IdSet, MAX_PAGE_LIMIT, PageLimit, PlaylistId,
        SelectorText, SortDirection, TrackId, TrackListFilter, TrackOrder, TrackView,
    },
    features::music_library::library::{LibraryId, LibraryRegistrationError, LibraryRegistry},
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

    #[error("library registry read failed")]
    Registry(#[source] LibraryRegistrationError),
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
        duration: track.duration_ms().map(|ms| ms as f32 / 1000.0),
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
        duration: track.duration_ms().map(|ms| ms as f32 / 1000.0),
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
        duration: track.duration_ms().map(|ms| ms as f32 / 1000.0),
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
    let state = summary.state();

    Album {
        id: album.id().to_string(),
        name: album.title().to_owned(),
        album_artist: artist.map(|artist| artist.name().to_owned()),
        album_artist_id: artist.map(|artist| artist.id().to_string()),
        song_count: summary.track_count(),
        duration: summary.duration_ms() as f32 / 1000.0,
        cover_art: Some(CoverArtTarget::album(album.id()).encode()),
        year: album.year(),
        created_at: summary.created_at().to_rfc3339(),
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
        song_count: summary.song_count(),
        cover_art: Some(CoverArtTarget::artist(artist.id()).encode()),
        sort_artist_name: artist.sort_name().map(str::to_owned),
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
    let title = SelectorText::new(query.title.as_deref().unwrap_or_default())
        .map_err(|_| SongError::InvalidSearch)?;
    let filter = TrackListFilter::new().with_title(title);
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

pub(crate) async fn album_list(
    library: &LibraryService,
    credential: &CredentialService,
    token: &str,
    order: AlbumOrder,
    filter: AlbumListFilter,
    start: u32,
    end: u32,
) -> Result<(Vec<Album>, u32), SongError> {
    let user = authorize(credential, token).await?;
    let (offset, limit) = page(start, end)?;
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
    sort: ArtistSort,
    filter: ArtistListFilter,
    start: u32,
    end: u32,
) -> Result<(Vec<Artist>, u32), SongError> {
    let user = authorize(credential, token).await?;
    let (offset, limit) = page(start, end)?;
    let total = library
        .artist_count(Some(&user), &filter)
        .await
        .map_err(SongError::Catalogue)?;
    let artists = library
        .artist_list(offset, limit, sort, &filter, Some(&user))
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

pub(crate) fn album_order(sort: Option<&str>) -> Result<AlbumOrder, SongError> {
    match sort {
        None => Ok(AlbumOrder::default()),
        Some("name") => Ok(AlbumOrder::Name),
        Some("album_artist" | "albumartist") => Ok(AlbumOrder::AlbumArtist),
        Some("artist") => Ok(AlbumOrder::Artist),
        Some("duration") => Ok(AlbumOrder::Duration),
        Some("songCount") => Ok(AlbumOrder::SongCount),
        Some("max_year") => Ok(AlbumOrder::MaxYear),
        Some("recently_added" | "recentlyadded" | "createdAt") => Ok(AlbumOrder::DateAdded),
        Some("starred_at") => Ok(AlbumOrder::StarredAt),
        Some("rating") => Ok(AlbumOrder::Rating),
        Some("play_count") => Ok(AlbumOrder::PlayCount),
        Some("play_date") => Ok(AlbumOrder::PlayDate),
        Some("random") => Ok(AlbumOrder::Random),
        Some(_) => Err(SongError::InvalidSearch),
    }
}

pub(crate) fn artist_sort(sort: Option<&str>) -> Result<ArtistSort, SongError> {
    match sort {
        None => Ok(ArtistSort::default()),
        Some("name") => Ok(ArtistSort::Name),
        Some("albumCount") => Ok(ArtistSort::AlbumCount),
        Some("songCount") => Ok(ArtistSort::SongCount),
        Some("starred_at") => Ok(ArtistSort::StarredAt),
        Some("rating") => Ok(ArtistSort::Rating),
        Some("playCount") => Ok(ArtistSort::PlayCount),
        Some(_) => Err(SongError::InvalidSearch),
    }
}

pub(crate) fn sort_direction(order: Option<&str>) -> Result<Option<SortDirection>, SongError> {
    match order {
        None => Ok(None),
        Some(raw) => match raw.to_ascii_lowercase().as_str() {
            "asc" => Ok(Some(SortDirection::Ascending)),
            "desc" => Ok(Some(SortDirection::Descending)),
            _ => Err(SongError::InvalidSearch),
        },
    }
}

pub(crate) async fn canonical_libraries(
    libraries: &LibraryRegistry,
    requested: &[i32],
) -> Result<IdSet<LibraryId>, SongError> {
    match libraries.ids_for_numeric(requested).await {
        Ok(Some(ids)) => Ok(ids),
        Ok(None) | Err(LibraryRegistrationError::Disabled) => Err(SongError::InvalidSearch),
        Err(error) => Err(SongError::Registry(error)),
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

fn page(start: u32, end: u32) -> Result<(u32, u32), SongError> {
    if end > start {
        let limit = end - start;

        if limit > MAX_PAGE_LIMIT {
            return Err(SongError::InvalidSearch);
        }

        Ok((start, limit))
    } else {
        Ok((start, DEFAULT_PAGE_SIZE))
    }
}

#[cfg(all(test, feature = "postgres-integration"))]
#[path = "service_tests.rs"]
mod tests;
