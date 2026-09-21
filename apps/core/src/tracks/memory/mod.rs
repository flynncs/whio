use std::{
    collections::{HashMap, HashSet},
    sync::Mutex,
};

use uuid::Uuid;

use crate::tracks::{
    Album, AlbumEntry, AlbumId, AlbumListFilter, AlbumMetadata, AlbumOrder, AlbumSummary,
    AlbumTrack, Artist, ArtistCredit, ArtistId, ArtistSummary, ArtworkRef, GenreSummary,
    LibraryState, MatchQuality, Playlist, PlaylistId, ProviderEntityState, ProviderId,
    ProviderKind, SearchTerms, SourceFileFacts, Starred, StateTarget, Track, TrackHit,
    TrackMetadata, checked_rating,
    domain::{album_quality, normalize_for_matching, track_quality},
};

use super::{SourceIdentity, SourceScope, TrackId, TrackRepositoryError};

mod catalogue;
mod library;
mod playlists;
mod reports;
mod sources;
mod state;

#[cfg(test)]
mod repository_tests;

type StarredAt = chrono::DateTime<chrono::Utc>;

struct TrackEntry {
    pub(super) track: Track,
    pub(super) sources: Vec<SourceIdentity>,
}

struct StoredPlaylist {
    pub(super) user: Uuid,
    pub(super) name: String,
    pub(super) comment: Option<String>,
    pub(super) public: bool,
    pub(super) created_at: chrono::DateTime<chrono::Utc>,
    pub(super) updated_at: chrono::DateTime<chrono::Utc>,
}

impl StoredPlaylist {
    fn playlist(&self, id: &PlaylistId, track_count: u32) -> Playlist {
        Playlist::new(
            id.clone(),
            self.name.clone(),
            self.comment.clone(),
            self.public,
            self.created_at,
            self.updated_at,
            track_count,
        )
        .expect("a stored playlist stays valid")
    }
}

#[derive(Default)]
struct TrackIndexes {
    pub(super) by_source: HashMap<SourceIdentity, TrackId>,
    pub(super) by_track_id: HashMap<TrackId, TrackEntry>,
    pub(super) artwork_by_source: HashMap<SourceIdentity, ArtworkRef>,
    pub(super) file_facts_by_source: HashMap<SourceIdentity, SourceFileFacts>,
    pub(super) artists_by_id: HashMap<ArtistId, Artist>,
    pub(super) artist_by_identifier: HashMap<String, ArtistId>,
    pub(super) artist_by_name: HashMap<String, ArtistId>,
    pub(super) albums_by_id: HashMap<AlbumId, Album>,
    pub(super) album_by_release_group: HashMap<String, AlbumId>,
    pub(super) album_by_release: HashMap<String, AlbumId>,
    pub(super) album_by_text: HashMap<(Option<ArtistId>, String), AlbumId>,
    pub(super) album_sources: HashMap<AlbumId, Vec<SourceIdentity>>,
    pub(super) artist_sources: HashMap<ArtistId, Vec<SourceIdentity>>,
    pub(super) provider_entity_state: HashMap<SourceIdentity, ProviderEntityState>,
    pub(super) album_tracks: HashMap<AlbumId, Vec<(TrackId, AlbumTrack)>>,
    pub(super) album_created_at: HashMap<AlbumId, chrono::DateTime<chrono::Utc>>,
    pub(super) track_favorites: HashMap<(Uuid, TrackId), StarredAt>,
    pub(super) album_favorites: HashMap<(Uuid, AlbumId), StarredAt>,
    pub(super) artist_favorites: HashMap<(Uuid, ArtistId), StarredAt>,
    pub(super) track_ratings: HashMap<(Uuid, TrackId), u8>,
    pub(super) album_ratings: HashMap<(Uuid, AlbumId), u8>,
    pub(super) artist_ratings: HashMap<(Uuid, ArtistId), u8>,
    pub(super) track_plays: HashMap<(Uuid, TrackId), (u32, Option<StarredAt>)>,
    pub(super) playlists: HashMap<PlaylistId, StoredPlaylist>,
    pub(super) playlist_tracks: HashMap<PlaylistId, Vec<TrackId>>,
}

