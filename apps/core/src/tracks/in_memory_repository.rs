use std::{
    collections::{HashMap, HashSet},
    sync::Mutex,
};

use async_trait::async_trait;
use uuid::Uuid;

use crate::tracks::{
    Album, AlbumEntry, AlbumId, AlbumListFilter, AlbumMetadata, AlbumOrder, AlbumSummary,
    AlbumTrack, Artist, ArtistCredit, ArtistId, ArtistSummary, ArtworkRef, GenreSummary,
    LibraryState, MatchQuality, ProviderId, ProviderKind, SearchTerms, SourceFileFacts, Starred,
    StateTarget, Track, TrackHit, TrackMetadata, checked_rating,
    domain::{album_quality, normalize_for_matching, track_quality},
};

use super::{SourceIdentity, SourceScope, TrackId, TrackRepository, TrackRepositoryError};

type StarredAt = chrono::DateTime<chrono::Utc>;

struct TrackEntry {
    track: Track,
    sources: Vec<SourceIdentity>,
}

#[derive(Default)]
struct TrackIndexes {
    by_source: HashMap<SourceIdentity, TrackId>,
    by_track_id: HashMap<TrackId, TrackEntry>,
    artwork_by_source: HashMap<SourceIdentity, ArtworkRef>,
    file_facts_by_source: HashMap<SourceIdentity, SourceFileFacts>,
    artists_by_id: HashMap<ArtistId, Artist>,
    artist_by_identifier: HashMap<String, ArtistId>,
    artist_by_name: HashMap<String, ArtistId>,
    albums_by_id: HashMap<AlbumId, Album>,
    album_by_release_group: HashMap<String, AlbumId>,
    album_by_release: HashMap<String, AlbumId>,
    album_by_text: HashMap<(Option<ArtistId>, String), AlbumId>,
    album_sources: HashMap<AlbumId, Vec<SourceIdentity>>,
    album_tracks: HashMap<AlbumId, Vec<(TrackId, AlbumTrack)>>,
    album_created_at: HashMap<AlbumId, chrono::DateTime<chrono::Utc>>,
    track_favorites: HashMap<(Uuid, TrackId), StarredAt>,
    album_favorites: HashMap<(Uuid, AlbumId), StarredAt>,
    artist_favorites: HashMap<(Uuid, ArtistId), StarredAt>,
    track_ratings: HashMap<(Uuid, TrackId), u8>,
    album_ratings: HashMap<(Uuid, AlbumId), u8>,
    artist_ratings: HashMap<(Uuid, ArtistId), u8>,
    track_plays: HashMap<(Uuid, TrackId), (u32, Option<StarredAt>)>,
}

#[derive(Default)]
pub struct InMemoryTrackRepository {
    indexes: Mutex<TrackIndexes>,
}

#[async_trait]
impl TrackRepository for InMemoryTrackRepository {
    async fn get_or_create(
        &self,
        source: SourceIdentity,
        metadata: TrackMetadata,
    ) -> Result<Track, TrackRepositoryError> {
        let mut indexes = self
            .indexes
            .lock()
            .map_err(|_| TrackRepositoryError::Unavailable)?;

        let track_id = match identified_track(&indexes, &metadata) {
            Some(identified) => {
                if let Some(previous) = indexes.by_source.get(&source).cloned()
                    && previous != identified
                {
                    detach_source(&mut indexes, &source, &previous);
                    drop_source_less_track(&mut indexes, &previous);
                }

                attach_source(&mut indexes, &source, &identified);

                identified
            }
            None => match indexes.by_source.get(&source).cloned() {
                Some(current) => current,
                None => {
                    let track_id = TrackId::new();
                    let track = Track::new(track_id.clone(), metadata.clone());

                    indexes.by_track_id.insert(
                        track_id.clone(),
                        TrackEntry {
                            track,
                            sources: vec![source.clone()],
                        },
                    );
                    indexes.by_source.insert(source, track_id.clone());

                    track_id
                }
            },
        };

        let entry = indexes
            .by_track_id
            .get_mut(&track_id)
            .ok_or(TrackRepositoryError::Unavailable)?;

        entry.track = Track::new(track_id, metadata);

        Ok(entry.track.clone())
    }

    async fn find_sources(
        &self,
        track_id: &TrackId,
    ) -> Result<Vec<SourceIdentity>, TrackRepositoryError> {
        let indexes = self
            .indexes
            .lock()
            .map_err(|_| TrackRepositoryError::Unavailable)?;

        Ok(indexes
            .by_track_id
            .get(track_id)
            .map(|entry| entry.sources.clone())
            .unwrap_or_default())
    }

    async fn find_track(&self, track_id: &TrackId) -> Result<Option<Track>, TrackRepositoryError> {
        let indexes = self
            .indexes
            .lock()
            .map_err(|_| TrackRepositoryError::Unavailable)?;

        Ok(indexes
            .by_track_id
            .get(track_id)
            .map(|entry| entry.track.clone()))
    }

    async fn find_track_state(
        &self,
        track_id: &TrackId,
        user: Uuid,
    ) -> Result<LibraryState, TrackRepositoryError> {
        let indexes = self
            .indexes
            .lock()
            .map_err(|_| TrackRepositoryError::Unavailable)?;

        Ok(track_state(&indexes, track_id, Some(user)))
    }

    async fn set_source_artwork(
        &self,
        source: &SourceIdentity,
        artwork: &ArtworkRef,
    ) -> Result<(), TrackRepositoryError> {
        let mut indexes = self
            .indexes
            .lock()
            .map_err(|_| TrackRepositoryError::Unavailable)?;

        indexes
            .artwork_by_source
            .insert(source.clone(), artwork.clone());
        Ok(())
    }

    async fn find_artwork(
        &self,
        track_id: &TrackId,
    ) -> Result<Option<(SourceIdentity, ArtworkRef)>, TrackRepositoryError> {
        let indexes = self
            .indexes
            .lock()
            .map_err(|_| TrackRepositoryError::Unavailable)?;

        let Some(entry) = indexes.by_track_id.get(track_id) else {
            return Ok(None);
        };

        Ok(entry
            .sources
            .iter()
            .filter_map(|source| {
                indexes
                    .artwork_by_source
                    .get(source)
                    .map(|artwork| (source, artwork))
            })
            .min_by_key(|(source, _)| source.provider_id().precedence())
            .map(|(source, artwork)| (source.clone(), artwork.clone())))
    }

    async fn set_source_file_facts(
        &self,
        source: &SourceIdentity,
        facts: &SourceFileFacts,
    ) -> Result<(), TrackRepositoryError> {
        let mut indexes = self
            .indexes
            .lock()
            .map_err(|_| TrackRepositoryError::Unavailable)?;

        indexes
            .file_facts_by_source
            .insert(source.clone(), facts.clone());
        Ok(())
    }

    async fn find_source_file_facts(
        &self,
        source: &SourceIdentity,
    ) -> Result<Option<SourceFileFacts>, TrackRepositoryError> {
        let indexes = self
            .indexes
            .lock()
            .map_err(|_| TrackRepositoryError::Unavailable)?;

        Ok(indexes.file_facts_by_source.get(source).cloned())
    }

