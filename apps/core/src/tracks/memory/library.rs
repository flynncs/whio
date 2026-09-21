use async_trait::async_trait;
use uuid::Uuid;

use crate::tracks::LibraryRead;

use super::InMemoryTrackRepository;
use super::*;

#[async_trait]
impl LibraryRead for InMemoryTrackRepository {
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
                        .with_sources(entry.sources.clone())
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

    async fn count_albums(
        &self,
        _user: Option<Uuid>,
        _filter: AlbumListFilter,
    ) -> Result<u32, TrackRepositoryError> {
        let indexes = self
            .indexes
            .lock()
            .map_err(|_| TrackRepositoryError::Unavailable)?;

        Ok(indexes.albums_by_id.len() as u32)
    }

    async fn count_artists(&self, _user: Option<Uuid>) -> Result<u32, TrackRepositoryError> {
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

    async fn dedup_candidates(
        &self,
        keys: &[String],
        limit: u32,
    ) -> Result<Vec<TrackHit>, TrackRepositoryError> {
        let indexes = self
            .indexes
            .lock()
            .map_err(|_| TrackRepositoryError::Unavailable)?;
        let keys = keys
            .iter()
            .map(|key| normalize_for_matching(key))
            .filter(|key| !key.is_empty())
            .collect::<HashSet<_>>();

        if keys.is_empty() {
            return Ok(Vec::new());
        }

        let mut hits = indexes
            .by_track_id
            .values()
            .filter(|entry| owned(&entry.sources))
            .filter(|entry| {
                let title = normalize_for_matching(entry.track.title());
                keys.iter().any(|key| {
                    title == *key
                        || title.starts_with(key.as_str())
                        || key.starts_with(title.as_str())
                })
            })
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
}