#[derive(Default)]
pub struct InMemoryTrackRepository {
    indexes: Mutex<TrackIndexes>,
}

fn owned(sources: &[SourceIdentity]) -> bool {
    sources
        .iter()
        .any(|source| source.provider_id().kind() == Some(ProviderKind::Local))
}

fn identified_track(indexes: &TrackIndexes, metadata: &TrackMetadata) -> Option<TrackId> {
    if let Some(recording_id) = metadata.musicbrainz_recording_id()
        && let Some((track_id, _)) = indexes
            .by_track_id
            .iter()
            .find(|(_, entry)| entry.track.musicbrainz_recording_id() == Some(recording_id))
    {
        return Some(track_id.clone());
    }

    let isrc = metadata.isrc()?;
    let mut matches = indexes
        .by_track_id
        .iter()
        .filter(|(_, entry)| entry.track.isrc() == Some(isrc));
    let only = matches.next()?;

    matches.next().is_none().then(|| only.0.clone())
}

fn attach_source(indexes: &mut TrackIndexes, source: &SourceIdentity, track_id: &TrackId) {
    if let Some(entry) = indexes.by_track_id.get_mut(track_id)
        && !entry.sources.contains(source)
    {
        entry.sources.push(source.clone());
    }

    indexes.by_source.insert(source.clone(), track_id.clone());
}

fn detach_source(indexes: &mut TrackIndexes, source: &SourceIdentity, track_id: &TrackId) {
    if let Some(entry) = indexes.by_track_id.get_mut(track_id) {
        entry.sources.retain(|existing| existing != source);
    }
}

fn drop_source_less_track(indexes: &mut TrackIndexes, track_id: &TrackId) {
    if indexes
        .by_track_id
        .get(track_id)
        .is_some_and(|entry| entry.sources.is_empty())
    {
        indexes.by_track_id.remove(track_id);
    }
}

fn preferred_artist(indexes: &TrackIndexes, candidate: &Artist, current: &Artist) -> bool {
    candidate
        .musicbrainz_artist_id()
        .is_some()
        .cmp(&current.musicbrainz_artist_id().is_some())
        .then_with(|| album_count(indexes, candidate.id()).cmp(&album_count(indexes, current.id())))
        .then_with(|| current.id().to_string().cmp(&candidate.id().to_string()))
        .is_gt()
}

fn detach_track(
    memberships: &mut HashMap<AlbumId, Vec<(TrackId, AlbumTrack)>>,
    keep: Option<&AlbumId>,
    track_id: &TrackId,
) {
    for (album_id, entries) in memberships.iter_mut() {
        if keep == Some(album_id) {
            continue;
        }
        entries.retain(|(entry, _)| entry != track_id);
    }
}

fn album_for_track(indexes: &TrackIndexes, track_id: &TrackId) -> Option<Album> {
    let album_id = indexes
        .album_tracks
        .iter()
        .find_map(|(album_id, entries)| {
            entries
                .iter()
                .any(|(entry, _)| entry == track_id)
                .then_some(album_id)
        })?;

    indexes.albums_by_id.get(album_id).cloned()
}

fn membership_order(membership: &AlbumTrack) -> (u16, bool, u16, u32) {
    (
        membership.disc_number().unwrap_or(1),
        membership.track_number().is_none(),
        membership.track_number().unwrap_or(0),
        membership.position(),
    )
}

fn ordered_memberships(indexes: &TrackIndexes, album_id: &AlbumId) -> Vec<(TrackId, AlbumTrack)> {
    let Some(memberships) = indexes.album_tracks.get(album_id) else {
        return Vec::new();
    };

    let mut ordered = memberships.clone();
    ordered.sort_by(|left, right| {
        membership_order(&left.1)
            .cmp(&membership_order(&right.1))
            .then_with(|| left.0.to_string().cmp(&right.0.to_string()))
    });

    ordered
}

