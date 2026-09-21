use async_trait::async_trait;
use uuid::Uuid;

use crate::tracks::StateStore;

use super::InMemoryTrackRepository;
use super::*;

#[async_trait]
impl StateStore for InMemoryTrackRepository {
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
