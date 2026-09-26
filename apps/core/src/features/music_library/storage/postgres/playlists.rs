use async_trait::async_trait;
use uuid::Uuid;

use crate::features::music_library::{
    domain::{Playlist, PlaylistId, TrackId},
    storage::contracts::{PlaylistStore, TrackRepositoryError},
};

use super::PgStore;
use super::decode::repository_error;

const PLAYLIST_COLUMNS: &str = "p.id, p.name, p.comment, p.public, p.created_at, p.updated_at, \
     (SELECT count(*) FROM playlist_entries AS e WHERE e.playlist_id = p.id)::BIGINT, \
     (SELECT COALESCE(SUM(t.duration_ms), 0)::BIGINT FROM playlist_entries AS e \
      JOIN tracks AS t ON t.id = e.track_id WHERE e.playlist_id = p.id)";

type PlaylistRow = (
    Uuid,
    String,
    Option<String>,
    bool,
    chrono::DateTime<chrono::Utc>,
    chrono::DateTime<chrono::Utc>,
    i64,
    i64,
);

fn decode_playlist(row: PlaylistRow) -> Result<Playlist, TrackRepositoryError> {
    let track_count = u32::try_from(row.6.max(0)).map_err(|_| TrackRepositoryError::InvalidData)?;
    let duration_ms = u64::try_from(row.7.max(0)).map_err(|_| TrackRepositoryError::InvalidData)?;

    Playlist::new(
        PlaylistId::from_uuid(row.0),
        row.1,
        row.2,
        row.3,
        row.4,
        row.5,
        track_count,
        duration_ms,
    )
    .map_err(|_| TrackRepositoryError::InvalidData)
}

fn validated(name: &str, comment: Option<&str>) -> Result<Playlist, TrackRepositoryError> {
    let now = chrono::Utc::now();

    Playlist::new(
        PlaylistId::new(),
        name.to_owned(),
        comment.map(str::to_owned),
        false,
        now,
        now,
        0,
        0,
    )
    .map_err(|_| TrackRepositoryError::InvalidData)
}

#[async_trait]
impl PlaylistStore for PgStore {
    async fn create_playlist(
        &self,
        user: Uuid,
        name: &str,
        comment: Option<&str>,
    ) -> Result<Playlist, TrackRepositoryError> {
        let candidate = validated(name, comment)?;

        let row = sqlx::query_as::<_, PlaylistRow>(
            r#"
            WITH inserted AS (
                INSERT INTO playlists (id, user_id, name, comment)
                VALUES ($1, $2, $3, $4)
                RETURNING id, name, comment, public, created_at, updated_at
            )
            SELECT inserted.id, inserted.name, inserted.comment, inserted.public,
                   inserted.created_at, inserted.updated_at,
                   (SELECT count(*) FROM playlist_entries AS e
                    WHERE e.playlist_id = inserted.id)::BIGINT,
                   0::BIGINT
            FROM inserted
            "#,
        )
        .bind(candidate.id().as_uuid())
        .bind(user)
        .bind(candidate.name())
        .bind(candidate.comment())
        .fetch_one(&self.pool)
        .await
        .map_err(repository_error)?;

        decode_playlist(row)
    }

    async fn update_playlist(
        &self,
        user: Uuid,
        playlist_id: &PlaylistId,
        name: &str,
        comment: Option<&str>,
        public: bool,
    ) -> Result<Option<Playlist>, TrackRepositoryError> {
        let candidate = validated(name, comment)?;

        let row = sqlx::query_as::<_, PlaylistRow>(
            r#"
            WITH updated AS (
                UPDATE playlists
                SET name = $3, comment = $4, public = $5, updated_at = CURRENT_TIMESTAMP
                WHERE id = $1 AND user_id = $2
                RETURNING id, name, comment, public, created_at, updated_at
            )
            SELECT updated.id, updated.name, updated.comment, updated.public,
                   updated.created_at, updated.updated_at,
                   (SELECT count(*) FROM playlist_entries AS e
                    WHERE e.playlist_id = updated.id)::BIGINT,
                   (SELECT COALESCE(SUM(t.duration_ms), 0)::BIGINT
                    FROM playlist_entries AS e
                    JOIN tracks AS t ON t.id = e.track_id
                    WHERE e.playlist_id = updated.id)
            FROM updated
            "#,
        )
        .bind(playlist_id.as_uuid())
        .bind(user)
        .bind(candidate.name())
        .bind(candidate.comment())
        .bind(public)
        .fetch_optional(&self.pool)
        .await
        .map_err(repository_error)?;

        row.map(decode_playlist).transpose()
    }

