use async_trait::async_trait;
use sqlx::{PgConnection, PgPool};
use uuid::Uuid;

use crate::features::music_library::{
    domain::{
        RestoreQueueEntry, RestoreQueueEntryId, RestoreQueueSnapshot, RestoreQueueValidationError,
        RestoreQueueWrite, RestoreQueueWriteOutcome, TrackId,
    },
    queue::MAX_RESTORE_QUEUE_ENTRIES,
    storage::contracts::{RestoreQueueStore, TrackRepositoryError},
};

use super::PgStore;
use super::decode::repository_error;

type RestoreQueueRow = (
    Option<i32>,
    i64,
    i64,
    Uuid,
    String,
    chrono::DateTime<chrono::Utc>,
);

fn validate_save(write: &RestoreQueueWrite) -> Result<(), RestoreQueueValidationError> {
    let entry_count = write.entries.len();
    if entry_count > MAX_RESTORE_QUEUE_ENTRIES {
        return Err(RestoreQueueValidationError::TooManyEntries {
            max_entries: MAX_RESTORE_QUEUE_ENTRIES,
        });
    }

    match write.current_index {
        None if entry_count > 0 => {
            Err(RestoreQueueValidationError::CurrentIndexMissing { entry_count })
        }
        Some(index) if usize::try_from(index).unwrap_or(usize::MAX) >= entry_count => {
            Err(RestoreQueueValidationError::CurrentIndexOutOfRange { index, entry_count })
        }
        _ => Ok(()),
    }
}

fn decode_entry(row: (Uuid, i32, Uuid)) -> Result<RestoreQueueEntry, TrackRepositoryError> {
    let position = u32::try_from(row.1).map_err(|_| TrackRepositoryError::InvalidData)?;

    Ok(RestoreQueueEntry::new(
        RestoreQueueEntryId::from_uuid(row.0),
        position,
        TrackId::from_uuid(row.2),
    ))
}

async fn entries(
    connection: &mut PgConnection,
    user: Uuid,
) -> Result<Vec<RestoreQueueEntry>, TrackRepositoryError> {
    let rows = sqlx::query_as::<_, (Uuid, i32, Uuid)>(
        "SELECT entry_id, position, track_id FROM restore_queue_entries \
         WHERE queue_user_id = $1 ORDER BY position",
    )
    .bind(user)
    .fetch_all(&mut *connection)
    .await
    .map_err(repository_error)?;

    rows.into_iter().map(decode_entry).collect()
}

async fn insert_entries(
    connection: &mut PgConnection,
    user: Uuid,
    tracks: &[TrackId],
) -> Result<(), sqlx::Error> {
    if tracks.is_empty() {
        return Ok(());
    }

    let candidate: Vec<Uuid> = tracks.iter().map(|track| *track.as_uuid()).collect();
    sqlx::query(
        "INSERT INTO restore_queue_entries (queue_user_id, position, track_id) \
         SELECT $1, candidate.ordinality::int - 1, candidate.track_id \
         FROM unnest($2::uuid[]) WITH ORDINALITY AS candidate(track_id, ordinality)",
    )
    .bind(user)
    .bind(candidate)
    .execute(&mut *connection)
    .await?;

    Ok(())
}

async fn first_unknown_track(
    pool: &PgPool,
    tracks: &[TrackId],
) -> Result<Option<TrackId>, TrackRepositoryError> {
    if tracks.is_empty() {
        return Ok(None);
    }

    let candidate: Vec<Uuid> = tracks.iter().map(|track| *track.as_uuid()).collect();
    let unknown = sqlx::query_scalar::<_, Uuid>(
        "SELECT candidate.track_id FROM unnest($1::uuid[]) AS candidate(track_id) \
         WHERE NOT EXISTS (SELECT 1 FROM tracks WHERE tracks.id = candidate.track_id) LIMIT 1",
    )
    .bind(candidate)
    .fetch_optional(pool)
    .await
    .map_err(repository_error)?;

    Ok(unknown.map(TrackId::from_uuid))
}