    async fn list_scanned_paths(
        &self,
        provider_id: &ProviderId,
        source_scope: &SourceScope,
    ) -> Result<Vec<(SourceIdentity, SourceFileFacts)>, TrackRepositoryError> {
        let indexes = self
            .indexes
            .lock()
            .map_err(|_| TrackRepositoryError::Unavailable)?;

        Ok(indexes
            .file_facts_by_source
            .iter()
            .filter(|(source, _)| {
                source.provider_id() == provider_id && source.source_scope() == source_scope
            })
            .map(|(source, facts)| (source.clone(), facts.clone()))
            .collect())
    }

    async fn resolve_or_create_artist(
        &self,
        credit: &ArtistCredit,
    ) -> Result<Artist, TrackRepositoryError> {
        let mut indexes = self
            .indexes
            .lock()
            .map_err(|_| TrackRepositoryError::Unavailable)?;

        Ok(resolve_artist(&mut indexes, credit))
    }

    async fn resolve_or_create_album(
        &self,
        metadata: &AlbumMetadata,
    ) -> Result<Album, TrackRepositoryError> {
        let mut indexes = self
            .indexes
            .lock()
            .map_err(|_| TrackRepositoryError::Unavailable)?;

        let album_artist_id = metadata
            .album_artist()
            .map(|credit| resolve_artist(&mut indexes, credit).id().clone());

        Ok(resolve_album(&mut indexes, metadata, album_artist_id))
    }

    async fn set_album_source(
        &self,
        album_id: &AlbumId,
        source: &SourceIdentity,
    ) -> Result<(), TrackRepositoryError> {
        let mut indexes = self
            .indexes
            .lock()
            .map_err(|_| TrackRepositoryError::Unavailable)?;

        if !indexes.albums_by_id.contains_key(album_id) {
            return Err(TrackRepositoryError::InvalidData);
        }

        let sources = indexes.album_sources.entry(album_id.clone()).or_default();
        if !sources.contains(source) {
            sources.push(source.clone());
        }

        Ok(())
    }

    async fn set_album_track(
        &self,
        album_id: &AlbumId,
        track_id: &TrackId,
        membership: &AlbumTrack,
    ) -> Result<(), TrackRepositoryError> {
        let mut indexes = self
            .indexes
            .lock()
            .map_err(|_| TrackRepositoryError::Unavailable)?;

        if !indexes.albums_by_id.contains_key(album_id)
            || !indexes.by_track_id.contains_key(track_id)
        {
            return Err(TrackRepositoryError::InvalidData);
        }

        detach_track(&mut indexes.album_tracks, Some(album_id), track_id);

        let tracks = indexes.album_tracks.entry(album_id.clone()).or_default();
        match tracks.iter_mut().find(|(id, _)| id == track_id) {
            Some(entry) => entry.1 = *membership,
            None => tracks.push((track_id.clone(), *membership)),
        }

        Ok(())
    }

    async fn clear_album_track(&self, track_id: &TrackId) -> Result<(), TrackRepositoryError> {
        let mut indexes = self
            .indexes
            .lock()
            .map_err(|_| TrackRepositoryError::Unavailable)?;

        detach_track(&mut indexes.album_tracks, None, track_id);
        Ok(())
    }

    async fn find_album(&self, album_id: &AlbumId) -> Result<Option<Album>, TrackRepositoryError> {
        let indexes = self
            .indexes
            .lock()
            .map_err(|_| TrackRepositoryError::Unavailable)?;

        Ok(indexes.albums_by_id.get(album_id).cloned())
    }

    async fn find_album_for_track(
        &self,
        track_id: &TrackId,
    ) -> Result<Option<Album>, TrackRepositoryError> {
        let indexes = self
            .indexes
            .lock()
            .map_err(|_| TrackRepositoryError::Unavailable)?;

        Ok(album_for_track(&indexes, track_id))
    }

    async fn find_artist(
        &self,
        artist_id: &ArtistId,
        _user: Option<Uuid>,
    ) -> Result<Option<Artist>, TrackRepositoryError> {
        let indexes = self
            .indexes
            .lock()
            .map_err(|_| TrackRepositoryError::Unavailable)?;

        Ok(indexes.artists_by_id.get(artist_id).cloned())
    }

    async fn find_album_summary(
        &self,
        album_id: &AlbumId,
        user: Option<Uuid>,
    ) -> Result<Option<AlbumSummary>, TrackRepositoryError> {
        let indexes = self
            .indexes
            .lock()
            .map_err(|_| TrackRepositoryError::Unavailable)?;

        Ok(indexes
            .albums_by_id
            .get(album_id)
            .map(|album| album_summary(&indexes, album, user)))
    }

    async fn find_artist_state(
        &self,
        artist_id: &ArtistId,
        user: Uuid,
    ) -> Result<LibraryState, TrackRepositoryError> {
        let indexes = self
            .indexes
            .lock()
            .map_err(|_| TrackRepositoryError::Unavailable)?;

        Ok(artist_state(&indexes, artist_id, Some(user)))
    }

    async fn find_album_sources(
        &self,
        album_id: &AlbumId,
    ) -> Result<Vec<SourceIdentity>, TrackRepositoryError> {
        let indexes = self
            .indexes
            .lock()
            .map_err(|_| TrackRepositoryError::Unavailable)?;

        Ok(indexes
            .album_sources
            .get(album_id)
            .cloned()
            .unwrap_or_default())
    }

    async fn find_album_entries(
        &self,
        album_id: &AlbumId,
        user: Option<Uuid>,
    ) -> Result<Vec<AlbumEntry>, TrackRepositoryError> {
        let indexes = self
            .indexes
            .lock()
            .map_err(|_| TrackRepositoryError::Unavailable)?;

        let entries = ordered_memberships(&indexes, album_id)
            .into_iter()
            .filter_map(|(track_id, membership)| {
                indexes.by_track_id.get(&track_id).map(|entry| {
                    AlbumEntry::new(entry.track.clone(), membership)
                        .with_state(track_state(&indexes, &track_id, user))
                })
            })
            .collect();

        Ok(entries)
    }

    async fn find_albums_for_artist(
        &self,
        artist_id: &ArtistId,
        user: Option<Uuid>,
    ) -> Result<Vec<AlbumSummary>, TrackRepositoryError> {
        let indexes = self
            .indexes
            .lock()
            .map_err(|_| TrackRepositoryError::Unavailable)?;

        Ok(ordered_albums(
            &indexes,
            |album| album.album_artist_id() == Some(artist_id),
            AlbumOrder::Title,
            user,
        )
        .into_iter()
        .map(|album| album_summary(&indexes, album, user))
        .collect())
    }

    async fn list_albums(
        &self,
        offset: u32,
        limit: u32,
        order: AlbumOrder,
        filter: AlbumListFilter,
        user: Option<Uuid>,
    ) -> Result<Vec<AlbumSummary>, TrackRepositoryError> {
        let indexes = self
            .indexes
            .lock()
            .map_err(|_| TrackRepositoryError::Unavailable)?;

        Ok(ordered_albums(
            &indexes,
            |album| album_matches_filter(&indexes, album, &filter, user),
            order,
            user,
        )
        .into_iter()
        .skip(offset as usize)
        .take(limit as usize)
        .map(|album| album_summary(&indexes, album, user))
        .collect())
    }

