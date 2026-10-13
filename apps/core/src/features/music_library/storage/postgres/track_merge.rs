use uuid::Uuid;

use crate::features::music_library::storage::contracts::TrackRepositoryError;

use super::decode::repository_error;
use super::reports::{move_keyed_rows, move_state_rows};
use sqlx::PgConnection;

pub(super) async fn merge_track(
    connection: &mut PgConnection,
    source_id: Uuid,
    target_id: Uuid,
) -> Result<(), TrackRepositoryError> {
    sqlx::query("UPDATE track_sources SET track_id = $1 WHERE track_id = $2")
        .bind(target_id)
        .bind(source_id)
        .execute(&mut *connection)
        .await
        .map_err(repository_error)?;
    move_keyed_rows(
        connection,
        "album_tracks",
        "track_id",
        source_id,
        target_id,
        &["album_id"],
    )
    .await?;
    merge_track_credits(connection, source_id, target_id).await?;
    merge_track_genres(connection, source_id, target_id).await?;
    fill_missing_track_dates(connection, source_id, target_id).await?;
    merge_play_stats(connection, source_id, target_id).await?;
    for table in ["track_favorites", "track_ratings"] {
        move_state_rows(connection, table, "track_id", source_id, target_id).await?;
    }
    sqlx::query("UPDATE playlist_entries SET track_id = $1 WHERE track_id = $2")
        .bind(target_id)
        .bind(source_id)
        .execute(&mut *connection)
        .await
        .map_err(repository_error)?;
    move_keyed_rows(
        connection,
        "listens",
        "track_id",
        source_id,
        target_id,
        &["user_id", "origin", "listen_at", "session_key"],
    )
    .await?;
    move_keyed_rows(
        connection,
        "listen_deliveries",
        "track_id",
        source_id,
        target_id,
        &[
            "user_id",
            "origin",
            "listen_at",
            "session_key",
            "destination_id",
        ],
    )
    .await?;
    sqlx::query("UPDATE restore_queue_entries SET track_id = $1 WHERE track_id = $2")
        .bind(target_id)
        .bind(source_id)
        .execute(&mut *connection)
        .await
        .map_err(repository_error)?;
    sqlx::query("DELETE FROM tracks WHERE id = $1")
        .bind(source_id)
        .execute(&mut *connection)
        .await
        .map_err(repository_error)?;
    Ok(())
}

async fn merge_play_stats(
    connection: &mut PgConnection,
    source_id: Uuid,
    target_id: Uuid,
) -> Result<(), TrackRepositoryError> {
    sqlx::query(
        r#"
        INSERT INTO track_play_stats (user_id, track_id, play_count, last_played_at)
        SELECT user_id, $2, play_count, last_played_at
        FROM track_play_stats WHERE track_id = $1
        ON CONFLICT (user_id, track_id) DO UPDATE
        SET play_count = track_play_stats.play_count + EXCLUDED.play_count,
            last_played_at = GREATEST(track_play_stats.last_played_at, EXCLUDED.last_played_at)
        "#,
    )
    .bind(source_id)
    .bind(target_id)
    .execute(&mut *connection)
    .await
    .map_err(repository_error)?;
    Ok(())
}

async fn merge_track_credits(
    connection: &mut PgConnection,
    source_id: Uuid,
    target_id: Uuid,
) -> Result<(), TrackRepositoryError> {
    sqlx::query(
        r#"
        WITH missing AS (
            SELECT name, artist_id, role, min(position) AS source_position
            FROM track_artist_credits AS moved
            WHERE moved.track_id = $1
              AND NOT EXISTS (
                  SELECT 1 FROM track_artist_credits AS kept
                  WHERE kept.track_id = $2
                    AND kept.artist_id IS NOT DISTINCT FROM moved.artist_id
                    AND kept.role IS NOT DISTINCT FROM moved.role
                    AND kept.name = moved.name
              )
            GROUP BY name, artist_id, role
        )
        INSERT INTO track_artist_credits (track_id, position, name, artist_id, role)
        SELECT $2,
               (COALESCE((SELECT max(position) FROM track_artist_credits WHERE track_id = $2), -1)
                + row_number() OVER (ORDER BY source_position))::INTEGER,
               name, artist_id, role
        FROM missing
        "#,
    )
    .bind(source_id)
    .bind(target_id)
    .execute(&mut *connection)
    .await
    .map_err(repository_error)?;
    Ok(())
}

async fn merge_track_genres(
    connection: &mut PgConnection,
    source_id: Uuid,
    target_id: Uuid,
) -> Result<(), TrackRepositoryError> {
    sqlx::query(
        r#"
        INSERT INTO track_genres (track_id, position, name)
        SELECT $2,
               (COALESCE((SELECT max(position) FROM track_genres WHERE track_id = $2), -1)
                + row_number() OVER (ORDER BY moved.position))::INTEGER,
               moved.name
        FROM track_genres AS moved
        WHERE moved.track_id = $1
          AND NOT EXISTS (
              SELECT 1 FROM track_genres AS kept
              WHERE kept.track_id = $2 AND kept.name_normalized = moved.name_normalized
          )
        "#,
    )
    .bind(source_id)
    .bind(target_id)
    .execute(&mut *connection)
    .await
    .map_err(repository_error)?;
    Ok(())
}

async fn fill_missing_track_dates(
    connection: &mut PgConnection,
    source_id: Uuid,
    target_id: Uuid,
) -> Result<(), TrackRepositoryError> {
    sqlx::query(
        r#"
        UPDATE tracks AS kept
        SET recording_date = COALESCE(kept.recording_date, moved.recording_date),
            recording_date_precision = CASE WHEN kept.recording_date IS NULL
                THEN moved.recording_date_precision ELSE kept.recording_date_precision END,
            original_date = COALESCE(kept.original_date, moved.original_date),
            original_date_precision = CASE WHEN kept.original_date IS NULL
                THEN moved.original_date_precision ELSE kept.original_date_precision END
        FROM tracks AS moved
        WHERE kept.id = $2 AND moved.id = $1
        "#,
    )
    .bind(source_id)
    .bind(target_id)
    .execute(&mut *connection)
    .await
    .map_err(repository_error)?;
    Ok(())
}
