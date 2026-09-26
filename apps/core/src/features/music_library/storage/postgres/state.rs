use async_trait::async_trait;
use sqlx::PgPool;
use uuid::Uuid;

use crate::features::music_library::{
    domain::{ArtistId, LibraryState, Starred, StateTarget, TrackId},
    storage::contracts::TrackRepositoryError,
};

use crate::features::music_library::storage::contracts::StateStore;

use super::PgStore;
use super::decode::{
    album_summaries_by_ids, artist_states, artist_summaries_by_ids, repository_error, track_states,
    tracks_by_ids,
};

#[async_trait]
impl StateStore for PgStore {
    async fn find_track_state(
        &self,
        track_id: &TrackId,
        user: Uuid,
    ) -> Result<LibraryState, TrackRepositoryError> {
        let states = track_states(
            &self.pool,
            Some(user),
            std::slice::from_ref(track_id.as_uuid()),
        )
        .await?;

        Ok(states.get(track_id.as_uuid()).copied().unwrap_or_default())
    }

    async fn find_artist_state(
        &self,
        artist_id: &ArtistId,
        user: Uuid,
    ) -> Result<LibraryState, TrackRepositoryError> {
        let states = artist_states(
            &self.pool,
            Some(user),
            std::slice::from_ref(artist_id.as_uuid()),
        )
        .await?;

        Ok(states.get(artist_id.as_uuid()).copied().unwrap_or_default())
    }

    async fn starred(&self, user: Uuid) -> Result<Starred, TrackRepositoryError> {
        let track_ids = sqlx::query_scalar::<_, Uuid>(
            "SELECT track_id FROM track_favorites WHERE user_id = $1 ORDER BY starred_at DESC, track_id",
        )
        .bind(user)
        .fetch_all(&self.pool)
        .await
        .map_err(repository_error)?;
        let album_ids = sqlx::query_scalar::<_, Uuid>(
            "SELECT album_id FROM album_favorites WHERE user_id = $1 ORDER BY starred_at DESC, album_id",
        )
        .bind(user)
        .fetch_all(&self.pool)
        .await
        .map_err(repository_error)?;
        let artist_ids = sqlx::query_scalar::<_, Uuid>(
            "SELECT artist_id FROM artist_favorites WHERE user_id = $1 ORDER BY starred_at DESC, artist_id",
        )
        .bind(user)
        .fetch_all(&self.pool)
        .await
        .map_err(repository_error)?;

        Ok(Starred::new(
            tracks_by_ids(&self.pool, &track_ids, user).await?,
            album_summaries_by_ids(&self.pool, &album_ids, Some(user)).await?,
            artist_summaries_by_ids(&self.pool, &artist_ids, Some(user)).await?,
        ))
    }

    async fn set_starred(
        &self,
        user: Uuid,
        target: StateTarget<'_>,
        starred: bool,
    ) -> Result<(), TrackRepositoryError> {
        match target {
            StateTarget::Track(track_id) => {
                set_starred_row(
                    &self.pool,
                    "track_favorites",
                    "track_id",
                    user,
                    track_id.as_uuid(),
                    starred,
                )
                .await
            }
            StateTarget::Album(album_id) => {
                set_starred_row(
                    &self.pool,
                    "album_favorites",
                    "album_id",
                    user,
                    album_id.as_uuid(),
                    starred,
                )
                .await
            }
            StateTarget::Artist(artist_id) => {
                set_starred_row(
                    &self.pool,
                    "artist_favorites",
                    "artist_id",
                    user,
                    artist_id.as_uuid(),
                    starred,
                )
                .await
            }
        }
    }

    async fn set_rating(
        &self,
        user: Uuid,
        target: StateTarget<'_>,
        rating: Option<u8>,
    ) -> Result<(), TrackRepositoryError> {
        let rating = crate::features::music_library::domain::checked_rating(rating)
            .map_err(|_| TrackRepositoryError::InvalidData)?;
        let rating = rating.map(i16::from);

        match target {
            StateTarget::Track(track_id) => {
                set_rating_row(
                    &self.pool,
                    "track_ratings",
                    "track_id",
                    user,
                    track_id.as_uuid(),
                    rating,
                )
                .await
            }
            StateTarget::Album(album_id) => {
                set_rating_row(
                    &self.pool,
                    "album_ratings",
                    "album_id",
                    user,
                    album_id.as_uuid(),
                    rating,
                )
                .await
            }
            StateTarget::Artist(artist_id) => {
                set_rating_row(
                    &self.pool,
                    "artist_ratings",
                    "artist_id",
                    user,
                    artist_id.as_uuid(),
                    rating,
                )
                .await
            }
        }
    }

    async fn record_play(
        &self,
        user: Uuid,
        track_id: &TrackId,
        played_at: chrono::DateTime<chrono::Utc>,
    ) -> Result<(), TrackRepositoryError> {
        sqlx::query(
            r#"
            INSERT INTO track_play_stats (user_id, track_id, play_count, last_played_at)
            VALUES ($1, $2, 1, $3)
            ON CONFLICT (user_id, track_id) DO UPDATE
            SET play_count = track_play_stats.play_count + 1,
                last_played_at = EXCLUDED.last_played_at
            "#,
        )
        .bind(user)
        .bind(track_id.as_uuid())
        .bind(played_at)
        .execute(&self.pool)
        .await
        .map(|_| ())
        .map_err(repository_error)
    }
}

async fn set_starred_row(
    pool: &PgPool,
    table: &'static str,
    column: &'static str,
    user: Uuid,
    id: &Uuid,
    starred: bool,
) -> Result<(), TrackRepositoryError> {
    let statement = if starred {
        format!("INSERT INTO {table} (user_id, {column}) VALUES ($1, $2) ON CONFLICT DO NOTHING")
    } else {
        format!("DELETE FROM {table} WHERE user_id = $1 AND {column} = $2")
    };

    sqlx::query(&statement)
        .bind(user)
        .bind(id)
        .execute(pool)
        .await
        .map(|_| ())
        .map_err(repository_error)
}

async fn set_rating_row(
    pool: &PgPool,
    table: &'static str,
    column: &'static str,
    user: Uuid,
    id: &Uuid,
    rating: Option<i16>,
) -> Result<(), TrackRepositoryError> {
    match rating {
        Some(rating) => {
            let statement = format!(
                "INSERT INTO {table} (user_id, {column}, rating) VALUES ($1, $2, $3)                  ON CONFLICT (user_id, {column}) DO UPDATE                  SET rating = EXCLUDED.rating, updated_at = CURRENT_TIMESTAMP"
            );
            sqlx::query(&statement)
                .bind(user)
                .bind(id)
                .bind(rating)
                .execute(pool)
                .await
                .map(|_| ())
                .map_err(repository_error)
        }
        None => {
            let statement = format!("DELETE FROM {table} WHERE user_id = $1 AND {column} = $2");
            sqlx::query(&statement)
                .bind(user)
                .bind(id)
                .execute(pool)
                .await
                .map(|_| ())
                .map_err(repository_error)
        }
    }
}