    async fn list_artists(
        &self,
        offset: u32,
        limit: u32,
        user: Option<Uuid>,
    ) -> Result<Vec<ArtistSummary>, TrackRepositoryError> {
        let indexes = self
            .indexes
            .lock()
            .map_err(|_| TrackRepositoryError::Unavailable)?;

        let mut artists = indexes.artists_by_id.values().collect::<Vec<_>>();
        artists.sort_by(|left, right| {
            normalize_for_matching(left.name())
                .cmp(&normalize_for_matching(right.name()))
                .then_with(|| left.id().to_string().cmp(&right.id().to_string()))
        });

        Ok(artists
            .into_iter()
            .skip(offset as usize)
            .take(limit as usize)
            .map(|artist| artist_summary(&indexes, artist, user))
            .collect())
    }

    async fn count_albums(&self) -> Result<u32, TrackRepositoryError> {
        let indexes = self
            .indexes
            .lock()
            .map_err(|_| TrackRepositoryError::Unavailable)?;

        Ok(indexes.albums_by_id.len() as u32)
    }

    async fn count_artists(&self) -> Result<u32, TrackRepositoryError> {
        let indexes = self
            .indexes
            .lock()
            .map_err(|_| TrackRepositoryError::Unavailable)?;

        Ok(indexes.artists_by_id.len() as u32)
    }

    async fn search_tracks(
        &self,
        query: &str,
        limit: u32,
        user: Option<Uuid>,
    ) -> Result<Vec<TrackHit>, TrackRepositoryError> {
        let indexes = self
            .indexes
            .lock()
            .map_err(|_| TrackRepositoryError::Unavailable)?;
        let terms = SearchTerms::new(query);

        if terms.is_empty() {
            return Ok(Vec::new());
        }

        let mut ranked = indexes
            .by_track_id
            .values()
            .filter_map(|entry| {
                track_quality(&terms, entry.track.title(), entry.track.artists())
                    .map(|quality| (quality, entry))
            })
            .collect::<Vec<_>>();

        ranked.sort_by(|(left_quality, left), (right_quality, right)| {
            left_quality
                .cmp(right_quality)
                .then_with(|| owned(&right.sources).cmp(&owned(&left.sources)))
                .then_with(|| {
                    normalize_for_matching(left.track.title())
                        .cmp(&normalize_for_matching(right.track.title()))
                })
                .then_with(|| {
                    left.track
                        .id()
                        .to_string()
                        .cmp(&right.track.id().to_string())
                })
        });
        ranked.truncate(limit as usize);

        Ok(ranked
            .into_iter()
            .map(|(quality, entry)| {
                TrackHit::new(
                    entry.track.clone(),
                    album_for_track(&indexes, entry.track.id()),
                    entry.sources.clone(),
                )
                .with_state(track_state(&indexes, entry.track.id(), user))
                .with_match_quality(Some(quality))
            })
            .collect())
    }

    async fn search_albums(
        &self,
        query: &str,
        limit: u32,
        user: Option<Uuid>,
    ) -> Result<Vec<AlbumSummary>, TrackRepositoryError> {
        let indexes = self
            .indexes
            .lock()
            .map_err(|_| TrackRepositoryError::Unavailable)?;
        let terms = SearchTerms::new(query);

        if terms.is_empty() {
            return Ok(Vec::new());
        }

        let mut ranked = indexes
            .albums_by_id
            .values()
            .filter_map(|album| {
                let album_artist = album
                    .album_artist_id()
                    .and_then(|artist_id| indexes.artists_by_id.get(artist_id))
                    .map(Artist::name);
                album_quality(&terms, album.title(), album_artist).map(|quality| (quality, album))
            })
            .collect::<Vec<_>>();

        ranked.sort_by(|(left_quality, left), (right_quality, right)| {
            left_quality
                .cmp(right_quality)
                .then_with(|| {
                    normalize_for_matching(left.title()).cmp(&normalize_for_matching(right.title()))
                })
                .then_with(|| left.id().to_string().cmp(&right.id().to_string()))
        });
        ranked.truncate(limit as usize);

        Ok(ranked
            .into_iter()
            .map(|(quality, album)| {
                album_summary(&indexes, album, user).with_match_quality(Some(quality))
            })
            .collect())
    }

    async fn search_artists(
        &self,
        query: &str,
        limit: u32,
        user: Option<Uuid>,
    ) -> Result<Vec<ArtistSummary>, TrackRepositoryError> {
        let indexes = self
            .indexes
            .lock()
            .map_err(|_| TrackRepositoryError::Unavailable)?;
        let terms = SearchTerms::new(query);

        if terms.is_empty() {
            return Ok(Vec::new());
        }

        let mut by_name: HashMap<String, (MatchQuality, &Artist)> = HashMap::new();
        for artist in indexes.artists_by_id.values() {
            let Some(quality) = terms.classify(artist.name()) else {
                continue;
            };
            let key = normalize_for_matching(artist.name());
            by_name
                .entry(key)
                .and_modify(|current| {
                    if preferred_artist(&indexes, artist, current.1) {
                        *current = (quality, artist);
                    }
                })
                .or_insert((quality, artist));
        }

        let mut ranked = by_name.into_values().collect::<Vec<_>>();
        ranked.sort_by(|(left_quality, left), (right_quality, right)| {
            left_quality
                .cmp(right_quality)
                .then_with(|| {
                    normalize_for_matching(left.name()).cmp(&normalize_for_matching(right.name()))
                })
                .then_with(|| left.id().to_string().cmp(&right.id().to_string()))
        });
        ranked.truncate(limit as usize);

        Ok(ranked
            .into_iter()
            .map(|(quality, artist)| {
                artist_summary(&indexes, artist, user).with_match_quality(Some(quality))
            })
            .collect())
    }

    async fn candidate_tracks(
        &self,
        titles: &[String],
        limit: u32,
    ) -> Result<Vec<TrackHit>, TrackRepositoryError> {
        let indexes = self
            .indexes
            .lock()
            .map_err(|_| TrackRepositoryError::Unavailable)?;
        let keys = titles
            .iter()
            .map(|title| normalize_for_matching(title))
            .filter(|key| !key.is_empty())
            .collect::<HashSet<_>>();

        if keys.is_empty() {
            return Ok(Vec::new());
        }

        let mut hits = indexes
            .by_track_id
            .values()
            .filter(|entry| keys.contains(&normalize_for_matching(entry.track.title())))
            .map(|entry| {
                TrackHit::new(
                    entry.track.clone(),
                    album_for_track(&indexes, entry.track.id()),
                    entry.sources.clone(),
                )
            })
            .collect::<Vec<_>>();
        hits.sort_by(|left, right| {
            normalize_for_matching(left.track().title())
                .cmp(&normalize_for_matching(right.track().title()))
                .then_with(|| {
                    left.track()
                        .id()
                        .to_string()
                        .cmp(&right.track().id().to_string())
                })
        });
        hits.truncate(limit as usize);

        Ok(hits)
    }

