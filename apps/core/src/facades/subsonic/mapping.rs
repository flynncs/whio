use whio_subsonic_api::models;

use crate::{
    features::metadata::domain::{Attribution, FieldEntry, FieldId, FieldValue, Presence},
    features::music_library::artwork::CoverArtTarget,
    features::music_library::catalogue::{AlbumDetail, ArtistDetail},
    features::music_library::domain::{
        AlbumEntry, AlbumSummary, AlbumTrack, Artist, ArtistInformation, ArtistSummary,
        GenreSummary, LibraryState, Playlist, TrackView,
    },
    features::music_library::library::Library,
    features::music_library::playlists::{PlaylistDetail, PlaylistEntry},
    features::playback::{ActivitySession, PlaybackState},
};

use super::payload::PayloadContext;
use super::service::PlayQueueView;

const UNKNOWN_CONTENT_TYPE: &str = "application/octet-stream";

pub(crate) fn child_from_playlist_entry(
    entry: &PlaylistEntry,
    tags: PayloadContext,
) -> models::Child {
    child_from_view(entry.track(), tags)
}

pub(crate) fn child_from_view(view: &TrackView, tags: PayloadContext) -> models::Child {
    let track = view.track();
    let representation = view.representation();

    models::Child {
        id: track.id().to_string(),
        is_dir: false,
        title: track.title().to_owned(),
        album: view.album().map(|album| album.title().to_owned()),
        artist: (!track.artists().is_empty())
            .then(|| tags.tags().artist(track.artists(), view.sources())),
        track: view
            .membership()
            .and_then(AlbumTrack::track_number)
            .map(i32::from),
        cover_art: Some(CoverArtTarget::song(track.id()).encode()),
        duration: wire_integer(track.duration_ms().map(|ms| ms / 1000)),
        disc_number: view
            .membership()
            .and_then(AlbumTrack::disc_number)
            .map(i32::from),
        album_id: view.album().map(|album| album.id().to_string()),
        genre: track.metadata().primary_genre().map(str::to_owned),
        user_rating: view.state().rating().map(i32::from),
        play_count: wire_integer(Some(view.state().play_count())).filter(|count| *count > 0),
        starred: view.state().starred_at().map(|at| at.fixed_offset()),
        played: view.state().last_played_at().map(|at| at.fixed_offset()),
        size: wire_integer(representation.and_then(|facts| facts.size_bytes)),
        content_type: Some(
            representation
                .and_then(|facts| facts.content_type.clone())
                .unwrap_or_else(|| UNKNOWN_CONTENT_TYPE.to_owned()),
        ),
        suffix: representation.and_then(|facts| facts.suffix.clone()),
        bit_rate: wire_integer(representation.and_then(|facts| facts.bitrate_kbps)),
        sampling_rate: wire_integer(representation.and_then(|facts| facts.sample_rate_hz)),
        bit_depth: wire_integer(representation.and_then(|facts| facts.bit_depth)),
        channel_count: wire_integer(representation.and_then(|facts| facts.channels)),
        transcoded_suffix: tags.transcoded_suffix().map(str::to_owned),
        transcoded_content_type: tags.transcoded_content_type().map(str::to_owned),
        media_type: Some(models::MediaType::Song),
        r#type: Some(models::GenericMediaType::Music),
        ..Default::default()
    }
}

pub(crate) fn child_from_entry(
    entry: &AlbumEntry,
    album: &AlbumSummary,
    tags: PayloadContext,
) -> models::Child {
    child_from_view(&entry.track_view(album), tags)
}

fn wire_integer(value: Option<impl TryInto<i32>>) -> Option<i32> {
    value.and_then(|number| number.try_into().ok())
}

pub(crate) fn now_playing_entry(
    view: &TrackView,
    session: &ActivitySession,
    username: &str,
    tags: PayloadContext,
    at: chrono::DateTime<chrono::Utc>,
) -> models::NowPlayingEntry {
    let child = child_from_view(view, tags);
    let minutes = (at - session.last_event_at()).num_minutes().max(0);

    models::NowPlayingEntry {
        id: child.id,
        parent: child.parent,
        is_dir: child.is_dir,
        title: child.title,
        album: child.album,
        artist: child.artist,
        track: child.track,
        year: child.year,
        genre: child.genre,
        cover_art: child.cover_art,
        size: child.size,
        content_type: child.content_type,
        suffix: child.suffix,
        transcoded_content_type: child.transcoded_content_type,
        transcoded_suffix: child.transcoded_suffix,
        duration: child.duration,
        bit_rate: child.bit_rate,
        bit_depth: child.bit_depth,
        sampling_rate: child.sampling_rate,
        channel_count: child.channel_count,
        path: child.path,
        is_video: child.is_video,
        user_rating: child.user_rating,
        average_rating: child.average_rating,
        play_count: child.play_count,
        disc_number: child.disc_number,
        created: child.created,
        starred: child.starred,
        album_id: child.album_id,
        artist_id: child.artist_id,
        r#type: child.r#type,
        media_type: child.media_type,
        bookmark_position: child.bookmark_position,
        original_width: child.original_width,
        original_height: child.original_height,
        played: child.played,
        bpm: child.bpm,
        comment: child.comment,
        sort_name: child.sort_name,
        music_brainz_id: child.music_brainz_id,
        isrc: child.isrc,
        genres: child.genres,
        artists: child.artists,
        display_artist: child.display_artist,
        album_artists: child.album_artists,
        display_album_artist: child.display_album_artist,
        contributors: child.contributors,
        display_composer: child.display_composer,
        moods: child.moods,
        replay_gain: child.replay_gain,
        explicit_status: child.explicit_status,
        works: child.works,
        movements: child.movements,
        groupings: child.groupings,
        username: username.to_owned(),
        minutes_ago: i32::try_from(minutes).unwrap_or(i32::MAX),
        player_id: 0,
        player_name: Some(session.client().as_str().to_owned()),
        state: Some(playback_state(session.state())),
        position_ms: Some(milliseconds(session.position_ms())),
        playback_rate: Some(session.playback_rate().get() as f32),
    }
}

