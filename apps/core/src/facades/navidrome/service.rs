use std::collections::HashMap;

use thiserror::Error;
use uuid::Uuid;

use crate::{
    facades::SourceTags,
    features::identity::service::{AuthError, CredentialService},
    features::music_library::artwork::CoverArtTarget,
    features::music_library::catalogue::{CatalogueError, CompletionService, LibraryService},
    features::music_library::domain::{
        AlbumId, AlbumListFilter, AlbumOrder, AlbumSummary, ArtistId, ArtistListFilter, ArtistSort,
        ArtistSummary, CommentPatch, GenreSummary, IdSet, MAX_PAGE_LIMIT, MoveTarget,
        OccurrenceSelector, PageLimit, Playlist as PlaylistRecord, PlaylistEntryChange, PlaylistId,
        PlaylistMetadataPatch, PlaylistOrder, RestoreQueueSnapshot, SortDirection, TrackId,
        TrackListFilter, TrackOrder, TrackView,
    },
    features::music_library::library::{LibraryId, LibraryRegistrationError, LibraryRegistry},
    features::music_library::playlists::{
        PlaylistDetail, PlaylistEntry, PlaylistError, PlaylistService,
    },
    features::music_library::queue::{RestoreQueueError, RestoreQueueSave, RestoreQueueService},
};