    async fn list_genres(&self) -> Result<Vec<GenreSummary>, TrackRepositoryError> {
        let indexes = self
            .indexes
            .lock()
            .map_err(|_| TrackRepositoryError::Unavailable)?;

        Ok(genre_summaries(&indexes))
    }

    async fn starred(&self, user: Uuid) -> Result<Starred, TrackRepositoryError> {
        let indexes = self
            .indexes
            .lock()
            .map_err(|_| TrackRepositoryError::Unavailable)?;

        let hits = favorite_ids(&indexes.track_favorites, user)
            .into_iter()
            .filter_map(|track_id| {
                indexes.by_track_id.get(&track_id).map(|entry| {
                    TrackHit::new(
                        entry.track.clone(),
                        album_for_track(&indexes, &track_id),
                        entry.sources.clone(),
                    )
                    .with_state(track_state(&indexes, &track_id, Some(user)))
                })
            })
            .collect();

        let albums = favorite_ids(&indexes.album_favorites, user)
            .into_iter()
            .filter_map(|album_id| indexes.albums_by_id.get(&album_id))
            .map(|album| album_summary(&indexes, album, Some(user)))
            .collect();

        let artists = favorite_ids(&indexes.artist_favorites, user)
            .into_iter()
            .filter_map(|artist_id| indexes.artists_by_id.get(&artist_id))
            .map(|artist| artist_summary(&indexes, artist, Some(user)))
            .collect();

        Ok(Starred::new(hits, albums, artists))
    }

    async fn set_starred(
        &self,
        user: Uuid,
        target: StateTarget<'_>,
        starred: bool,
    ) -> Result<(), TrackRepositoryError> {
        let mut indexes = self
            .indexes
            .lock()
            .map_err(|_| TrackRepositoryError::Unavailable)?;
        let now = chrono::Utc::now();

        match target {
            StateTarget::Track(track_id) => set_favorite(
                &mut indexes.track_favorites,
                user,
                track_id.clone(),
                starred,
                now,
            ),
            StateTarget::Album(album_id) => set_favorite(
                &mut indexes.album_favorites,
                user,
                album_id.clone(),
                starred,
                now,
            ),
            StateTarget::Artist(artist_id) => set_favorite(
                &mut indexes.artist_favorites,
                user,
                artist_id.clone(),
                starred,
                now,
            ),
        }

        Ok(())
    }

    async fn set_rating(
        &self,
        user: Uuid,
        target: StateTarget<'_>,
        rating: Option<u8>,
    ) -> Result<(), TrackRepositoryError> {
        let mut indexes = self
            .indexes
            .lock()
            .map_err(|_| TrackRepositoryError::Unavailable)?;
        let rating = checked_rating(rating).map_err(|_| TrackRepositoryError::InvalidData)?;

        match target {
            StateTarget::Track(track_id) => {
                set_rating_entry(&mut indexes.track_ratings, user, track_id.clone(), rating);
            }
            StateTarget::Album(album_id) => {
                set_rating_entry(&mut indexes.album_ratings, user, album_id.clone(), rating);
            }
            StateTarget::Artist(artist_id) => {
                set_rating_entry(&mut indexes.artist_ratings, user, artist_id.clone(), rating);
            }
        }

        Ok(())
    }