fn playback_state(state: PlaybackState) -> models::now_playing_entry::State {
    match state {
        PlaybackState::Starting => models::now_playing_entry::State::Starting,
        PlaybackState::Playing => models::now_playing_entry::State::Playing,
        PlaybackState::Paused => models::now_playing_entry::State::Paused,
        PlaybackState::Stopped => models::now_playing_entry::State::Stopped,
    }
}

pub(crate) fn album_from_summary(summary: &AlbumSummary) -> models::AlbumId3 {
    album_fields(summary).into_album()
}

pub(crate) fn album_with_songs(
    summary: &AlbumSummary,
    songs: Vec<models::Child>,
) -> models::AlbumId3WithSongs {
    album_fields(summary).into_album_with_songs(songs)
}

pub(crate) fn artist_from_summary(summary: &ArtistSummary) -> models::ArtistId3 {
    artist_id3(
        summary.artist(),
        Some(summary.album_count()),
        summary.state(),
    )
}

pub(crate) fn artist_with_albums(detail: &ArtistDetail) -> models::ArtistWithAlbumsId3 {
    let artist = detail.artist.artist();

    models::ArtistWithAlbumsId3 {
        id: artist.id().to_string(),
        name: artist.name().to_owned(),
        album_count: Some(detail.artist.album_count() as i32),
        music_brainz_id: artist.musicbrainz_artist_id().map(str::to_owned),
        sort_name: artist.sort_name().map(str::to_owned),
        starred: detail
            .artist
            .state()
            .starred_at()
            .map(|at| at.fixed_offset()),
        album: detail.albums.iter().map(album_from_summary).collect(),
        ..Default::default()
    }
}

const LASTFM_PROVIDER: &str = "lastfm";

struct ArtistInfoFields {
    biography: Option<String>,
    music_brainz_id: Option<String>,
    last_fm_url: Option<String>,
    small_image_url: Option<String>,
    medium_image_url: Option<String>,
    large_image_url: Option<String>,
}

fn artist_info_fields(information: &ArtistInformation) -> ArtistInfoFields {
    let artist = information.artist.artist.artist();
    let image_url = CoverArtTarget::artist(artist.id()).encode();
    let (biography, last_fm_url) = served_biography(&information.metadata.fields);

    ArtistInfoFields {
        biography,
        music_brainz_id: artist.musicbrainz_artist_id().map(str::to_owned),
        last_fm_url,
        small_image_url: Some(image_url.clone()),
        medium_image_url: Some(image_url.clone()),
        large_image_url: Some(image_url),
    }
}

pub(crate) fn artist_info(information: &ArtistInformation) -> models::ArtistInfo {
    let fields = artist_info_fields(information);

    models::ArtistInfo {
        biography: fields.biography,
        music_brainz_id: fields.music_brainz_id,
        last_fm_url: fields.last_fm_url,
        small_image_url: fields.small_image_url,
        medium_image_url: fields.medium_image_url,
        large_image_url: fields.large_image_url,
        similar_artist: None,
    }
}

pub(crate) fn artist_info2(
    information: &ArtistInformation,
    similar_artist: Vec<models::ArtistId3>,
) -> models::ArtistInfo2 {
    let fields = artist_info_fields(information);

    models::ArtistInfo2 {
        biography: fields.biography,
        music_brainz_id: fields.music_brainz_id,
        last_fm_url: fields.last_fm_url,
        small_image_url: fields.small_image_url,
        medium_image_url: fields.medium_image_url,
        large_image_url: fields.large_image_url,
        similar_artist: Some(similar_artist),
    }
}

fn served_biography(entries: &[FieldEntry]) -> (Option<String>, Option<String>) {
    let Some(entry) = entries
        .iter()
        .find(|entry| entry.field == FieldId::BiographyContent)
    else {
        return (None, None);
    };
    if entry.presence != Presence::Present {
        return (None, None);
    }
    let Some(FieldValue::Text(text)) = entry.value.as_ref() else {
        return (None, None);
    };
    let Some(terms) = entry.licence.as_ref() else {
        return (None, None);
    };
    if !terms.redistribution_allowed {
        return (None, None);
    }

    let credited = if terms.attribution_required {
        match entry.attribution.as_ref() {
            Some(attribution) => credited_text(text, attribution),
            None => return (None, None),
        }
    } else {
        text.clone()
    };

    (Some(credited), last_fm_url(entry))
}