use super::{
    dto::{Album, Artist, Genre, Playlist, PlaylistSong, Queue, QueueSaveBody, Song},
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

    #[error("forbidden")]
    Forbidden,

    #[error("invalid request")]
    InvalidRequest,

    #[error("library registry read failed")]
    Registry(#[source] LibraryRegistrationError),

    #[error("restore queue request failed")]
    Queue(#[source] RestoreQueueError),
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
        suffix: view.representation().and_then(|facts| facts.suffix.clone()),
        codec: view.representation().and_then(|facts| facts.codec.clone()),
        size: view.representation().and_then(|facts| facts.size_bytes),
        bit_rate: view.representation().and_then(|facts| facts.bitrate_kbps),
        sample_rate: view.representation().and_then(|facts| facts.sample_rate_hz),
        bit_depth: view.representation().and_then(|facts| facts.bit_depth),
        channels: view.representation().and_then(|facts| facts.channels),
        starred: state.is_starred(),
        starred_at: state.starred_at().map(|at| at.to_rfc3339()),
        rating: state.rating().unwrap_or(0),
        play_count: u64::from(state.play_count()),
        play_date: state.last_played_at().map(|at| at.to_rfc3339()),
    }
}

fn song_from_playlist_entry(entry: &PlaylistEntry, item_id: u32, tags: SourceTags) -> Song {
    let mut song = song_from_view(entry.track(), tags);
    song.id = item_id.to_string();

    song
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

#[derive(Clone)]
pub(crate) struct SongListRequest {
    pub(crate) filter: TrackListFilter,
    pub(crate) order: TrackOrder,
    pub(crate) offset: u32,
    pub(crate) limit: PageLimit,
}

pub(crate) async fn song_list(
    library: &LibraryService,
    completion: &CompletionService,
    credential: &CredentialService,
    token: &str,
    request: SongListRequest,
    tags: SourceTags,
) -> Result<(Vec<Song>, u32), SongError> {
    let user = authorize(credential, token).await?;
    let album_id = request
        .filter
        .album_id()
        .filter(|ids| ids.len() == 1)
        .and_then(|ids| ids.iter().next());
    if let Some(album_id) = album_id {
        completion
            .album_detail(album_id, Some(&user))
            .await
            .map_err(SongError::Catalogue)?
            .ok_or(SongError::NotFound)?;
    }

    let page = match album_id {
        Some(album_id) => {
            library
                .album_track_page(
                    album_id,
                    request.offset,
                    request.limit,
                    request.order,
                    &request.filter,
                    Some(&user),
                )
                .await
        }
        None => {
            library
                .track_page(
                    request.offset,
                    request.limit,
                    request.order,
                    &request.filter,
                    Some(&user),
                )
                .await
        }
    }
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

pub(crate) async fn song_detail(
    library: &LibraryService,
    credential: &CredentialService,
    raw_id: &str,
    token: &str,
    tags: SourceTags,
) -> Result<Song, SongError> {
    let user = authorize(credential, token).await?;

    let track_id: TrackId = raw_id.parse().map_err(|_| SongError::NotFound)?;
    let mut views = library
        .track_views(&[track_id], Some(&user))
        .await
        .map_err(SongError::Catalogue)?;
    let view = views.pop().ok_or(SongError::NotFound)?;

    Ok(song_from_view(&view, tags))
}

const DEFAULT_PAGE_SIZE: u32 = 500;

pub(crate) async fn album_list(
    completion: &CompletionService,
    credential: &CredentialService,
    token: &str,
    order: AlbumOrder,
    filter: AlbumListFilter,
    start: u32,
    end: u32,
) -> Result<(Vec<Album>, u32), SongError> {
    let user = authorize(credential, token).await?;
    let (offset, limit) = page(start, end)?;
    let limit = PageLimit::new(limit).map_err(|_| SongError::InvalidSearch)?;
    let page = completion
        .album_page(offset, limit, order, &filter, Some(&user))
        .await
        .map_err(SongError::Catalogue)?;

    Ok((
        page.albums.iter().map(album_from_summary).collect(),
        page.total,
    ))
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

pub(crate) fn item_id(position: u32) -> u32 {
    position.saturating_add(1)
}

pub(crate) fn item_index(item_id: u32) -> Option<u32> {
    item_id.checked_sub(1)
}

pub(crate) fn move_target(from: u32, insert_before: i64) -> MoveTarget {
    let insert_before = if insert_before <= 0 {
        0
    } else {
        u32::try_from(insert_before).unwrap_or(u32::MAX)
    };
    let destination = insert_before.saturating_sub(1);

    if destination > from {
        MoveTarget::Before(destination.saturating_add(1))
    } else {
        MoveTarget::Before(destination)
    }
}

pub(crate) fn entry_additions(track_ids: &[TrackId]) -> PlaylistEntryChange {
    if track_ids.is_empty() {
        PlaylistEntryChange::Unchanged
    } else {
        PlaylistEntryChange::Apply {
            removals: Vec::new(),
            additions: track_ids.to_vec(),
        }
    }
}

pub(crate) struct PlaylistCreate {
    pub(crate) playlist_id: Option<PlaylistId>,
    pub(crate) owner_id: Option<Uuid>,
    pub(crate) name: Option<String>,
    pub(crate) comment: Option<String>,
    pub(crate) public: Option<bool>,
    pub(crate) additions: Vec<TrackId>,
}

fn playlist_song(playlist: &PlaylistId, entry: &PlaylistEntry, tags: SourceTags) -> PlaylistSong {
    PlaylistSong {
        song: song_from_playlist_entry(entry, item_id(entry.position()), tags),
        media_file_id: entry.track().track().id().to_string(),
        playlist_id: playlist.to_string(),
    }
}

fn playlist_from(playlist: &PlaylistRecord) -> Playlist {
    Playlist {
        id: playlist.id().to_string(),
        name: playlist.name().to_owned(),
        comment: playlist.comment().map(str::to_owned),
        owner_id: playlist.owner_id().to_string(),
        owner_name: playlist.owner_username().to_owned(),
        public: playlist.is_public(),
        song_count: playlist.track_count(),
        duration: playlist.duration_ms() as f32 / 1000.0,
        created_at: playlist.created_at().to_rfc3339(),
        updated_at: playlist.updated_at().to_rfc3339(),
        songs: None,
    }
}

fn playlist_with_songs(detail: &PlaylistDetail, tags: SourceTags) -> Playlist {
    let mut playlist = playlist_from(&detail.playlist);
    playlist.songs = Some(
        detail
            .entries
            .iter()
            .map(|entry| playlist_song(detail.playlist.id(), entry, tags))
            .collect(),
    );

    playlist
}

fn playlist_id(raw_id: &str) -> Result<PlaylistId, SongError> {
    raw_id.parse().map_err(|_| SongError::NotFound)
}

async fn visible_detail(
    playlists: &PlaylistService,
    user: Uuid,
    raw_id: &str,
) -> Result<PlaylistDetail, SongError> {
    let playlist_id = playlist_id(raw_id)?;

    playlists
        .detail(user, &playlist_id)
        .await
        .map_err(SongError::Playlist)?
        .ok_or(SongError::NotFound)
}

async fn refuse(playlists: &PlaylistService, user: Uuid, playlist_id: &PlaylistId) -> SongError {
    match playlists.detail(user, playlist_id).await {
        Ok(Some(detail)) if !detail.editable => SongError::Forbidden,
        Ok(Some(_)) | Ok(None) => SongError::NotFound,
        Err(error) => SongError::Playlist(error),
    }
}

fn owner_claim(user: Uuid, owner_id: Option<Uuid>) -> Result<(), SongError> {
    match owner_id {
        None => Ok(()),
        Some(owner) if owner == user => Ok(()),
        Some(_) => Err(SongError::Forbidden),
    }
}

pub(crate) struct PlaylistListQuery {
    pub order: PlaylistOrder,
    pub direction: SortDirection,
    pub start: u32,
    pub end: u32,
}

pub(crate) async fn playlist_list(
    playlists: &PlaylistService,
    credential: &CredentialService,
    token: &str,
    query: PlaylistListQuery,
    tags: SourceTags,
) -> Result<(Vec<Playlist>, u32), SongError> {
    let user = authorize(credential, token).await?;
    let summaries = playlists
        .list_ordered(user, query.order, query.direction)
        .await
        .map_err(SongError::Playlist)?;
    let total = u32::try_from(summaries.len()).unwrap_or(u32::MAX);
    let (offset, limit) = page(query.start, query.end)?;
    let mut result = Vec::new();

    for summary in summaries.iter().skip(offset as usize).take(limit as usize) {
        if let Some(detail) = playlists
            .detail(user, summary.id())
            .await
            .map_err(SongError::Playlist)?
        {
            result.push(playlist_with_songs(&detail, tags));
        }
    }

    Ok((result, total))
}

pub(crate) async fn playlist_detail(
    playlists: &PlaylistService,
    credential: &CredentialService,
    token: &str,
    raw_id: &str,
    tags: SourceTags,
) -> Result<Playlist, SongError> {
    let user = authorize(credential, token).await?;

    Ok(playlist_with_songs(
        &visible_detail(playlists, user, raw_id).await?,
        tags,
    ))
}

pub(crate) async fn playlist_tracks(
    playlists: &PlaylistService,
    credential: &CredentialService,
    token: &str,
    raw_id: &str,
    start: u32,
    end: Option<u32>,
    tags: SourceTags,
) -> Result<(Vec<PlaylistSong>, u32), SongError> {
    let user = authorize(credential, token).await?;
    let detail = visible_detail(playlists, user, raw_id).await?;
    let songs = detail
        .entries
        .iter()
        .map(|entry| playlist_song(detail.playlist.id(), entry, tags))
        .collect::<Vec<_>>();
    let total = u32::try_from(songs.len()).unwrap_or(u32::MAX);
    let page = match end {
        Some(end) => songs
            .into_iter()
            .skip(start as usize)
            .take(end.saturating_sub(start) as usize)
            .collect(),
        None => songs.into_iter().skip(start as usize).collect(),
    };

    Ok((page, total))
}

pub(crate) async fn playlist_create(
    playlists: &PlaylistService,
    credential: &CredentialService,
    token: &str,
    request: PlaylistCreate,
) -> Result<String, SongError> {
    let user = authorize(credential, token).await?;
    owner_claim(user, request.owner_id)?;

    let playlist_id = match request.playlist_id {
        Some(playlist_id) => {
            let entries = PlaylistEntryChange::Replace(request.additions);

            match playlists
                .write_at_revision(
                    user,
                    &playlist_id,
                    None,
                    PlaylistMetadataPatch::default(),
                    entries,
                )
                .await
                .map_err(SongError::Playlist)?
            {
                Some(playlist) => playlist.id().clone(),
                None => return Err(refuse(playlists, user, &playlist_id).await),
            }
        }
        None => {
            let name = request.name.ok_or(SongError::InvalidRequest)?;
            let playlist = playlists
                .create(user, &name, request.comment.as_deref())
                .await
                .map_err(SongError::Playlist)?;
            let metadata = PlaylistMetadataPatch {
                name: None,
                comment: CommentPatch::Keep,
                public: request.public,
            };

            match playlists
                .write_at_revision(
                    user,
                    playlist.id(),
                    None,
                    metadata,
                    entry_additions(&request.additions),
                )
                .await
                .map_err(SongError::Playlist)?
            {
                Some(playlist) => playlist.id().clone(),
                None => return Err(SongError::NotFound),
            }
        }
    };

    Ok(playlist_id.to_string())
}

pub(crate) async fn playlist_edit(
    playlists: &PlaylistService,
    credential: &CredentialService,
    token: &str,
    raw_id: &str,
    owner_id: Option<Uuid>,
    metadata: PlaylistMetadataPatch,
    entries: PlaylistEntryChange,
) -> Result<Playlist, SongError> {
    let user = authorize(credential, token).await?;
    owner_claim(user, owner_id)?;
    let playlist_id = playlist_id(raw_id)?;

    match playlists
        .write_at_revision(user, &playlist_id, None, metadata, entries)
        .await
        .map_err(SongError::Playlist)?
    {
        Some(playlist) => Ok(playlist_from(&playlist)),
        None => Err(refuse(playlists, user, &playlist_id).await),
    }
}

pub(crate) async fn playlist_move(
    playlists: &PlaylistService,
    credential: &CredentialService,
    token: &str,
    raw_id: &str,
    selector: OccurrenceSelector,
    target: MoveTarget,
) -> Result<(), SongError> {
    let user = authorize(credential, token).await?;
    let playlist_id = playlist_id(raw_id)?;

    match playlists
        .move_entry(user, &playlist_id, selector, target, None)
        .await
        .map_err(SongError::Playlist)?
    {
        true => Ok(()),
        false => Err(refuse(playlists, user, &playlist_id).await),
    }
}

pub(crate) async fn playlist_delete(
    playlists: &PlaylistService,
    credential: &CredentialService,
    token: &str,
    raw_id: &str,
) -> Result<(), SongError> {
    let user = authorize(credential, token).await?;
    let playlist_id = playlist_id(raw_id)?;

    match playlists
        .delete(user, &playlist_id)
        .await
        .map_err(SongError::Playlist)?
    {
        true => Ok(()),
        false => Err(refuse(playlists, user, &playlist_id).await),
    }
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
    completion: &CompletionService,
    credential: &CredentialService,
    token: &str,
    raw_id: &str,
) -> Result<Artist, SongError> {
    let user = authorize(credential, token).await?;
    let artist_id: ArtistId = raw_id.parse().map_err(|_| SongError::NotFound)?;
    let detail = completion
        .artist_detail(&artist_id, Some(&user))
        .await
        .map_err(SongError::Catalogue)?
        .ok_or(SongError::NotFound)?;

    Ok(artist_from_summary(&detail.artist))
}

pub(crate) fn song_order(sort: Option<&str>) -> Result<Option<TrackOrder>, SongError> {
    let Some(sort) = sort else {
        return Ok(None);
    };
    let canonical = match sort {
        "order_album_artist_name" => "album_artist",
        "releaseYear" => "release_year",
        "createdAt" => "date_added",
        "playCount" => "play_count",
        "playDate" => "play_date",
        value => value,
    };

    TrackOrder::parse(canonical)
        .map(Some)
        .ok_or(SongError::InvalidSearch)
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

pub(crate) async fn queue_read(
    library: &LibraryService,
    credential: &CredentialService,
    token: &str,
    queue: &RestoreQueueService,
    tags: SourceTags,
) -> Result<Queue, SongError> {
    let user = authorize(credential, token).await?;
    let Some(snapshot) = queue.read(user).await.map_err(SongError::Queue)? else {
        return Ok(Queue::empty());
    };

    if snapshot.is_empty() {
        return Ok(Queue::empty());
    }

    let track_ids: Vec<TrackId> = snapshot
        .entries()
        .iter()
        .map(|entry| entry.track_id().clone())
        .collect();
    let views = library
        .track_views(&track_ids, Some(&user))
        .await
        .map_err(SongError::Catalogue)?;
    let by_track: HashMap<TrackId, TrackView> = views
        .into_iter()
        .map(|view| (view.track().id().clone(), view))
        .collect();
    let items = snapshot
        .entries()
        .iter()
        .map(|entry| {
            by_track
                .get(entry.track_id())
                .map(|view| song_from_view(view, tags))
                .ok_or(SongError::NotFound)
        })
        .collect::<Result<Vec<Song>, SongError>>()?;

    Ok(queue_from(&snapshot, items))
}

pub(crate) async fn queue_save(
    credential: &CredentialService,
    token: &str,
    queue: &RestoreQueueService,
    body: QueueSaveBody,
) -> Result<(), SongError> {
    let user = authorize(credential, token).await?;
    let position_ms = body.position.unwrap_or(0);
    if i64::try_from(position_ms).is_err() {
        return Err(SongError::InvalidRequest);
    }
    let entries = body
        .ids
        .unwrap_or_default()
        .iter()
        .map(|raw| track_id(raw))
        .collect::<Result<Vec<_>, _>>()?;
    let save = RestoreQueueSave {
        entries,
        current_index: body.current,
        position_ms,
    };

    queue.save(user, save).await.map_err(SongError::Queue)?;

    Ok(())
}

pub(crate) fn queue_from(snapshot: &RestoreQueueSnapshot, items: Vec<Song>) -> Queue {
    Queue {
        changed_by: snapshot.changed_by_username().to_owned(),
        current: snapshot.current_index().unwrap_or(0),
        items,
        position: snapshot.position_ms(),
        updated_at: snapshot.changed_at().to_rfc3339(),
    }
}

fn track_id(raw: &str) -> Result<TrackId, SongError> {
    raw.parse().map_err(|_| SongError::NotFound)
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