    async fn record_play(
        &self,
        user: Uuid,
        track_id: &TrackId,
        played_at: chrono::DateTime<chrono::Utc>,
    ) -> Result<(), TrackRepositoryError> {
        let mut indexes = self
            .indexes
            .lock()
            .map_err(|_| TrackRepositoryError::Unavailable)?;

        let entry = indexes
            .track_plays
            .entry((user, track_id.clone()))
            .or_insert((0, None));
        entry.0 = entry.0.saturating_add(1);
        entry.1 = Some(played_at);

        Ok(())
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tracks::{SourceScope, TrackMetadata};

    fn source(provider: &str, external_id: &str) -> SourceIdentity {
        SourceIdentity::new(
            ProviderId::new(provider.to_owned()).unwrap(),
            SourceScope::Global,
            external_id.to_owned(),
        )
        .unwrap()
    }

    fn metadata(title: &str) -> TrackMetadata {
        TrackMetadata::from_names(title.to_owned(), vec!["Artist".to_owned()], Some(1_000)).unwrap()
    }

    fn identified(title: &str, recording_id: &str) -> TrackMetadata {
        metadata(title)
            .with_recording_id(Some(recording_id.to_owned()))
            .unwrap()
    }

    fn with_isrc(title: &str, isrc: &str) -> TrackMetadata {
        metadata(title).with_isrc(Some(isrc.to_owned())).unwrap()
    }

    #[tokio::test]
    async fn repository_maintains_stable_bidirectional_mappings() {
        let repository = InMemoryTrackRepository::default();
        let first_source = source("youtube_music", "source-a");
        let second_source = source("youtube_music", "source-b");

        let first_track = repository
            .get_or_create(first_source.clone(), metadata("First"))
            .await
            .unwrap();
        let repeated_track = repository
            .get_or_create(first_source.clone(), metadata("Updated"))
            .await
            .unwrap();
        let second_track = repository
            .get_or_create(second_source, metadata("Second"))
            .await
            .unwrap();

        assert_eq!(first_track.id(), repeated_track.id());
        assert_ne!(first_track.id(), second_track.id());
        assert_eq!(repeated_track.title(), "Updated");
        assert_eq!(
            repository.find_sources(first_track.id()).await.unwrap(),
            vec![first_source]
        );
    }

    #[tokio::test]
    async fn two_files_of_one_recording_share_a_track() {
        let repository = InMemoryTrackRepository::default();
        let first = source("local", "album-a/01 song.flac");
        let second = source("local", "album-b/01 song.flac");

        let first_track = repository
            .get_or_create(first.clone(), identified("Song", "rec-1"))
            .await
            .unwrap();
        let second_track = repository
            .get_or_create(second.clone(), identified("Song", "rec-1"))
            .await
            .unwrap();

        assert_eq!(first_track.id(), second_track.id());
        assert_eq!(
            repository.find_sources(first_track.id()).await.unwrap(),
            vec![first, second]
        );
    }

    #[tokio::test]
    async fn a_moved_file_keeps_its_track() {
        let repository = InMemoryTrackRepository::default();

        let before = repository
            .get_or_create(
                source("local", "old/01 song.flac"),
                identified("Song", "rec-1"),
            )
            .await
            .unwrap();
        let after = repository
            .get_or_create(
                source("local", "new/01 song.flac"),
                identified("Song", "rec-1"),
            )
            .await
            .unwrap();

        assert_eq!(before.id(), after.id());
        assert!(
            repository
                .find_sources(before.id())
                .await
                .unwrap()
                .contains(&source("local", "new/01 song.flac")),
            "the new path is a source of the surviving track"
        );
    }

    #[tokio::test]
    async fn an_unidentified_file_keeps_its_own_track() {
        let repository = InMemoryTrackRepository::default();

        let first = repository
            .get_or_create(source("local", "a.flac"), metadata("Song"))
            .await
            .unwrap();
        let second = repository
            .get_or_create(source("local", "b.flac"), metadata("Song"))
            .await
            .unwrap();

        assert_ne!(
            first.id(),
            second.id(),
            "a file with no identifier cannot be matched to another"
        );
    }

    #[tokio::test]
    async fn gaining_an_identifier_already_held_moves_the_source() {
        let repository = InMemoryTrackRepository::default();
        let owned = source("local", "album-a/01 song.flac");

        let identified_track = repository
            .get_or_create(
                source("local", "album-b/01 song.flac"),
                identified("Song", "rec-1"),
            )
            .await
            .unwrap();
        let plain_track = repository
            .get_or_create(owned.clone(), metadata("Song"))
            .await
            .unwrap();
        assert_ne!(plain_track.id(), identified_track.id());

        let merged = repository
            .get_or_create(owned.clone(), identified("Song", "rec-1"))
            .await
            .unwrap();

        assert_eq!(merged.id(), identified_track.id());
        assert_eq!(
            repository.find_sources(merged.id()).await.unwrap(),
            vec![source("local", "album-b/01 song.flac"), owned]
        );
        assert!(
            repository
                .find_track(plain_track.id())
                .await
                .unwrap()
                .is_none(),
            "the vacated track had no other source"
        );
    }

    #[tokio::test]
    async fn an_isrc_matches_only_when_it_is_unambiguous() {
        let repository = InMemoryTrackRepository::default();

        let first = repository
            .get_or_create(source("local", "a.flac"), with_isrc("Song", "ISRC-1"))
            .await
            .unwrap();
        let second = repository
            .get_or_create(source("local", "b.flac"), with_isrc("Song", "ISRC-1"))
            .await
            .unwrap();
        assert_eq!(first.id(), second.id());

        let ambiguous = repository
            .get_or_create(source("local", "c.flac"), with_isrc("Song", "ISRC-1"))
            .await
            .unwrap();
        assert_eq!(ambiguous.id(), first.id());
    }

    #[tokio::test]
    async fn a_recording_id_outranks_an_isrc() {
        let repository = InMemoryTrackRepository::default();

        let by_isrc = repository
            .get_or_create(source("local", "a.flac"), with_isrc("Song", "ISRC-1"))
            .await
            .unwrap();
        let by_recording = repository
            .get_or_create(source("local", "b.flac"), identified("Song", "rec-1"))
            .await
            .unwrap();
        assert_ne!(by_isrc.id(), by_recording.id());

        let both = metadata("Song")
            .with_recording_id(Some("rec-1".to_owned()))
            .unwrap()
            .with_isrc(Some("ISRC-1".to_owned()))
            .unwrap();
        let resolved = repository
            .get_or_create(source("local", "c.flac"), both)
            .await
            .unwrap();

        assert_eq!(resolved.id(), by_recording.id());
    }

    #[tokio::test]
    async fn repository_returns_a_stored_track_by_id() {
        let repository = InMemoryTrackRepository::default();
        let stored = repository
            .get_or_create(source("youtube_music", "source-a"), metadata("First"))
            .await
            .unwrap();

        let found = repository
            .find_track(stored.id())
            .await
            .unwrap()
            .expect("stored track is found");

        assert_eq!(found, stored);
        assert!(
            repository
                .find_track(&TrackId::new())
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn repository_returns_no_sources_for_an_unknown_track() {
        let repository = InMemoryTrackRepository::default();

        assert!(
            repository
                .find_sources(&TrackId::new())
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn artwork_resolves_from_the_source_that_stored_it() {
        let repository = InMemoryTrackRepository::default();
        let source = source("youtube_music", "vid-1");

        let track = repository
            .get_or_create(source.clone(), metadata("Song"))
            .await
            .unwrap();

        repository
            .set_source_artwork(
                &source,
                &ArtworkRef::url("https://art.test/ytm.jpg".to_owned()).unwrap(),
            )
            .await
            .unwrap();

        let (owner, artwork) = repository
            .find_artwork(track.id())
            .await
            .unwrap()
            .expect("artwork is stored");
        assert_eq!(owner.provider_id().as_str(), "youtube_music");
        assert_eq!(
            artwork,
            ArtworkRef::url("https://art.test/ytm.jpg".to_owned()).unwrap()
        );
    }

    #[tokio::test]
    async fn recording_identifiers_round_trip_on_the_track() {
        let repository = InMemoryTrackRepository::default();
        let stored = repository
            .get_or_create(
                source("local", "recording-1"),
                metadata("Song")
                    .with_recording_id(Some("mb-recording".to_owned()))
                    .unwrap()
                    .with_isrc(Some("ISRC-1".to_owned()))
                    .unwrap(),
            )
            .await
            .unwrap();

        let found = repository
            .find_track(stored.id())
            .await
            .unwrap()
            .expect("stored track is found");
        assert_eq!(found.musicbrainz_recording_id(), Some("mb-recording"));
        assert_eq!(found.isrc(), Some("ISRC-1"));
    }

    fn credit(name: &str, identifier: Option<&str>) -> ArtistCredit {
        ArtistCredit::new(name.to_owned(), identifier.map(str::to_owned)).unwrap()
    }

    fn album_metadata(title: &str) -> AlbumMetadata {
        AlbumMetadata::new(title.to_owned()).unwrap()
    }

    #[tokio::test]
    async fn artist_ladder_prefers_identifiers_then_normalised_names() {
        let repository = InMemoryTrackRepository::default();

        let first = repository
            .resolve_or_create_artist(&credit("Daft Punk", None))
            .await
            .unwrap();
        let again = repository
            .resolve_or_create_artist(&credit("  daft   punk ", None))
            .await
            .unwrap();
        assert_eq!(first.id(), again.id(), "normalised names match");
        assert_eq!(again.musicbrainz_artist_id(), None);

        let by_name = repository
            .resolve_or_create_artist(&credit("Daft Punk", Some("mb-1")))
            .await
            .unwrap();
        assert_eq!(by_name.id(), first.id(), "an id attaches to the name row");
        assert_eq!(by_name.musicbrainz_artist_id(), Some("mb-1"));

        let by_id = repository
            .resolve_or_create_artist(&credit("Someone Else", Some("mb-1")))
            .await
            .unwrap();
        assert_eq!(by_id.id(), first.id(), "the id wins over the name");
    }

    #[tokio::test]
    async fn an_id_bearing_and_a_name_only_artist_coexist() {
        let repository = InMemoryTrackRepository::default();

        let named = repository
            .resolve_or_create_artist(&credit("Daft Punk", Some("mb-1")))
            .await
            .unwrap();
        let plain = repository
            .resolve_or_create_artist(&credit("Daft Punk", None))
            .await
            .unwrap();

        assert_ne!(named.id(), plain.id());
        assert_eq!(named.musicbrainz_artist_id(), Some("mb-1"));
        assert_eq!(plain.musicbrainz_artist_id(), None);
    }

    #[tokio::test]
    async fn album_ladder_matches_identifiers_then_normalised_text() {
        let repository = InMemoryTrackRepository::default();
        let artist = credit("Daft Punk", None);

        let first = repository
            .resolve_or_create_album(&album_metadata("Discovery").with_album_artist(artist.clone()))
            .await
            .unwrap();
        let same_text = repository
            .resolve_or_create_album(&album_metadata("  discovery ").with_album_artist(artist))
            .await
            .unwrap();
        assert_eq!(first.id(), same_text.id());

        let by_group = repository
            .resolve_or_create_album(
                &album_metadata("Totally Different")
                    .with_release_group_id(Some("rg-1".to_owned()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let same_group = repository
            .resolve_or_create_album(
                &album_metadata("Also Different")
                    .with_release_group_id(Some("rg-1".to_owned()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            by_group.id(),
            same_group.id(),
            "the group id wins over text"
        );
    }

    #[tokio::test]
    async fn an_identifier_attaches_to_a_text_only_album() {
        let repository = InMemoryTrackRepository::default();

        let plain = repository
            .resolve_or_create_album(&album_metadata("Discovery"))
            .await
            .unwrap();
        assert_eq!(plain.musicbrainz_release_group_id(), None);

        let discovered = repository
            .resolve_or_create_album(
                &album_metadata("Discovery")
                    .with_release_group_id(Some("rg-1".to_owned()))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(discovered.id(), plain.id());
        assert_eq!(discovered.musicbrainz_release_group_id(), Some("rg-1"));
        assert_eq!(
            repository
                .find_album(plain.id())
                .await
                .unwrap()
                .unwrap()
                .musicbrainz_release_group_id(),
            Some("rg-1")
        );
    }

    #[tokio::test]
    async fn two_identifiers_are_two_albums_even_when_the_text_agrees() {
        let repository = InMemoryTrackRepository::default();

        let original = repository
            .resolve_or_create_album(
                &album_metadata("Discovery")
                    .with_release_group_id(Some("rg-original".to_owned()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let remaster = repository
            .resolve_or_create_album(
                &album_metadata("Discovery")
                    .with_release_group_id(Some("rg-remaster".to_owned()))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_ne!(original.id(), remaster.id());
    }

    #[tokio::test]
    async fn album_sources_and_membership_round_trip() {
        let repository = InMemoryTrackRepository::default();
        let album = repository
            .resolve_or_create_album(&album_metadata("Discovery"))
            .await
            .unwrap();
        let track = repository
            .get_or_create(source("local", "path-a"), metadata("Song"))
            .await
            .unwrap();
        let album_source = source("youtube_music", "album-1");
        let membership = AlbumTrack::new(Some(1), Some(3), 2).unwrap();

        repository
            .set_album_source(album.id(), &album_source)
            .await
            .unwrap();
        repository
            .set_album_source(album.id(), &album_source)
            .await
            .unwrap();
        assert_eq!(
            repository.find_album_sources(album.id()).await.unwrap(),
            vec![album_source]
        );

        repository
            .set_album_track(album.id(), track.id(), &membership)
            .await
            .unwrap();
        let entries = repository
            .find_album_entries(album.id(), None)
            .await
            .unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].track().id(), track.id());
        assert_eq!(entries[0].membership(), &membership);
        assert_eq!(
            repository
                .find_album_for_track(track.id())
                .await
                .unwrap()
                .expect("the track is filed under the album")
                .id(),
            album.id()
        );

        assert!(
            repository
                .set_album_track(
                    &AlbumId::new(),
                    track.id(),
                    &AlbumTrack::new(None, None, 0).unwrap(),
                )
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn re_filing_a_track_replaces_its_membership() {
        let repository = InMemoryTrackRepository::default();
        let first = repository
            .resolve_or_create_album(&album_metadata("First"))
            .await
            .unwrap();
        let second = repository
            .resolve_or_create_album(&album_metadata("Second"))
            .await
            .unwrap();
        let track = repository
            .get_or_create(source("local", "path-a"), metadata("Song"))
            .await
            .unwrap();
        let membership = AlbumTrack::new(Some(1), Some(1), 0).unwrap();

        repository
            .set_album_track(first.id(), track.id(), &membership)
            .await
            .unwrap();
        repository
            .set_album_track(second.id(), track.id(), &membership)
            .await
            .unwrap();

        assert_eq!(
            repository
                .find_album_for_track(track.id())
                .await
                .unwrap()
                .expect("the track is filed once")
                .id(),
            second.id()
        );
        assert!(
            repository
                .find_album_entries(first.id(), None)
                .await
                .unwrap()
                .is_empty(),
            "the previous album no longer holds the track"
        );
        assert_eq!(
            repository
                .find_album_entries(second.id(), None)
                .await
                .unwrap()
                .len(),
            1
        );

        repository.clear_album_track(track.id()).await.unwrap();
        assert!(
            repository
                .find_album_for_track(track.id())
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            repository
                .find_album_entries(second.id(), None)
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn album_entries_are_ordered_and_totalled() {
        let repository = InMemoryTrackRepository::default();
        let artist = repository
            .resolve_or_create_artist(&credit("Daft Punk", None))
            .await
            .unwrap();
        let album = repository
            .resolve_or_create_album(
                &album_metadata("Discovery").with_album_artist(credit("Daft Punk", None)),
            )
            .await
            .unwrap();

        let third = repository
            .get_or_create(source("local", "path-c"), metadata("Third"))
            .await
            .unwrap();
        let first = repository
            .get_or_create(source("local", "path-a"), metadata("First"))
            .await
            .unwrap();
        let second = repository
            .get_or_create(source("local", "path-b"), metadata("Second"))
            .await
            .unwrap();

        for (track, membership) in [
            (&third, AlbumTrack::new(Some(1), Some(3), 2).unwrap()),
            (&first, AlbumTrack::new(Some(1), Some(1), 0).unwrap()),
            (&second, AlbumTrack::new(None, None, 0).unwrap()),
        ] {
            repository
                .set_album_track(album.id(), track.id(), &membership)
                .await
                .unwrap();
        }

        let titles = repository
            .find_album_entries(album.id(), None)
            .await
            .unwrap()
            .iter()
            .map(|entry| entry.track().title().to_owned())
            .collect::<Vec<_>>();
        assert_eq!(
            titles,
            ["First", "Third", "Second"],
            "numbered tracks come first in track order, unnumbered last"
        );

        let summaries = repository
            .list_albums(0, 10, AlbumOrder::Title, AlbumListFilter::default(), None)
            .await
            .unwrap();
        assert_eq!(summaries.len(), 1);
        assert_eq!(summaries[0].album().id(), album.id());
        assert_eq!(summaries[0].album_artist().unwrap().id(), artist.id());
        assert_eq!(summaries[0].track_count(), 3);
        assert_eq!(summaries[0].duration_ms(), 3_000);

        let albums = repository
            .find_albums_for_artist(artist.id(), None)
            .await
            .unwrap();
        assert_eq!(albums.len(), 1);
        assert_eq!(albums[0].album().id(), album.id());

        let artists = repository.list_artists(0, 10, None).await.unwrap();
        assert_eq!(artists.len(), 1);
        assert_eq!(artists[0].artist().id(), artist.id());
        assert_eq!(artists[0].album_count(), 1);
    }

    #[tokio::test]
    async fn search_matches_titles_and_artists_and_ranks_owned_first() {
        let repository = InMemoryTrackRepository::default();
        let owned = repository
            .get_or_create(
                source("local", "path-a"),
                TrackMetadata::from_names(
                    "Zebra".to_owned(),
                    vec!["Daft Punk".to_owned()],
                    Some(1_000),
                )
                .unwrap(),
            )
            .await
            .unwrap();
        let remote = repository
            .get_or_create(
                source("youtube_music", "vid-1"),
                TrackMetadata::from_names(
                    "Apple".to_owned(),
                    vec!["Daft Punk".to_owned()],
                    Some(1_000),
                )
                .unwrap(),
            )
            .await
            .unwrap();

        let by_artist = repository
            .search_tracks("daft punk", 10, None)
            .await
            .unwrap();
        assert_eq!(
            by_artist
                .iter()
                .map(|hit| hit.track().id().clone())
                .collect::<Vec<_>>(),
            vec![owned.id().clone(), remote.id().clone()],
            "the owned track outranks the remote one despite the title"
        );

        let by_title = repository.search_tracks("zeb", 10, None).await.unwrap();
        assert_eq!(by_title.len(), 1);
        assert_eq!(by_title[0].track().id(), owned.id());
        assert_eq!(by_title[0].sources().len(), 1);

        assert!(
            repository
                .search_tracks("nothing here", 10, None)
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn search_ranks_match_quality_before_ownership() {
        let repository = InMemoryTrackRepository::default();
        let exact = repository
            .get_or_create(
                source("local", "path-exact"),
                TrackMetadata::from_names(
                    "Zebra".to_owned(),
                    vec!["Daft Punk".to_owned()],
                    Some(1_000),
                )
                .unwrap(),
            )
            .await
            .unwrap();
        let prefix = repository
            .get_or_create(
                source("local", "path-prefix"),
                TrackMetadata::from_names(
                    "Zebra Crossing".to_owned(),
                    vec!["Other".to_owned()],
                    Some(1_000),
                )
                .unwrap(),
            )
            .await
            .unwrap();
        let word = repository
            .get_or_create(
                source("youtube_music", "vid-word"),
                TrackMetadata::from_names(
                    "The Zebra".to_owned(),
                    vec!["Other".to_owned()],
                    Some(1_000),
                )
                .unwrap(),
            )
            .await
            .unwrap();

        let hits = repository.search_tracks("zebra", 10, None).await.unwrap();
        let ordered = hits
            .iter()
            .map(|hit| (hit.track().id().clone(), hit.match_quality()))
            .collect::<Vec<_>>();

        assert_eq!(
            ordered,
            vec![
                (exact.id().clone(), Some(MatchQuality::Exact)),
                (prefix.id().clone(), Some(MatchQuality::Prefix)),
                (word.id().clone(), Some(MatchQuality::WordBoundary)),
            ],
            "an owned prefix does not outrank a remote word-boundary match"
        );
    }

    #[tokio::test]
    async fn search_uses_ownership_inside_a_tier() {
        let repository = InMemoryTrackRepository::default();
        let remote = repository
            .get_or_create(
                source("youtube_music", "vid-exact"),
                TrackMetadata::from_names(
                    "Zebra".to_owned(),
                    vec!["Other".to_owned()],
                    Some(1_000),
                )
                .unwrap(),
            )
            .await
            .unwrap();
        let owned = repository
            .get_or_create(
                source("local", "path-exact"),
                TrackMetadata::from_names(
                    "Zebra".to_owned(),
                    vec!["Other".to_owned()],
                    Some(1_000),
                )
                .unwrap(),
            )
            .await
            .unwrap();

        let hits = repository.search_tracks("zebra", 10, None).await.unwrap();
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].track().id(), owned.id());
        assert_eq!(hits[1].track().id(), remote.id());
    }

    #[tokio::test]
    async fn search_does_not_match_inside_a_word() {
        let repository = InMemoryTrackRepository::default();
        repository
            .get_or_create(
                source("local", "path-american"),
                TrackMetadata::from_names(
                    "all-american bitch".to_owned(),
                    vec!["Olivia Rodrigo".to_owned()],
                    Some(1_000),
                )
                .unwrap(),
            )
            .await
            .unwrap();

        assert!(
            repository
                .search_tracks("me", 10, None)
                .await
                .unwrap()
                .is_empty(),
            "a short token must not match inside american"
        );
        assert!(
            repository
                .search_tracks("american", 10, None)
                .await
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            repository
                .search_tracks("bitch", 10, None)
                .await
                .unwrap()
                .len(),
            1,
            "a whole word still matches"
        );
    }

    #[tokio::test]
    async fn search_artists_collapses_rows_sharing_a_name() {
        let repository = InMemoryTrackRepository::default();
        let identified = repository
            .resolve_or_create_artist(&credit("Cage the Elephant", Some("mb-1")))
            .await
            .unwrap();
        repository
            .resolve_or_create_artist(&credit("Cage The Elephant", None))
            .await
            .unwrap();

        let artists = repository.search_artists("cage", 10, None).await.unwrap();
        assert_eq!(artists.len(), 1, "the two rows present as one result");
        assert_eq!(artists[0].artist().id(), identified.id());
        assert!(artists[0].artist().musicbrainz_artist_id().is_some());
    }

    #[tokio::test]
    async fn candidate_tracks_batches_normalized_title_lookups() {
        let repository = InMemoryTrackRepository::default();
        let first = repository
            .get_or_create(
                source("local", "path-a"),
                TrackMetadata::from_names(
                    "It\u{2019}s Just Forever".to_owned(),
                    vec!["Cage the Elephant".to_owned()],
                    Some(1_000),
                )
                .unwrap(),
            )
            .await
            .unwrap();
        repository
            .get_or_create(
                source("local", "path-b"),
                TrackMetadata::from_names(
                    "Other".to_owned(),
                    vec!["Other".to_owned()],
                    Some(1_000),
                )
                .unwrap(),
            )
            .await
            .unwrap();

        let hits = repository
            .candidate_tracks(&["It's Just Forever".to_owned(), "missing".to_owned()], 25)
            .await
            .unwrap();

        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].track().id(), first.id());
        assert!(hits[0].match_quality().is_none());
    }

    #[tokio::test]
    async fn search_matches_albums_and_their_artists() {
        let repository = InMemoryTrackRepository::default();
        repository
            .resolve_or_create_album(
                &album_metadata("Discovery").with_album_artist(credit("Daft Punk", None)),
            )
            .await
            .unwrap();

        assert_eq!(
            repository
                .search_albums("disc", 10, None)
                .await
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            repository
                .search_albums("daft", 10, None)
                .await
                .unwrap()
                .len(),
            1,
            "the album artist name matches the album"
        );
        assert!(
            repository
                .search_albums("nope", 10, None)
                .await
                .unwrap()
                .is_empty()
        );

        assert_eq!(
            repository
                .search_artists("daft", 10, None)
                .await
                .unwrap()
                .len(),
            1
        );
        assert!(
            repository
                .search_artists("nope", 10, None)
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn albums_order_by_title_artist_and_creation() {
        let repository = InMemoryTrackRepository::default();
        let first = repository
            .resolve_or_create_album(
                &album_metadata("Zebra").with_album_artist(credit("A Band", None)),
            )
            .await
            .unwrap();
        let second = repository
            .resolve_or_create_album(
                &album_metadata("Apple").with_album_artist(credit("Z Band", None)),
            )
            .await
            .unwrap();

        let by_title = repository
            .list_albums(0, 10, AlbumOrder::Title, AlbumListFilter::default(), None)
            .await
            .unwrap();
        assert_eq!(by_title[0].album().id(), second.id());
        assert_eq!(by_title[1].album().id(), first.id());

        let by_artist = repository
            .list_albums(0, 10, AlbumOrder::Artist, AlbumListFilter::default(), None)
            .await
            .unwrap();
        assert_eq!(by_artist[0].album().id(), first.id());

        let recent = repository
            .list_albums(
                0,
                10,
                AlbumOrder::RecentlyAdded,
                AlbumListFilter::default(),
                None,
            )
            .await
            .unwrap();
        assert_eq!(recent[0].album().id(), second.id());
    }

    #[tokio::test]
    async fn per_user_state_round_trips_for_every_entity() {
        let repository = InMemoryTrackRepository::default();
        let user = Uuid::now_v7();
        let track = repository
            .get_or_create(source("local", "path-a"), metadata("Song"))
            .await
            .unwrap();
        let album = repository
            .resolve_or_create_album(&album_metadata("Discovery"))
            .await
            .unwrap();
        let artist = repository
            .resolve_or_create_artist(&credit("Daft Punk", None))
            .await
            .unwrap();

        repository
            .set_starred(user, StateTarget::Track(track.id()), true)
            .await
            .unwrap();
        repository
            .set_rating(user, StateTarget::Track(track.id()), Some(4))
            .await
            .unwrap();
        repository
            .record_play(user, track.id(), chrono::Utc::now())
            .await
            .unwrap();
        repository
            .record_play(user, track.id(), chrono::Utc::now())
            .await
            .unwrap();

        let state = repository.find_track_state(track.id(), user).await.unwrap();
        assert!(state.is_starred());
        assert_eq!(state.rating(), Some(4));
        assert_eq!(state.play_count(), 2);
        assert!(
            repository
                .find_track_state(track.id(), Uuid::now_v7())
                .await
                .unwrap()
                .play_count()
                == 0,
            "state is per user"
        );

        repository
            .set_starred(user, StateTarget::Album(album.id()), true)
            .await
            .unwrap();
        let summary = repository
            .find_album_summary(album.id(), Some(user))
            .await
            .unwrap()
            .unwrap();
        assert!(summary.state().is_starred());

        repository
            .set_starred(user, StateTarget::Artist(artist.id()), true)
            .await
            .unwrap();
        repository
            .set_rating(user, StateTarget::Artist(artist.id()), Some(5))
            .await
            .unwrap();
        let artist_state = repository
            .find_artist_state(artist.id(), user)
            .await
            .unwrap();
        assert!(artist_state.is_starred());
        assert_eq!(artist_state.rating(), Some(5));

        let starred = repository.starred(user).await.unwrap();
        assert_eq!(starred.tracks().len(), 1);
        assert_eq!(starred.albums().len(), 1);
        assert_eq!(starred.artists().len(), 1);

        repository
            .set_rating(user, StateTarget::Track(track.id()), Some(0))
            .await
            .unwrap();
        assert_eq!(
            repository
                .find_track_state(track.id(), user)
                .await
                .unwrap()
                .rating(),
            None
        );
    }

    #[tokio::test]
    async fn album_orders_reflect_plays_and_ratings() {
        let repository = InMemoryTrackRepository::default();
        let user = Uuid::now_v7();
        let first = repository
            .resolve_or_create_album(&album_metadata("First"))
            .await
            .unwrap();
        let second = repository
            .resolve_or_create_album(&album_metadata("Second"))
            .await
            .unwrap();
        let first_track = repository
            .get_or_create(source("local", "path-a"), metadata("A"))
            .await
            .unwrap();
        let second_track = repository
            .get_or_create(source("local", "path-b"), metadata("B"))
            .await
            .unwrap();
        repository
            .set_album_track(first.id(), first_track.id(), &AlbumTrack::unplaced())
            .await
            .unwrap();
        repository
            .set_album_track(second.id(), second_track.id(), &AlbumTrack::unplaced())
            .await
            .unwrap();

        repository
            .record_play(user, second_track.id(), chrono::Utc::now())
            .await
            .unwrap();
        repository
            .record_play(user, second_track.id(), chrono::Utc::now())
            .await
            .unwrap();
        repository
            .set_rating(user, StateTarget::Album(first.id()), Some(5))
            .await
            .unwrap();

        let frequent = repository
            .list_albums(
                0,
                10,
                AlbumOrder::Frequent,
                AlbumListFilter::new(),
                Some(user),
            )
            .await
            .unwrap();
        assert_eq!(frequent[0].album().id(), second.id());
        assert_eq!(frequent[0].state().play_count(), 2);

        let highest = repository
            .list_albums(
                0,
                10,
                AlbumOrder::HighestRated,
                AlbumListFilter::new(),
                Some(user),
            )
            .await
            .unwrap();
        assert_eq!(highest[0].album().id(), first.id());

        let recent = repository
            .list_albums(
                0,
                10,
                AlbumOrder::Recent,
                AlbumListFilter::new(),
                Some(user),
            )
            .await
            .unwrap();
        assert_eq!(recent[0].album().id(), second.id());
    }

    #[tokio::test]
    async fn genres_aggregate_and_filter_albums() {
        let repository = InMemoryTrackRepository::default();
        let album = repository
            .resolve_or_create_album(&album_metadata("Discovery").with_year(Some(2001)).unwrap())
            .await
            .unwrap();
        let rock = repository
            .get_or_create(
                source("local", "path-a"),
                metadata("One").with_genre(Some("Rock".to_owned())).unwrap(),
            )
            .await
            .unwrap();
        let pop = repository
            .get_or_create(
                source("local", "path-b"),
                metadata("Two").with_genre(Some("Pop".to_owned())).unwrap(),
            )
            .await
            .unwrap();
        repository
            .set_album_track(album.id(), rock.id(), &AlbumTrack::unplaced())
            .await
            .unwrap();
        repository
            .set_album_track(album.id(), pop.id(), &AlbumTrack::unplaced())
            .await
            .unwrap();

        let genres = repository.list_genres().await.unwrap();
        assert_eq!(genres.len(), 2);
        assert!(
            genres
                .iter()
                .any(|genre| genre.genre() == "Rock" && genre.song_count() == 1)
        );

        let rock_albums = repository
            .list_albums(
                0,
                10,
                AlbumOrder::Title,
                AlbumListFilter::new().with_genre(Some("rock".to_owned())),
                None,
            )
            .await
            .unwrap();
        assert_eq!(rock_albums.len(), 1);
        assert_eq!(rock_albums[0].album().id(), album.id());

        let by_year = repository
            .list_albums(
                0,
                10,
                AlbumOrder::YearAscending,
                AlbumListFilter::new().with_year(Some(2000), Some(2005)),
                None,
            )
            .await
            .unwrap();
        assert_eq!(by_year.len(), 1);

        let none = repository
            .list_albums(
                0,
                10,
                AlbumOrder::YearAscending,
                AlbumListFilter::new().with_year(Some(2010), Some(2015)),
                None,
            )
            .await
            .unwrap();
        assert!(none.is_empty());
    }
}