    async fn delete_playlist(
        &self,
        user: Uuid,
        playlist_id: &PlaylistId,
    ) -> Result<bool, TrackRepositoryError> {
        let deleted = sqlx::query("DELETE FROM playlists WHERE id = $1 AND user_id = $2")
            .bind(playlist_id.as_uuid())
            .bind(user)
            .execute(&self.pool)
            .await
            .map_err(repository_error)?
            .rows_affected();

        Ok(deleted > 0)
    }

    async fn find_playlist(
        &self,
        user: Uuid,
        playlist_id: &PlaylistId,
    ) -> Result<Option<Playlist>, TrackRepositoryError> {
        let row = sqlx::query_as::<_, PlaylistRow>(&format!(
            "SELECT {PLAYLIST_COLUMNS} FROM playlists AS p WHERE p.id = $1 AND p.user_id = $2"
        ))
        .bind(playlist_id.as_uuid())
        .bind(user)
        .fetch_optional(&self.pool)
        .await
        .map_err(repository_error)?;

        row.map(decode_playlist).transpose()
    }

    async fn list_playlists(&self, user: Uuid) -> Result<Vec<Playlist>, TrackRepositoryError> {
        let rows = sqlx::query_as::<_, PlaylistRow>(&format!(
            r#"
            SELECT {PLAYLIST_COLUMNS} FROM playlists AS p
            WHERE p.user_id = $1
            ORDER BY lower(p.name), p.id
            "#
        ))
        .bind(user)
        .fetch_all(&self.pool)
        .await
        .map_err(repository_error)?;

        rows.into_iter().map(decode_playlist).collect()
    }

    async fn playlist_tracks(
        &self,
        playlist_id: &PlaylistId,
    ) -> Result<Vec<TrackId>, TrackRepositoryError> {
        let rows = sqlx::query_as::<_, (Uuid,)>(
            r#"
            SELECT track_id FROM playlist_entries
            WHERE playlist_id = $1
            ORDER BY position, track_id
            "#,
        )
        .bind(playlist_id.as_uuid())
        .fetch_all(&self.pool)
        .await
        .map_err(repository_error)?;

        Ok(rows
            .into_iter()
            .map(|(track_id,)| TrackId::from_uuid(track_id))
            .collect())
    }

    async fn add_playlist_track(
        &self,
        user: Uuid,
        playlist_id: &PlaylistId,
        track_id: &TrackId,
    ) -> Result<bool, TrackRepositoryError> {
        let inserted = sqlx::query(
            r#"
            INSERT INTO playlist_entries (playlist_id, position, track_id)
            SELECT $1,
                   COALESCE((SELECT max(entry.position) FROM playlist_entries AS entry
                             WHERE entry.playlist_id = $1), -1) + 1,
                   $2
            WHERE EXISTS (SELECT 1 FROM playlists WHERE id = $1 AND user_id = $3)
              AND EXISTS (SELECT 1 FROM tracks WHERE id = $2)
            ON CONFLICT (playlist_id, track_id) DO NOTHING
            "#,
        )
        .bind(playlist_id.as_uuid())
        .bind(track_id.as_uuid())
        .bind(user)
        .execute(&self.pool)
        .await
        .map_err(repository_error)?
        .rows_affected();

        Ok(inserted > 0)
    }

    async fn remove_playlist_track(
        &self,
        user: Uuid,
        playlist_id: &PlaylistId,
        track_id: &TrackId,
    ) -> Result<bool, TrackRepositoryError> {
        let removed = sqlx::query(
            r#"
            DELETE FROM playlist_entries AS entry
            USING playlists
            WHERE entry.playlist_id = playlists.id
              AND playlists.id = $1
              AND playlists.user_id = $2
              AND entry.track_id = $3
            "#,
        )
        .bind(playlist_id.as_uuid())
        .bind(user)
        .bind(track_id.as_uuid())
        .execute(&self.pool)
        .await
        .map_err(repository_error)?
        .rows_affected();

        Ok(removed > 0)
    }
}