#[async_trait]
impl RestoreQueueStore for PgStore {
    async fn read_restore_queue(
        &self,
        user: Uuid,
    ) -> Result<Option<RestoreQueueSnapshot>, TrackRepositoryError> {
        let row = sqlx::query_as::<_, RestoreQueueRow>(
            "SELECT q.current_index, q.position_ms, q.revision, q.changed_by, u.username, \
             q.changed_at FROM restore_queues AS q JOIN users AS u ON u.id = q.changed_by \
             WHERE q.user_id = $1",
        )
        .bind(user)
        .fetch_optional(&self.pool)
        .await
        .map_err(repository_error)?;

        let Some((current_index, position_ms, revision, changed_by, username, changed_at)) = row
        else {
            return Ok(None);
        };
        let current_index = current_index
            .map(u32::try_from)
            .transpose()
            .map_err(|_| TrackRepositoryError::InvalidData)?;
        let position_ms =
            u64::try_from(position_ms).map_err(|_| TrackRepositoryError::InvalidData)?;

        let mut connection = self.pool.acquire().await.map_err(repository_error)?;
        let entries = entries(&mut connection, user).await?;

        Ok(Some(RestoreQueueSnapshot::new(
            user,
            changed_by,
            username,
            current_index,
            position_ms,
            revision,
            changed_at,
            entries,
        )))
    }

    async fn save_restore_queue(
        &self,
        write: RestoreQueueWrite,
    ) -> Result<RestoreQueueWriteOutcome, TrackRepositoryError> {
        if let Err(error) = validate_save(&write) {
            return Ok(RestoreQueueWriteOutcome::Rejected(error));
        }
        let current_index = match write.current_index {
            Some(index) => {
                Some(i32::try_from(index).map_err(|_| TrackRepositoryError::InvalidData)?)
            }
            None => None,
        };
        let position_ms =
            i64::try_from(write.position_ms).map_err(|_| TrackRepositoryError::InvalidData)?;

        let mut transaction = self.pool.begin().await.map_err(repository_error)?;
        let locked = sqlx::query_scalar::<_, i64>(
            "SELECT revision FROM restore_queues WHERE user_id = $1 FOR UPDATE",
        )
        .bind(write.user_id)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(repository_error)?;
        let current_revision = locked.unwrap_or(0);

        if let Some(expected) = write.expected_revision
            && expected != current_revision
        {
            transaction.rollback().await.map_err(repository_error)?;

            return Ok(RestoreQueueWriteOutcome::RevisionConflict { current_revision });
        }

        let saved = sqlx::query_as::<_, (i64, chrono::DateTime<chrono::Utc>, String)>(
            "INSERT INTO restore_queues (user_id, current_index, position_ms, revision, changed_by) \
             VALUES ($1, $2, $3, 1, $1) \
             ON CONFLICT (user_id) DO UPDATE SET current_index = EXCLUDED.current_index, \
                 position_ms = EXCLUDED.position_ms, changed_by = EXCLUDED.changed_by, \
                 changed_at = CURRENT_TIMESTAMP, revision = restore_queues.revision + 1 \
             RETURNING revision, changed_at, (SELECT username FROM users WHERE id = $1)",
        )
        .bind(write.user_id)
        .bind(current_index)
        .bind(position_ms)
        .fetch_one(&mut *transaction)
        .await
        .map_err(repository_error)?;

        sqlx::query("DELETE FROM restore_queue_entries WHERE queue_user_id = $1")
            .bind(write.user_id)
            .execute(&mut *transaction)
            .await
            .map_err(repository_error)?;

        if let Err(error) = insert_entries(&mut transaction, write.user_id, &write.entries).await {
            if matches!(&error, sqlx::Error::Database(database) if database.is_foreign_key_violation())
            {
                transaction.rollback().await.map_err(repository_error)?;
                if let Some(track_id) = first_unknown_track(&self.pool, &write.entries).await? {
                    return Ok(RestoreQueueWriteOutcome::Rejected(
                        RestoreQueueValidationError::UnknownTrack { track_id },
                    ));
                }
            }

            return Err(repository_error(error));
        }

        let entries = entries(&mut transaction, write.user_id).await?;
        transaction.commit().await.map_err(repository_error)?;

        Ok(RestoreQueueWriteOutcome::Saved(RestoreQueueSnapshot::new(
            write.user_id,
            write.user_id,
            saved.2,
            write.current_index,
            write.position_ms,
            saved.0,
            saved.1,
            entries,
        )))
    }

    async fn clear_restore_queue(&self, user: Uuid) -> Result<bool, TrackRepositoryError> {
        let cleared = sqlx::query("DELETE FROM restore_queues WHERE user_id = $1")
            .bind(user)
            .execute(&self.pool)
            .await
            .map_err(repository_error)?
            .rows_affected();

        Ok(cleared > 0)
    }
}
