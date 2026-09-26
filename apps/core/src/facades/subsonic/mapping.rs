use whio_subsonic_api::models;

use crate::{
    facades::SourceTags,
    features::music_library::artwork::CoverArtTarget,
    features::music_library::catalogue::{AlbumDetail, ArtistDetail, SearchHit},
    features::music_library::domain::{
        Album, AlbumEntry, AlbumId, AlbumSummary, AlbumTrack, Artist, ArtistSummary, GenreSummary,
        LibraryState, Playlist, SourceIdentity, Track,
    },
    features::music_library::playlists::PlaylistDetail,
};

pub(crate) fn child_from_hit(hit: &SearchHit, tags: SourceTags) -> models::Child {
    child_from_track(
        hit.track(),
        hit.album().map(Album::id),
        hit.album().map(Album::title),
        hit.sources(),
        tags,
        None,
        hit.state(),
    )
}

pub(crate) fn child_from_entry(
    entry: &AlbumEntry,
    album_id: &AlbumId,
    album_title: &str,
    tags: SourceTags,
) -> models::Child {
    child_from_track(
        entry.track(),
        Some(album_id),
        Some(album_title),
        entry.sources(),
        tags,
        Some(entry.membership()),
        entry.state(),
    )
}

fn child_from_track(
    track: &Track,
    album_id: Option<&AlbumId>,
    album_title: Option<&str>,
    sources: &[SourceIdentity],
    tags: SourceTags,
    membership: Option<&AlbumTrack>,
    state: LibraryState,
) -> models::Child {
    models::Child {
        id: track.id().to_string(),
        is_dir: false,
        title: track.title().to_owned(),
        album: album_title.map(str::to_owned),
        artist: (!track.artists().is_empty()).then(|| tags.artist(track.artists(), sources)),
        track: membership.and_then(AlbumTrack::track_number).map(i32::from),
        cover_art: Some(CoverArtTarget::song(track.id()).encode()),
        duration: track.duration_ms().map(|ms| (ms / 1000) as i32),
        disc_number: membership.and_then(AlbumTrack::disc_number).map(i32::from),
        album_id: album_id.map(AlbumId::to_string),
        genre: track.metadata().genre().map(str::to_owned),
        user_rating: state.rating().map(i32::from),
        play_count: i32::try_from(state.play_count())
            .ok()
            .filter(|count| *count > 0),
        starred: state.starred_at().map(|at| at.fixed_offset()),
        played: state.last_played_at().map(|at| at.fixed_offset()),
        media_type: Some(models::MediaType::Song),
        r#type: Some(models::GenericMediaType::Music),
        ..Default::default()
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

pub(crate) fn album_detail_with_songs(
    detail: &AlbumDetail,
    tags: SourceTags,
) -> models::AlbumId3WithSongs {
    let album = detail.album.album();
    let songs = detail
        .entries
        .iter()
        .map(|entry| child_from_entry(entry, album.id(), album.title(), tags))
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
        owner: None,
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
    tags: SourceTags,
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
                .map(|hit| child_from_hit(hit, tags))
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::features::music_library::domain::{
        Artist, ArtistId, ProviderId, SourceScope, TrackId, TrackMetadata,
    };

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

    #[test]
    fn a_hit_carries_its_album_and_cover_art() {
        let track = track(&["artist"]);
        let id = track.id().to_string();
        let hit = SearchHit::new(track, Some(album()), vec![source()]);

        let child = child_from_hit(&hit, SourceTags::Off);

        assert_eq!(child.album.as_deref(), Some("album"));
        assert_eq!(child.cover_art.as_deref(), Some(id.as_str()));
        assert_eq!(child.artist.as_deref(), Some("artist"));
    }

    #[test]
    fn a_hit_tags_the_artist_only_when_configured() {
        let hit = SearchHit::new(
            track(&["artist", "featured"]),
            Some(album()),
            vec![source()],
        );

        assert_eq!(
            child_from_hit(&hit, SourceTags::Off).artist.as_deref(),
            Some("artist, featured")
        );
        assert_eq!(
            child_from_hit(&hit, SourceTags::ArtistPrefix)
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

        let child = child_from_entry(&entry, album.id(), album.title(), SourceTags::Off);

        assert_eq!(child.album.as_deref(), Some("album"));
        assert_eq!(
            child.album_id.as_deref(),
            Some(album.id().to_string().as_str())
        );
        assert_eq!(child.track, Some(3));
        assert_eq!(child.disc_number, Some(1));
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
}