fn credited_text(text: &str, attribution: &Attribution) -> String {
    match attribution.url.as_deref().filter(|url| !url.is_empty()) {
        Some(url) => format!("{text}\n\n{} {url}", attribution.text),
        None => format!("{text}\n\n{}", attribution.text),
    }
}

fn last_fm_url(entry: &FieldEntry) -> Option<String> {
    let source = entry.source.as_ref()?;

    if source.provider_id != LASTFM_PROVIDER {
        return None;
    }

    source.url.clone().filter(|url| !url.is_empty())
}

pub(crate) fn album_detail_with_songs(
    detail: &AlbumDetail,
    tags: PayloadContext,
) -> models::AlbumId3WithSongs {
    let songs = detail
        .entries
        .iter()
        .map(|entry| child_from_entry(entry, &detail.album, tags))
        .collect();

    album_with_songs(&detail.album, songs)
}

pub(crate) fn album_artist(artist: &Artist) -> models::ArtistId3 {
    artist_id3(artist, None, LibraryState::default())
}

pub(crate) fn playlist_from_domain(playlist: &Playlist) -> models::Playlist {
    models::Playlist {
        id: playlist.id().to_string(),
        name: playlist.name().to_owned(),
        comment: playlist.comment().map(str::to_owned),
        owner: Some(playlist.owner_username().to_owned()),
        public: Some(playlist.is_public()),
        song_count: i32::try_from(playlist.track_count()).unwrap_or(i32::MAX),
        duration: (playlist.duration_ms() / 1000) as i32,
        created: playlist.created_at().fixed_offset(),
        changed: playlist.updated_at().fixed_offset(),
        cover_art: None,
        allowed_user: None,
        readonly: None,
        valid_until: None,
    }
}

pub(crate) fn playlist_with_songs(
    detail: &PlaylistDetail,
    tags: PayloadContext,
) -> models::PlaylistWithSongs {
    let playlist = playlist_from_domain(&detail.playlist);

    models::PlaylistWithSongs {
        id: playlist.id,
        name: playlist.name,
        comment: playlist.comment,
        owner: playlist.owner,
        public: playlist.public,
        song_count: playlist.song_count,
        duration: playlist.duration,
        created: playlist.created,
        changed: playlist.changed,
        cover_art: playlist.cover_art,
        allowed_user: playlist.allowed_user,
        readonly: playlist.readonly,
        valid_until: playlist.valid_until,
        entry: Some(
            detail
                .entries
                .iter()
                .map(|entry| child_from_playlist_entry(entry, tags))
                .collect(),
        ),
    }
}

fn artist_id3(artist: &Artist, album_count: Option<u32>, state: LibraryState) -> models::ArtistId3 {
    models::ArtistId3 {
        id: artist.id().to_string(),
        name: artist.name().to_owned(),
        album_count: album_count.map(|count| count as i32),
        cover_art: Some(CoverArtTarget::artist(artist.id()).encode()),
        music_brainz_id: artist.musicbrainz_artist_id().map(str::to_owned),
        sort_name: artist.sort_name().map(str::to_owned),
        starred: state.starred_at().map(|at| at.fixed_offset()),
        ..Default::default()
    }
}

pub(crate) fn genre_from_summary(summary: &GenreSummary) -> models::Genre {
    models::Genre {
        value: summary.genre().to_owned(),
        song_count: summary.song_count() as i32,
        album_count: summary.album_count() as i32,
    }
}

pub(crate) fn music_folder(library: &Library) -> models::MusicFolder {
    models::MusicFolder {
        id: library.numeric_id(),
        name: Some(library.label().to_owned()),
    }
}

struct AlbumFields {
    id: String,
    name: String,
    artist: Option<String>,
    artist_id: Option<String>,
    artists: Option<Vec<models::ArtistId3>>,
    display_artist: Option<String>,
    cover_art: Option<String>,
    song_count: i32,
    duration: i32,
    created: chrono::DateTime<chrono::FixedOffset>,
    year: Option<i32>,
    record_labels: Option<Vec<models::RecordLabel>>,
    music_brainz_id: Option<String>,
    release_types: Option<Vec<String>>,
    play_count: Option<i32>,
    starred: Option<chrono::DateTime<chrono::FixedOffset>>,
    played: Option<chrono::DateTime<chrono::FixedOffset>>,
    user_rating: Option<i32>,
}

fn album_fields(summary: &AlbumSummary) -> AlbumFields {
    let album = summary.album();
    let artist = summary.album_artist();
    let artists = artist.map(|artist| vec![album_artist(artist)]);

    AlbumFields {
        id: album.id().to_string(),
        name: album.title().to_owned(),
        artist: artist.map(|artist| artist.name().to_owned()),
        artist_id: artist.map(|artist| artist.id().to_string()),
        artists,
        display_artist: artist.map(|artist| artist.name().to_owned()),
        cover_art: Some(CoverArtTarget::album(album.id()).encode()),
        song_count: summary.track_count() as i32,
        duration: (summary.duration_ms() / 1000) as i32,
        created: summary.created_at().fixed_offset(),
        year: album.year(),
        record_labels: album.label().map(|label| {
            vec![models::RecordLabel {
                name: label.to_owned(),
            }]
        }),
        music_brainz_id: album
            .musicbrainz_release_group_id()
            .or_else(|| album.musicbrainz_release_id())
            .map(str::to_owned),
        release_types: album
            .release_type()
            .map(|release_type| vec![release_type.to_owned()]),
        play_count: i32::try_from(summary.state().play_count())
            .ok()
            .filter(|count| *count > 0),
        starred: summary.state().starred_at().map(|at| at.fixed_offset()),
        played: summary.state().last_played_at().map(|at| at.fixed_offset()),
        user_rating: summary.state().rating().map(i32::from),
    }
}