fn ordered_albums(
    indexes: &TrackIndexes,
    keep: impl Fn(&Album) -> bool,
    order: AlbumOrder,
    user: Option<Uuid>,
) -> Vec<&Album> {
    let mut albums = indexes
        .albums_by_id
        .values()
        .filter(|album| keep(album))
        .collect::<Vec<_>>();
    let seed = Uuid::now_v7().as_u128() as u64;

    albums.sort_by(|left, right| {
        let primary = match order {
            AlbumOrder::Title => {
                normalize_for_matching(left.title()).cmp(&normalize_for_matching(right.title()))
            }
            AlbumOrder::Artist => album_artist_name(indexes, left)
                .cmp(&album_artist_name(indexes, right))
                .then_with(|| {
                    normalize_for_matching(left.title()).cmp(&normalize_for_matching(right.title()))
                }),
            AlbumOrder::RecentlyAdded => {
                album_created_at(indexes, right).cmp(&album_created_at(indexes, left))
            }
            AlbumOrder::Random => shuffle_key(left.id(), seed).cmp(&shuffle_key(right.id(), seed)),
            AlbumOrder::Frequent => album_state(indexes, right.id(), user)
                .play_count()
                .cmp(&album_state(indexes, left.id(), user).play_count()),
            AlbumOrder::Recent => album_state(indexes, right.id(), user)
                .last_played_at()
                .cmp(&album_state(indexes, left.id(), user).last_played_at()),
            AlbumOrder::HighestRated => album_state(indexes, right.id(), user)
                .rating()
                .cmp(&album_state(indexes, left.id(), user).rating()),
            AlbumOrder::Starred => album_state(indexes, right.id(), user)
                .starred_at()
                .cmp(&album_state(indexes, left.id(), user).starred_at()),
            AlbumOrder::YearAscending => left
                .year()
                .is_none()
                .cmp(&right.year().is_none())
                .then_with(|| left.year().cmp(&right.year())),
            AlbumOrder::YearDescending => right
                .year()
                .is_none()
                .cmp(&left.year().is_none())
                .then_with(|| right.year().cmp(&left.year())),
        };

        primary
            .then_with(|| {
                normalize_for_matching(left.title()).cmp(&normalize_for_matching(right.title()))
            })
            .then_with(|| left.id().to_string().cmp(&right.id().to_string()))
    });

    albums
}

fn shuffle_key(album_id: &AlbumId, seed: u64) -> u64 {
    mix(album_id.as_uuid().as_u128() as u64 ^ seed)
}