impl AlbumFields {
    fn into_album(self) -> models::AlbumId3 {
        models::AlbumId3 {
            id: self.id,
            name: self.name,
            artist: self.artist,
            artist_id: self.artist_id,
            artists: self.artists,
            display_artist: self.display_artist,
            cover_art: self.cover_art,
            song_count: self.song_count,
            duration: self.duration,
            created: self.created,
            year: self.year,
            record_labels: self.record_labels,
            music_brainz_id: self.music_brainz_id,
            release_types: self.release_types,
            play_count: self.play_count,
            starred: self.starred,
            played: self.played,
            user_rating: self.user_rating,
            ..Default::default()
        }
    }

    fn into_album_with_songs(self, songs: Vec<models::Child>) -> models::AlbumId3WithSongs {
        models::AlbumId3WithSongs {
            id: self.id,
            name: self.name,
            artist: self.artist,
            artist_id: self.artist_id,
            artists: self.artists,
            display_artist: self.display_artist,
            cover_art: self.cover_art,
            song_count: self.song_count,
            duration: self.duration,
            created: self.created,
            year: self.year,
            record_labels: self.record_labels,
            music_brainz_id: self.music_brainz_id,
            release_types: self.release_types,
            play_count: self.play_count,
            starred: self.starred,
            played: self.played,
            user_rating: self.user_rating,
            song: songs,
            ..Default::default()
        }
    }
}

pub(crate) const IGNORED_ARTICLES: &str = "The El La Los Las Le Les Os As O A";

pub(crate) fn index_letter(name: &str) -> String {
    let name = strip_article(name);

    match name.chars().next() {
        Some(letter) if letter.is_ascii_alphabetic() => letter.to_ascii_uppercase().to_string(),
        _ => "#".to_owned(),
    }
}

fn strip_article(name: &str) -> &str {
    for article in IGNORED_ARTICLES.split(' ') {
        if let Some(rest) = name.strip_prefix(article)
            && let Some(rest) = rest.strip_prefix(' ')
        {
            return rest;
        }
    }

    name
}

pub(crate) fn group_by_index<T>(items: Vec<T>, name: impl Fn(&T) -> &str) -> Vec<(String, Vec<T>)> {
    let mut groups: Vec<(String, Vec<T>)> = Vec::new();

    for item in items {
        let letter = index_letter(name(&item));

        match groups.iter_mut().find(|(existing, _)| *existing == letter) {
            Some((_, group)) => group.push(item),
            None => groups.push((letter, vec![item])),
        }
    }

    groups.sort_by(|left, right| left.0.cmp(&right.0));
    groups
}

pub(crate) fn artist_index(artists: Vec<ArtistSummary>) -> Vec<models::Index> {
    group_by_index(artists, |summary| summary.artist().name())
        .into_iter()
        .map(|(name, artists)| models::Index {
            name,
            artist: Some(artists.iter().map(legacy_artist).collect()),
        })
        .collect()
}

fn legacy_artist(summary: &ArtistSummary) -> models::Artist {
    let artist = summary.artist();

    models::Artist {
        id: artist.id().to_string(),
        name: artist.name().to_owned(),
        artist_image_url: None,
        starred: summary.state().starred_at().map(|at| at.fixed_offset()),
        user_rating: summary.state().rating().map(i32::from),
        average_rating: None,
    }
}