fn mix(mut value: u64) -> u64 {
    value = value.wrapping_add(0x9E37_79B9_7F4A_7C15);
    value = (value ^ (value >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    value = (value ^ (value >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    value ^ (value >> 31)
}

fn album_artist_name(indexes: &TrackIndexes, album: &Album) -> String {
    album
        .album_artist_id()
        .and_then(|artist_id| indexes.artists_by_id.get(artist_id))
        .map_or_else(String::new, |artist| normalize_for_matching(artist.name()))
}

fn album_created_at(indexes: &TrackIndexes, album: &Album) -> chrono::DateTime<chrono::Utc> {
    indexes
        .album_created_at
        .get(album.id())
        .copied()
        .unwrap_or(chrono::DateTime::UNIX_EPOCH)
}

fn album_summary(indexes: &TrackIndexes, album: &Album, user: Option<Uuid>) -> AlbumSummary {
    let memberships = ordered_memberships(indexes, album.id());
    let track_count = memberships.len() as u32;
    let duration_ms = memberships
        .iter()
        .filter_map(|(track_id, _)| indexes.by_track_id.get(track_id))
        .filter_map(|entry| entry.track.duration_ms())
        .fold(0u64, u64::saturating_add);
    let album_artist = album
        .album_artist_id()
        .and_then(|artist_id| indexes.artists_by_id.get(artist_id).cloned());
    AlbumSummary::new(
        album.clone(),
        album_artist,
        track_count,
        duration_ms,
        album_created_at(indexes, album),
    )
    .with_state(album_state(indexes, album.id(), user))
}

fn artist_summary(indexes: &TrackIndexes, artist: &Artist, user: Option<Uuid>) -> ArtistSummary {
    ArtistSummary::new(artist.clone(), album_count(indexes, artist.id())).with_state(artist_state(
        indexes,
        artist.id(),
        user,
    ))
}

fn album_count(indexes: &TrackIndexes, artist_id: &ArtistId) -> u32 {
    indexes
        .albums_by_id
        .values()
        .filter(|album| album.album_artist_id() == Some(artist_id))
        .count() as u32
}

fn track_state(indexes: &TrackIndexes, track_id: &TrackId, user: Option<Uuid>) -> LibraryState {
    let Some(user) = user else {
        return LibraryState::default();
    };
    let starred_at = indexes
        .track_favorites
        .get(&(user, track_id.clone()))
        .copied();
    let rating = indexes
        .track_ratings
        .get(&(user, track_id.clone()))
        .copied();
    let (play_count, last_played_at) = indexes
        .track_plays
        .get(&(user, track_id.clone()))
        .copied()
        .unwrap_or((0, None));

    LibraryState::new(starred_at, rating, play_count, last_played_at)
        .expect("in-memory state is validated on write")
}

fn album_state(indexes: &TrackIndexes, album_id: &AlbumId, user: Option<Uuid>) -> LibraryState {
    let Some(user) = user else {
        return LibraryState::default();
    };
    let starred_at = indexes
        .album_favorites
        .get(&(user, album_id.clone()))
        .copied();
    let rating = indexes
        .album_ratings
        .get(&(user, album_id.clone()))
        .copied();
    let (play_count, last_played_at) = indexes
        .album_tracks
        .get(album_id)
        .map(|memberships| {
            memberships
                .iter()
                .fold((0u32, None), |(count, last), (track_id, _)| {
                    let (track_count, track_last) = indexes
                        .track_plays
                        .get(&(user, track_id.clone()))
                        .copied()
                        .unwrap_or((0, None));
                    (count.saturating_add(track_count), last.max(track_last))
                })
        })
        .unwrap_or((0, None));

    LibraryState::new(starred_at, rating, play_count, last_played_at)
        .expect("in-memory state is validated on write")
}

fn artist_state(indexes: &TrackIndexes, artist_id: &ArtistId, user: Option<Uuid>) -> LibraryState {
    let Some(user) = user else {
        return LibraryState::default();
    };
    let starred_at = indexes
        .artist_favorites
        .get(&(user, artist_id.clone()))
        .copied();
    let rating = indexes
        .artist_ratings
        .get(&(user, artist_id.clone()))
        .copied();
    let (play_count, last_played_at) = indexes
        .albums_by_id
        .values()
        .filter(|album| album.album_artist_id() == Some(artist_id))
        .fold((0u32, None), |(count, last), album| {
            let album_state = album_state(indexes, album.id(), Some(user));
            (
                count.saturating_add(album_state.play_count()),
                last.max(album_state.last_played_at()),
            )
        });

    LibraryState::new(starred_at, rating, play_count, last_played_at)
        .expect("in-memory state is validated on write")
}

fn album_matches_filter(
    indexes: &TrackIndexes,
    album: &Album,
    filter: &AlbumListFilter,
    user: Option<Uuid>,
) -> bool {
    if filter.is_starred()
        && !user.is_some_and(|user| {
            indexes
                .album_favorites
                .contains_key(&(user, album.id().clone()))
        })
    {
        return false;
    }

    if let Some(genre) = filter.genre() {
        let needle = normalize_for_matching(genre);
        let matches = ordered_memberships(indexes, album.id())
            .iter()
            .any(|(track_id, _)| {
                indexes
                    .by_track_id
                    .get(track_id)
                    .and_then(|entry| entry.track.metadata().genre())
                    .is_some_and(|track_genre| normalize_for_matching(track_genre) == needle)
            });
        if !matches {
            return false;
        }
    }

    if let Some(from) = filter.year_from()
        && album.year().is_none_or(|year| year < from)
    {
        return false;
    }
    if let Some(to) = filter.year_to()
        && album.year().is_none_or(|year| year > to)
    {
        return false;
    }

    true
}

fn genre_summaries(indexes: &TrackIndexes) -> Vec<GenreSummary> {
    let mut totals: HashMap<String, (String, u32, HashSet<AlbumId>)> = HashMap::new();

    for entry in indexes.by_track_id.values() {
        let Some(genre) = entry.track.metadata().genre() else {
            continue;
        };
        let key = normalize_for_matching(genre);
        let bucket = totals
            .entry(key)
            .or_insert_with(|| (genre.to_owned(), 0, HashSet::new()));
        bucket.1 = bucket.1.saturating_add(1);
        if let Some(album) = album_for_track(indexes, entry.track.id()) {
            bucket.2.insert(album.id().clone());
        }
    }

    let mut summaries = totals
        .into_values()
        .map(|(name, song_count, albums)| {
            GenreSummary::new(name, song_count, albums.len() as u32)
                .expect("a stored genre is valid")
        })
        .collect::<Vec<_>>();
    summaries.sort_by(|left, right| {
        normalize_for_matching(left.genre())
            .cmp(&normalize_for_matching(right.genre()))
            .then_with(|| left.genre().cmp(right.genre()))
    });

    summaries
}

fn favorite_ids<K: Clone + Eq + std::hash::Hash>(
    favorites: &HashMap<(Uuid, K), StarredAt>,
    user: Uuid,
) -> Vec<K> {
    let mut ids = favorites
        .iter()
        .filter(|((owner, _), _)| *owner == user)
        .map(|((_, id), starred_at)| (*starred_at, id.clone()))
        .collect::<Vec<_>>();
    ids.sort_by_key(|(starred_at, _)| std::cmp::Reverse(*starred_at));

    ids.into_iter().map(|(_, id)| id).collect()
}

fn set_favorite<K: Clone + Eq + std::hash::Hash>(
    favorites: &mut HashMap<(Uuid, K), StarredAt>,
    user: Uuid,
    id: K,
    starred: bool,
    now: StarredAt,
) {
    if starred {
        favorites.insert((user, id), now);
    } else {
        favorites.remove(&(user, id));
    }
}

fn set_rating_entry<K: Clone + Eq + std::hash::Hash>(
    ratings: &mut HashMap<(Uuid, K), u8>,
    user: Uuid,
    id: K,
    rating: Option<u8>,
) {
    match rating {
        Some(rating) => {
            ratings.insert((user, id), rating);
        }
        None => {
            ratings.remove(&(user, id));
        }
    }
}

fn resolve_artist(indexes: &mut TrackIndexes, credit: &ArtistCredit) -> Artist {
    if let Some(identifier) = credit.musicbrainz_artist_id()
        && let Some(artist_id) = indexes.artist_by_identifier.get(identifier)
        && let Some(artist) = indexes.artists_by_id.get(artist_id)
    {
        return artist.clone();
    }

    let normalized = normalize_for_matching(credit.name());

    if let Some(artist_id) = indexes.artist_by_name.get(&normalized).cloned() {
        let Some(artist) = indexes.artists_by_id.get(&artist_id).cloned() else {
            unreachable!("a name index only points at a stored artist");
        };

        let Some(identifier) = credit.musicbrainz_artist_id() else {
            return artist;
        };

        let attached = Artist::new(
            artist.id().clone(),
            artist.name().to_owned(),
            artist.sort_name().map(str::to_owned),
            Some(identifier.to_owned()),
        )
        .expect("a stored artist stays valid when an identifier is attached");

        indexes
            .artist_by_identifier
            .insert(identifier.to_owned(), attached.id().clone());
        indexes.artist_by_name.remove(&normalized);
        indexes
            .artists_by_id
            .insert(attached.id().clone(), attached.clone());

        return attached;
    }

    let artist = Artist::new(
        ArtistId::new(),
        credit.name().to_owned(),
        None,
        credit.musicbrainz_artist_id().map(str::to_owned),
    )
    .expect("a validated credit yields a valid artist");

    if let Some(identifier) = artist.musicbrainz_artist_id() {
        indexes
            .artist_by_identifier
            .insert(identifier.to_owned(), artist.id().clone());
    } else {
        indexes
            .artist_by_name
            .insert(normalized, artist.id().clone());
    }
    indexes
        .artists_by_id
        .insert(artist.id().clone(), artist.clone());

    artist
}

fn resolve_album(
    indexes: &mut TrackIndexes,
    metadata: &AlbumMetadata,
    album_artist_id: Option<ArtistId>,
) -> Album {
    if let Some(identifier) = metadata.musicbrainz_release_group_id()
        && let Some(album_id) = indexes.album_by_release_group.get(identifier)
        && let Some(album) = indexes.albums_by_id.get(album_id)
    {
        return album.clone();
    }

    if let Some(identifier) = metadata.musicbrainz_release_id()
        && let Some(album_id) = indexes.album_by_release.get(identifier)
        && let Some(album) = indexes.albums_by_id.get(album_id)
    {
        return album.clone();
    }

    let normalized = normalize_for_matching(metadata.title());
    let key = (album_artist_id.clone(), normalized);

    if let Some(album_id) = indexes.album_by_text.get(&key).cloned()
        && let Some(existing) = indexes.albums_by_id.get(&album_id).cloned()
    {
        let existing_has_identifier = existing.musicbrainz_release_group_id().is_some()
            || existing.musicbrainz_release_id().is_some();
        let incoming_has_identifier = metadata.musicbrainz_release_group_id().is_some()
            || metadata.musicbrainz_release_id().is_some();

        let both_identified = existing_has_identifier && incoming_has_identifier;

        if !both_identified {
            if !incoming_has_identifier {
                return existing;
            }

            let attached = Album::new(
                existing.id().clone(),
                existing.title().to_owned(),
                album_artist_id,
                metadata.year().or(existing.year()),
                preferred(metadata.release_type(), existing.release_type()),
                preferred(metadata.label(), existing.label()),
                preferred(
                    metadata.musicbrainz_release_group_id(),
                    existing.musicbrainz_release_group_id(),
                ),
                preferred(
                    metadata.musicbrainz_release_id(),
                    existing.musicbrainz_release_id(),
                ),
            )
            .expect("attaching an identifier keeps the album valid");

            if let Some(identifier) = attached.musicbrainz_release_group_id() {
                indexes
                    .album_by_release_group
                    .insert(identifier.to_owned(), attached.id().clone());
            }
            if let Some(identifier) = attached.musicbrainz_release_id() {
                indexes
                    .album_by_release
                    .insert(identifier.to_owned(), attached.id().clone());
            }
            indexes
                .albums_by_id
                .insert(attached.id().clone(), attached.clone());

            return attached;
        }
    }

    let album = Album::new(
        AlbumId::new(),
        metadata.title().to_owned(),
        album_artist_id,
        metadata.year(),
        metadata.release_type().map(str::to_owned),
        metadata.label().map(str::to_owned),
        metadata.musicbrainz_release_group_id().map(str::to_owned),
        metadata.musicbrainz_release_id().map(str::to_owned),
    )
    .expect("validated album metadata yields a valid album");

    indexes
        .album_by_text
        .entry(key)
        .or_insert(album.id().clone());
    if let Some(identifier) = album.musicbrainz_release_group_id() {
        indexes
            .album_by_release_group
            .insert(identifier.to_owned(), album.id().clone());
    }
    if let Some(identifier) = album.musicbrainz_release_id() {
        indexes
            .album_by_release
            .insert(identifier.to_owned(), album.id().clone());
    }
    indexes
        .albums_by_id
        .insert(album.id().clone(), album.clone());
    indexes
        .album_created_at
        .insert(album.id().clone(), chrono::Utc::now());

    album
}

fn preferred<'a>(incoming: Option<&'a str>, existing: Option<&'a str>) -> Option<String> {
    incoming.or(existing).map(str::to_owned)
}