pub(crate) enum DirectoryTarget<'a> {
    Artist(&'a ArtistDetail),
    Album(&'a AlbumDetail),
}

pub(crate) fn music_directory(
    target: DirectoryTarget<'_>,
    tags: PayloadContext,
) -> models::Directory {
    match target {
        DirectoryTarget::Artist(detail) => {
            let artist = detail.artist.artist();

            models::Directory {
                id: artist.id().to_string(),
                parent: None,
                name: artist.name().to_owned(),
                starred: detail
                    .artist
                    .state()
                    .starred_at()
                    .map(|at| at.fixed_offset()),
                user_rating: detail.artist.state().rating().map(i32::from),
                play_count: play_count(detail.artist.state()),
                average_rating: None,
                child: Some(detail.albums.iter().map(album_child).collect()),
            }
        }
        DirectoryTarget::Album(detail) => {
            let album = detail.album.album();

            models::Directory {
                id: album.id().to_string(),
                parent: album.album_artist_id().map(|id| id.to_string()),
                name: album.title().to_owned(),
                starred: detail
                    .album
                    .state()
                    .starred_at()
                    .map(|at| at.fixed_offset()),
                user_rating: detail.album.state().rating().map(i32::from),
                play_count: play_count(detail.album.state()),
                average_rating: None,
                child: Some(
                    detail
                        .entries
                        .iter()
                        .map(|entry| child_from_entry(entry, &detail.album, tags))
                        .collect(),
                ),
            }
        }
    }
}

fn play_count(state: LibraryState) -> Option<i32> {
    i32::try_from(state.play_count())
        .ok()
        .filter(|count| *count > 0)
}

fn album_child(summary: &AlbumSummary) -> models::Child {
    let album = summary.album();
    let artist = summary.album_artist();

    models::Child {
        id: album.id().to_string(),
        parent: album.album_artist_id().map(|id| id.to_string()),
        is_dir: true,
        title: album.title().to_owned(),
        album: Some(album.title().to_owned()),
        artist: artist.map(|artist| artist.name().to_owned()),
        year: album.year(),
        cover_art: Some(CoverArtTarget::album(album.id()).encode()),
        duration: Some((summary.duration_ms() / 1000) as i32),
        created: Some(summary.created_at().fixed_offset()),
        starred: summary.state().starred_at().map(|at| at.fixed_offset()),
        play_count: play_count(summary.state()),
        user_rating: summary.state().rating().map(i32::from),
        artist_id: album.album_artist_id().map(|id| id.to_string()),
        media_type: Some(models::MediaType::Album),
        ..Default::default()
    }
}

pub(crate) const BOUND_MESSAGE: &str = "Queue holds at most 1000 entries.";

pub(crate) fn play_queue(view: PlayQueueView) -> models::PlayQueue {
    let current = view
        .current_index
        .and_then(|index| view.entries.get(index as usize))
        .map(|entry| entry.id.clone());

    models::PlayQueue {
        current,
        position: Some(milliseconds(view.position_ms)),
        username: view.username,
        changed: view.changed_at.fixed_offset(),
        changed_by: no_changed_by(),
        entry: Some(view.entries),
    }
}

pub(crate) fn empty_play_queue(username: &str) -> models::PlayQueue {
    models::PlayQueue {
        current: None,
        position: None,
        username: username.to_owned(),
        changed: no_queue_changed(),
        changed_by: no_changed_by(),
        entry: None,
    }
}

pub(crate) fn play_queue_by_index(view: PlayQueueView) -> models::PlayQueueByIndex {
    models::PlayQueueByIndex {
        current_index: view.current_index.map(zero_based),
        position: Some(milliseconds(view.position_ms)),
        username: view.username,
        changed: view.changed_at.fixed_offset(),
        changed_by: no_changed_by(),
        entry: Some(view.entries),
    }
}

pub(crate) fn empty_play_queue_by_index(username: &str) -> models::PlayQueueByIndex {
    models::PlayQueueByIndex {
        current_index: None,
        position: None,
        username: username.to_owned(),
        changed: no_queue_changed(),
        changed_by: no_changed_by(),
        entry: None,
    }
}

fn zero_based(index: u32) -> i32 {
    i32::try_from(index).unwrap_or(i32::MAX)
}

fn milliseconds(position_ms: u64) -> i32 {
    i32::try_from(position_ms).unwrap_or(i32::MAX)
}

fn no_changed_by() -> String {
    String::new()
}

fn no_queue_changed() -> chrono::DateTime<chrono::FixedOffset> {
    chrono::DateTime::from_naive_utc_and_offset(
        chrono::NaiveDate::from_ymd_opt(1, 1, 1)
            .expect("year 1 is a representable date")
            .and_hms_opt(0, 0, 0)
            .expect("midnight is a representable time"),
        chrono::FixedOffset::east_opt(0).expect("utc is a valid fixed offset"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::features::metadata::domain::{
        EntityKey, FactSource, FieldValue, Freshness, MatchBasis, MetadataEntityKind,
        MetadataRecord,
    };
    use crate::features::metadata::licences::{LicenceTerms, licence_terms};
    use crate::features::music_library::domain::{
        Album, AlbumId, Artist, ArtistId, PlaylistEntryId, ProviderId, RefreshStatus,
        SourceIdentity, SourceRepresentationFacts, SourceScope, Track, TrackId, TrackMetadata,
    };
    use crate::features::music_library::queue::MAX_RESTORE_QUEUE_ENTRIES;

    fn track(artists: &[&str]) -> Track {
        Track::new(
            TrackId::new(),
            TrackMetadata::from_names(
                "song".to_owned(),
                artists.iter().map(|artist| (*artist).to_owned()).collect(),
                Some(200_000),
            )
            .expect("valid metadata"),
        )
    }

    fn album() -> Album {
        Album::new(
            AlbumId::new(),
            "album".to_owned(),
            None,
            None,
            None,
            None,
            None,
            None,
        )
        .expect("valid album")
    }

    fn source() -> SourceIdentity {
        SourceIdentity::new(
            ProviderId::new("youtube_music".to_owned()).expect("valid provider"),
            SourceScope::Global,
            "vid-1".to_owned(),
        )
        .expect("valid source")
    }

    fn track_view(track: Track, album: Option<Album>, sources: Vec<SourceIdentity>) -> TrackView {
        TrackView::new(track, album, None, None, LibraryState::default(), sources)
    }

    fn representation() -> SourceRepresentationFacts {
        SourceRepresentationFacts {
            source: source(),
            content_type: Some("audio/flac".to_owned()),
            suffix: Some("flac".to_owned()),
            container: Some("flac".to_owned()),
            codec: Some("flac".to_owned()),
            size_bytes: Some(4_000),
            bitrate_kbps: Some(900),
            sample_rate_hz: Some(44_100),
            bit_depth: Some(16),
            channels: Some(2),
        }
    }

    #[test]
    fn a_view_carries_its_album_and_cover_art() {
        let track = track(&["artist"]);
        let id = track.id().to_string();
        let view = track_view(track, Some(album()), vec![source()]);

        let child = child_from_view(&view, PayloadContext::default());

        assert_eq!(child.album.as_deref(), Some("album"));
        assert_eq!(child.cover_art.as_deref(), Some(id.as_str()));
        assert_eq!(child.artist.as_deref(), Some("artist"));
    }

    #[test]
    fn a_view_tags_the_artist_only_when_configured() {
        let view = track_view(
            track(&["artist", "featured"]),
            Some(album()),
            vec![source()],
        );

        assert_eq!(
            child_from_view(&view, PayloadContext::default())
                .artist
                .as_deref(),
            Some("artist, featured")
        );
        assert_eq!(
            child_from_view(
                &view,
                PayloadContext::from(crate::facades::SourceTags::ArtistPrefix)
            )
            .artist
            .as_deref(),
            Some("[YTM] artist, featured")
        );
    }

    #[test]
    fn an_album_entry_carries_its_place_on_the_album() {
        let album = album();
        let entry = AlbumEntry::new(
            track(&["artist"]),
            AlbumTrack::new(Some(1), Some(3), 2).expect("valid membership"),
        );
        let summary = AlbumSummary::new(album.clone(), None, 1, 200_000, chrono::Utc::now());

        let child = child_from_entry(&entry, &summary, PayloadContext::default());

        assert_eq!(child.album.as_deref(), Some("album"));
        assert_eq!(
            child.album_id.as_deref(),
            Some(album.id().to_string().as_str())
        );
        assert_eq!(child.track, Some(3));
        assert_eq!(child.disc_number, Some(1));
    }

    #[test]
    fn child_carries_the_selected_sources_representation_facts() {
        let view = track_view(track(&["artist"]), Some(album()), vec![source()])
            .with_representation(Some(representation()));

        let child = child_from_view(&view, PayloadContext::default());

        assert_eq!(child.content_type.as_deref(), Some("audio/flac"));
        assert_eq!(child.suffix.as_deref(), Some("flac"));
        assert_eq!(child.size, Some(4_000));
        assert_eq!(child.bit_rate, Some(900));
        assert_eq!(child.sampling_rate, Some(44_100));
        assert_eq!(child.bit_depth, Some(16));
        assert_eq!(child.channel_count, Some(2));
    }

    #[test]
    fn provider_only_track_uses_an_unknown_representation_until_delivery() {
        let view = track_view(track(&["artist"]), Some(album()), vec![source()]);

        let child = child_from_view(&view, PayloadContext::default());

        assert_eq!(
            child.content_type.as_deref(),
            Some("application/octet-stream")
        );
        assert_eq!(child.suffix, None);
        assert_eq!(child.size, None);
        assert_eq!(child.bit_rate, None);
        assert_eq!(child.sampling_rate, None);
        assert_eq!(child.bit_depth, None);
        assert_eq!(child.channel_count, None);

        let mut unrecorded = representation();
        unrecorded.content_type = None;
        let selected = view.with_representation(Some(unrecorded));

        let child = child_from_view(&selected, PayloadContext::default());

        assert_eq!(
            child.content_type.as_deref(),
            Some("application/octet-stream")
        );
        assert_eq!(child.size, Some(4_000));
    }

    #[test]
    fn representation_zero_and_protocol_integer_overflow_are_distinct() {
        let mut zero = representation();
        zero.size_bytes = Some(0);
        zero.bitrate_kbps = Some(0);
        let view = track_view(track(&["artist"]), Some(album()), vec![source()])
            .with_representation(Some(zero));

        let child = child_from_view(&view, PayloadContext::default());

        assert_eq!(child.size, Some(0));
        assert_eq!(child.bit_rate, Some(0));

        let mut overflow = representation();
        overflow.size_bytes = Some(u64::try_from(i32::MAX).unwrap() + 1);
        overflow.bitrate_kbps = Some(u32::try_from(i32::MAX).unwrap() + 1);
        let view = track_view(track(&["artist"]), Some(album()), vec![source()])
            .with_representation(Some(overflow));

        let child = child_from_view(&view, PayloadContext::default());

        assert_eq!(child.size, None);
        assert_eq!(child.bit_rate, None);
    }

    #[test]
    fn playlist_children_preserve_the_shared_album_membership() {
        let view = TrackView::new(
            track(&["artist"]),
            Some(album()),
            Some(AlbumTrack::new(Some(2), Some(4), 3).expect("valid membership")),
            None,
            LibraryState::default(),
            vec![source()],
        );
        let entry = PlaylistEntry::new(PlaylistEntryId::new(), 0, view);

        let child = child_from_playlist_entry(&entry, PayloadContext::default());

        assert_eq!(child.track, Some(4));
        assert_eq!(child.disc_number, Some(2));
        assert_eq!(child.album.as_deref(), Some("album"));
    }

    #[test]
    fn an_album_summary_maps_its_artist_and_totals() {
        let artist = Artist::new(ArtistId::new(), "Daft Punk".to_owned(), None, None).unwrap();
        let summary = AlbumSummary::new(
            album(),
            Some(artist.clone()),
            2,
            400_000,
            chrono::Utc::now(),
        );

        let mapped = album_from_summary(&summary);

        assert_eq!(mapped.name, "album");
        assert_eq!(mapped.artist.as_deref(), Some("Daft Punk"));
        assert_eq!(
            mapped.artist_id.as_deref(),
            Some(artist.id().to_string().as_str())
        );
        assert_eq!(mapped.song_count, 2);
        assert_eq!(mapped.duration, 400);
        assert_eq!(
            mapped.cover_art.as_deref(),
            Some(format!("al-{}", mapped.id).as_str())
        );
    }

    #[test]
    fn an_index_letter_follows_the_ignored_articles() {
        assert_eq!(index_letter("Daft Punk"), "D");
        assert_eq!(index_letter("The Beatles"), "B");
        assert_eq!(index_letter("Las Vegas"), "V");
        assert_eq!(index_letter("A Perfect Circle"), "P");
        assert_eq!(index_letter("Los Lobos"), "L");
        assert_eq!(index_letter("99 Posse"), "#");
        assert_eq!(index_letter("the beatles"), "T");
        assert_eq!(index_letter(""), "#");
    }

    #[test]
    fn grouping_files_each_item_under_its_letter_in_order() {
        let groups = group_by_index(
            vec!["The Beatles", "Daft Punk", "99 Posse", "Bjork"],
            |name| name,
        );
        let names = groups
            .iter()
            .map(|(letter, _)| letter.as_str())
            .collect::<Vec<_>>();

        assert_eq!(names, ["#", "B", "D"]);
        assert_eq!(groups[0].1, ["99 Posse"]);
        assert_eq!(groups[1].1, ["The Beatles", "Bjork"]);
        assert_eq!(groups[2].1, ["Daft Punk"]);
    }

    #[test]
    fn the_bound_message_names_the_configured_bound() {
        assert!(BOUND_MESSAGE.contains(&MAX_RESTORE_QUEUE_ENTRIES.to_string()));
    }

    fn terms(id: &str) -> LicenceTerms {
        licence_terms(id, 1).expect("the registry carries the licence")
    }

    fn artist(mbid: Option<&str>) -> Artist {
        Artist::new(
            ArtistId::new(),
            "Daft Punk".to_owned(),
            Some("Daft Punk".to_owned()),
            mbid.map(str::to_owned),
        )
        .expect("valid artist")
    }

    fn information(artist: Artist, fields: Vec<FieldEntry>) -> ArtistInformation {
        ArtistInformation {
            cover_art: CoverArtTarget::artist(artist.id()).encode(),
            artist: crate::features::music_library::catalogue::ArtistDetail {
                artist: ArtistSummary::new(artist, 2),
                albums: Vec::new(),
            },
            metadata: MetadataRecord {
                entity: EntityKey::new(MetadataEntityKind::Artist, uuid::Uuid::nil()),
                display_name: "Daft Punk".to_owned(),
                identity: None,
                fields,
                disagreements: Vec::new(),
                contested: false,
                generated_at: chrono::Utc::now(),
            },
            portraits: Vec::new(),
            refresh: RefreshStatus {
                state: crate::features::metadata::RefreshState {
                    phase: crate::features::metadata::RefreshPhase::Idle,
                    identity_revision: "revision".to_owned(),
                    job_id: None,
                },
                outcome: None,
            },
        }
    }

    fn entry(field: FieldId, value: FieldValue, licence: &str) -> FieldEntry {
        FieldEntry {
            field,
            presence: Presence::Present,
            value: Some(value),
            source: Some(FactSource {
                provider_id: licence.to_owned(),
                external_id: "external-1".to_owned(),
                url: None,
                match_basis: MatchBasis::Identifier,
            }),
            licence: Some(terms(licence)),
            attribution: Some(Attribution {
                text: "Wikipedia contributors".to_owned(),
                url: Some("https://en.wikipedia.org/wiki/Daft_Punk".to_owned()),
            }),
            bytes: None,
            facts: Vec::new(),
            attempt: None,
            retrieved_at: None,
            freshness: Freshness::Fresh,
            absence: None,
        }
    }

    fn biography(
        text: &str,
        licence: &str,
        provider: &str,
        source_url: Option<&str>,
    ) -> FieldEntry {
        let mut entry = entry(
            FieldId::BiographyContent,
            FieldValue::Text(text.to_owned()),
            licence,
        );
        entry.source = Some(FactSource {
            provider_id: provider.to_owned(),
            external_id: "external-1".to_owned(),
            url: source_url.map(str::to_owned),
            match_basis: MatchBasis::Identifier,
        });

        entry
    }

    #[test]
    fn a_wikipedia_biography_carries_its_attribution_and_link() {
        let information = information(
            artist(None),
            vec![biography(
                "A French electronic duo.",
                "wikipedia",
                "wikipedia",
                Some("https://en.wikipedia.org/wiki/Daft_Punk"),
            )],
        );

        let info = artist_info2(&information, Vec::new());

        assert_eq!(
            info.biography.as_deref(),
            Some(
                "A French electronic duo.\n\nWikipedia contributors \
                 https://en.wikipedia.org/wiki/Daft_Punk"
            )
        );
        assert_eq!(info.last_fm_url, None);
    }

    #[test]
    fn a_lastfm_biography_populates_the_lastfm_url() {
        let information = information(
            artist(None),
            vec![biography(
                "A French electronic duo.",
                "lastfm",
                "lastfm",
                Some("https://www.last.fm/music/Daft+Punk"),
            )],
        );

        let info = artist_info2(&information, Vec::new());

        assert_eq!(
            info.last_fm_url.as_deref(),
            Some("https://www.last.fm/music/Daft+Punk")
        );
        assert!(
            info.biography
                .as_deref()
                .is_some_and(|text| text.contains("Wikipedia contributors"))
        );
    }

    #[test]
    fn a_lastfm_biography_without_a_source_url_omits_the_lastfm_url() {
        let information = information(
            artist(None),
            vec![biography("A duo.", "lastfm", "lastfm", None)],
        );

        assert_eq!(artist_info2(&information, Vec::new()).last_fm_url, None);
    }

    #[test]
    fn a_biography_whose_licence_forbids_redistribution_is_omitted() {
        let information = information(
            artist(None),
            vec![biography(
                "A biography we may not redistribute.",
                "theaudiodb",
                "theaudiodb",
                Some("https://www.theaudiodb.com/artist/1"),
            )],
        );

        let info = artist_info2(&information, Vec::new());

        assert_eq!(info.biography, None);
        assert_eq!(info.last_fm_url, None);
    }

    #[test]
    fn a_biography_that_cannot_be_cited_is_omitted() {
        let mut entry = biography("A duo.", "wikipedia", "wikipedia", None);
        entry.attribution = None;
        let information = information(artist(None), vec![entry]);

        assert_eq!(artist_info2(&information, Vec::new()).biography, None);
    }

    #[test]
    fn an_absent_or_unlicensed_biography_is_omitted() {
        let mut absent = biography("A duo.", "wikipedia", "wikipedia", None);
        absent.presence = Presence::Absent;
        absent.value = None;
        let mut unlicensed = biography("A duo.", "wikipedia", "wikipedia", None);
        unlicensed.licence = None;
        let present = information(
            artist(None),
            vec![biography("A duo.", "wikipedia", "wikipedia", None)],
        );
        let absent = information(artist(None), vec![absent]);
        let unlicensed = information(artist(None), vec![unlicensed]);

        for missing in [&absent, &unlicensed] {
            assert_eq!(artist_info(missing).biography, None);
            assert_eq!(artist_info2(missing, Vec::new()).biography, None);
        }

        assert!(artist_info(&present).biography.is_some());
        assert!(artist_info2(&present, Vec::new()).biography.is_some());
    }

    #[test]
    fn a_biography_without_a_citation_requirement_is_served_verbatim() {
        let information = information(
            artist(None),
            vec![biography("A duo.", "musicbrainz", "musicbrainz", None)],
        );

        assert_eq!(
            artist_info2(&information, Vec::new()).biography.as_deref(),
            Some("A duo.")
        );
    }

    #[test]
    fn the_canonical_mbid_is_served_rather_than_the_identity_fact() {
        let mut mbid_fact = entry(
            FieldId::IdentityMbid,
            FieldValue::Text("from-a-fact".to_owned()),
            "musicbrainz",
        );
        mbid_fact.source = Some(FactSource {
            provider_id: "musicbrainz".to_owned(),
            external_id: "from-a-fact".to_owned(),
            url: None,
            match_basis: MatchBasis::Identifier,
        });
        let information = information(artist(Some("canonical-column")), vec![mbid_fact]);

        assert_eq!(
            artist_info2(&information, Vec::new())
                .music_brainz_id
                .as_deref(),
            Some("canonical-column")
        );
    }

    #[test]
    fn the_image_urls_are_the_mounted_artist_route() {
        let artist = artist(None);
        let id = artist.id().to_string();
        let information = information(
            artist,
            vec![entry(
                FieldId::ImagePortrait,
                FieldValue::Reference("https://external.example/portrait.jpg".to_owned()),
                "commons",
            )],
        );

        let info = artist_info2(&information, Vec::new());
        let mounted = format!("ar-{id}");

        assert_eq!(info.large_image_url.as_deref(), Some(mounted.as_str()));
        assert_eq!(info.medium_image_url.as_deref(), Some(mounted.as_str()));
        assert_eq!(info.small_image_url.as_deref(), Some(mounted.as_str()));
    }

    #[test]
    fn both_artist_info_shapes_agree_on_the_shared_fields() {
        let information = information(
            artist(Some("canonical-column")),
            vec![biography(
                "A duo.",
                "wikipedia",
                "wikipedia",
                Some("https://en.wikipedia.org/wiki/Daft_Punk"),
            )],
        );

        let legacy = artist_info(&information);
        let id3 = artist_info2(&information, Vec::new());

        assert_eq!(legacy.biography, id3.biography);
        assert_eq!(legacy.music_brainz_id, id3.music_brainz_id);
        assert_eq!(legacy.last_fm_url, id3.last_fm_url);
        assert_eq!(legacy.large_image_url, id3.large_image_url);
        assert_eq!(legacy.medium_image_url, id3.medium_image_url);
        assert_eq!(legacy.small_image_url, id3.small_image_url);
    }
}
