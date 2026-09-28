use async_trait::async_trait;
use sqlx::PgConnection;
use uuid::Uuid;

use crate::features::music_library::{
    domain::{
        CommentPatch, MoveTarget, OccurrenceSelector, Playlist, PlaylistEntryChange,
        PlaylistEntryError, PlaylistEntryId, PlaylistId, PlaylistMetadataPatch, PlaylistOccurrence,
        PlaylistWrite, PlaylistWriteOutcome, TrackId,
    },
    storage::contracts::{PlaylistStore, TrackRepositoryError},
};

use super::PgStore;
use super::decode::repository_error;

const PLAYLIST_COLUMNS: &str = "p.id, p.user_id, u.username, p.name, p.comment, p.public, p.revision, \
     p.created_at, p.updated_at, \
     (SELECT count(*) FROM playlist_entries AS e WHERE e.playlist_id = p.id)::BIGINT, \
     (SELECT COALESCE(SUM(t.duration_ms), 0)::BIGINT FROM playlist_entries AS e \
      JOIN tracks AS t ON t.id = e.track_id WHERE e.playlist_id = p.id)";

const PLAYLIST_SOURCE: &str = "FROM playlists AS p JOIN users AS u ON u.id = p.user_id";

type PlaylistRow = (
    Uuid,
    Uuid,
    String,
    String,
    Option<String>,
    bool,
    i64,
    chrono::DateTime<chrono::Utc>,
    chrono::DateTime<chrono::Utc>,
    i64,
    i64,
);

enum EntriesApplied {
    Done,
    Rejected(PlaylistEntryError),
}

fn decode_playlist(row: PlaylistRow) -> Result<Playlist, TrackRepositoryError> {
    let track_count = u32::try_from(row.9.max(0)).map_err(|_| TrackRepositoryError::InvalidData)?;
    let duration_ms =
        u64::try_from(row.10.max(0)).map_err(|_| TrackRepositoryError::InvalidData)?;

    Playlist::new(
        PlaylistId::from_uuid(row.0),
        row.1,
        row.2,
        row.3,
        row.4,
        row.5,
        row.6,
        row.7,
        row.8,
        track_count,
        duration_ms,
    )
    .map_err(|_| TrackRepositoryError::InvalidData)
}

fn decode_occurrence(row: (Uuid, i32, Uuid)) -> Result<PlaylistOccurrence, TrackRepositoryError> {
    let position = u32::try_from(row.1).map_err(|_| TrackRepositoryError::InvalidData)?;

    Ok(PlaylistOccurrence::new(
        PlaylistEntryId::from_uuid(row.0),
        position,
        TrackId::from_uuid(row.2),
    ))
}

async fn entry_snapshot(
    connection: &mut PgConnection,
    playlist_id: &PlaylistId,
) -> Result<Vec<(Uuid, i32)>, TrackRepositoryError> {
    sqlx::query_as::<_, (Uuid, i32)>(
        "SELECT entry_id, position FROM playlist_entries WHERE playlist_id = $1 ORDER BY position",
    )
    .bind(playlist_id.as_uuid())
    .fetch_all(&mut *connection)
    .await
    .map_err(repository_error)
}

async fn entry_order(
    connection: &mut PgConnection,
    playlist_id: &PlaylistId,
) -> Result<Vec<Uuid>, TrackRepositoryError> {
    sqlx::query_scalar::<_, Uuid>(
        "SELECT entry_id FROM playlist_entries WHERE playlist_id = $1 ORDER BY position",
    )
    .bind(playlist_id.as_uuid())
    .fetch_all(&mut *connection)
    .await
    .map_err(repository_error)
}

async fn first_unknown_track(
    connection: &mut PgConnection,
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
    .fetch_optional(&mut *connection)
    .await
    .map_err(repository_error)?;

    Ok(unknown.map(TrackId::from_uuid))
}

async fn insert_entries(
    connection: &mut PgConnection,
    playlist_id: &PlaylistId,
    tracks: &[TrackId],
) -> Result<(), TrackRepositoryError> {
    if tracks.is_empty() {
        return Ok(());
    }

    let candidate: Vec<Uuid> = tracks.iter().map(|track| *track.as_uuid()).collect();
    sqlx::query(
        "INSERT INTO playlist_entries (playlist_id, position, track_id) \
         SELECT $1, \
                COALESCE((SELECT max(entry.position) FROM playlist_entries AS entry \
                          WHERE entry.playlist_id = $1), -1) + candidate.ordinality::int, \
                candidate.track_id \
         FROM unnest($2::uuid[]) WITH ORDINALITY AS candidate(track_id, ordinality)",
    )
    .bind(playlist_id.as_uuid())
    .bind(candidate)
    .execute(&mut *connection)
    .await
    .map_err(repository_error)?;

    Ok(())
}

async fn renumber(
    connection: &mut PgConnection,
    playlist_id: &PlaylistId,
    order: &[Uuid],
) -> Result<(), TrackRepositoryError> {
    sqlx::query(
        "UPDATE playlist_entries \
         SET position = position + \
             (SELECT COALESCE(max(position), -1) + 1 FROM playlist_entries WHERE playlist_id = $1) \
         WHERE playlist_id = $1",
    )
    .bind(playlist_id.as_uuid())
    .execute(&mut *connection)
    .await
    .map_err(repository_error)?;

    sqlx::query(
        "UPDATE playlist_entries AS entry SET position = ordered.position \
         FROM (SELECT candidate.entry_id, (candidate.ordinality - 1)::int AS position \
               FROM unnest($2::uuid[]) WITH ORDINALITY AS candidate(entry_id, ordinality)) AS ordered \
         WHERE entry.playlist_id = $1 AND entry.entry_id = ordered.entry_id",
    )
    .bind(playlist_id.as_uuid())
    .bind(order)
    .execute(&mut *connection)
    .await
    .map_err(repository_error)?;

    Ok(())
}

fn resolved_index(
    snapshot: &[(Uuid, i32)],
    selector: &OccurrenceSelector,
) -> Result<usize, PlaylistEntryError> {
    match selector {
        OccurrenceSelector::Entry(entry_id) => snapshot
            .iter()
            .position(|(candidate, _)| candidate == entry_id.as_uuid())
            .ok_or_else(|| PlaylistEntryError::UnknownEntry {
                entry_id: entry_id.clone(),
            }),
        OccurrenceSelector::Index(index) => {
            let resolved = usize::try_from(*index).unwrap_or(usize::MAX);
            if resolved < snapshot.len() {
                Ok(resolved)
            } else {
                Err(PlaylistEntryError::IndexOutOfRange { index: *index })
            }
        }
    }
}

fn reordered(
    snapshot: &[(Uuid, i32)],
    selector: &OccurrenceSelector,
    target: MoveTarget,
) -> Result<Vec<Uuid>, PlaylistEntryError> {
    let from = resolved_index(snapshot, selector)?;
    let mut lifted: Vec<Uuid> = snapshot.iter().map(|(entry_id, _)| *entry_id).collect();
    let moved = lifted.remove(from);

    let at = match target {
        MoveTarget::Start => 0,
        MoveTarget::End => lifted.len(),
        MoveTarget::Before(index) => {
            let before = usize::try_from(index).unwrap_or(usize::MAX);
            if before >= snapshot.len() {
                lifted.len()
            } else if before == from {
                from
            } else if before > from {
                before - 1
            } else {
                before
            }
        }
    };

    lifted.insert(at, moved);

    Ok(lifted)
}

async fn apply_entries(
    connection: &mut PgConnection,
    playlist_id: &PlaylistId,
    change: &PlaylistEntryChange,
) -> Result<EntriesApplied, TrackRepositoryError> {
    match change {
        PlaylistEntryChange::Unchanged => Ok(EntriesApplied::Done),
        PlaylistEntryChange::Replace(tracks) => {
            if let Some(track_id) = first_unknown_track(connection, tracks).await? {
                return Ok(EntriesApplied::Rejected(PlaylistEntryError::UnknownTrack {
                    track_id,
                }));
            }

            sqlx::query("DELETE FROM playlist_entries WHERE playlist_id = $1")
                .bind(playlist_id.as_uuid())
                .execute(&mut *connection)
                .await
                .map_err(repository_error)?;
            insert_entries(connection, playlist_id, tracks).await?;

            Ok(EntriesApplied::Done)
        }
        PlaylistEntryChange::Apply {
            removals,
            additions,
        } => {
            let snapshot = entry_snapshot(connection, playlist_id).await?;
            let mut removed = Vec::with_capacity(removals.len());
            for selector in removals {
                let index = match resolved_index(&snapshot, selector) {
                    Ok(index) => index,
                    Err(error) => return Ok(EntriesApplied::Rejected(error)),
                };
                removed.push(snapshot[index].0);
            }

            if let Some(track_id) = first_unknown_track(connection, additions).await? {
                return Ok(EntriesApplied::Rejected(PlaylistEntryError::UnknownTrack {
                    track_id,
                }));
            }

            if !removed.is_empty() {
                sqlx::query(
                    "DELETE FROM playlist_entries WHERE playlist_id = $1 AND entry_id = ANY($2)",
                )
                .bind(playlist_id.as_uuid())
                .bind(&removed)
                .execute(&mut *connection)
                .await
                .map_err(repository_error)?;
            }

            insert_entries(connection, playlist_id, additions).await?;

            if !removed.is_empty() {
                let order = entry_order(connection, playlist_id).await?;
                renumber(connection, playlist_id, &order).await?;
            }

            Ok(EntriesApplied::Done)
        }
        PlaylistEntryChange::Move { selector, target } => {
            let snapshot = entry_snapshot(connection, playlist_id).await?;
            let order = match reordered(&snapshot, selector, *target) {
                Ok(order) => order,
                Err(error) => return Ok(EntriesApplied::Rejected(error)),
            };

            let current: Vec<Uuid> = snapshot.iter().map(|(entry_id, _)| *entry_id).collect();
            if order != current {
                renumber(connection, playlist_id, &order).await?;
            }

            Ok(EntriesApplied::Done)
        }
    }
}

#[async_trait]
impl PlaylistStore for PgStore {
    async fn create_playlist(
        &self,
        owner: Uuid,
        name: &str,
        comment: Option<&str>,
    ) -> Result<Playlist, TrackRepositoryError> {
        let name = Playlist::validated_name(name).map_err(|_| TrackRepositoryError::InvalidData)?;
        let comment =
            Playlist::validated_comment(comment).map_err(|_| TrackRepositoryError::InvalidData)?;
        let id = Uuid::now_v7();

        sqlx::query("INSERT INTO playlists (id, user_id, name, comment) VALUES ($1, $2, $3, $4)")
            .bind(id)
            .bind(owner)
            .bind(&name)
            .bind(&comment)
            .execute(&self.pool)
            .await
            .map_err(repository_error)?;

        let row = sqlx::query_as::<_, PlaylistRow>(&format!(
            "SELECT {PLAYLIST_COLUMNS} {PLAYLIST_SOURCE} WHERE p.id = $1"
        ))
        .bind(id)
        .fetch_one(&self.pool)
        .await
        .map_err(repository_error)?;

        decode_playlist(row)
    }

    async fn find_playlist(
        &self,
        playlist_id: &PlaylistId,
    ) -> Result<Option<Playlist>, TrackRepositoryError> {
        let row = sqlx::query_as::<_, PlaylistRow>(&format!(
            "SELECT {PLAYLIST_COLUMNS} {PLAYLIST_SOURCE} WHERE p.id = $1"
        ))
        .bind(playlist_id.as_uuid())
        .fetch_optional(&self.pool)
        .await
        .map_err(repository_error)?;

        row.map(decode_playlist).transpose()
    }

    async fn list_playlists(&self, principal: Uuid) -> Result<Vec<Playlist>, TrackRepositoryError> {
        let rows = sqlx::query_as::<_, PlaylistRow>(&format!(
            "SELECT {PLAYLIST_COLUMNS} {PLAYLIST_SOURCE} \
             WHERE p.user_id = $1 OR p.public ORDER BY lower(p.name), p.id"
        ))
        .bind(principal)
        .fetch_all(&self.pool)
        .await
        .map_err(repository_error)?;

        rows.into_iter().map(decode_playlist).collect()
    }

    async fn delete_playlist(
        &self,
        owner: Uuid,
        playlist_id: &PlaylistId,
    ) -> Result<bool, TrackRepositoryError> {
        let deleted = sqlx::query("DELETE FROM playlists WHERE id = $1 AND user_id = $2")
            .bind(playlist_id.as_uuid())
            .bind(owner)
            .execute(&self.pool)
            .await
            .map_err(repository_error)?
            .rows_affected();

        Ok(deleted > 0)
    }

    async fn transfer_playlist_owner(
        &self,
        playlist_id: &PlaylistId,
        new_owner: Uuid,
    ) -> Result<bool, TrackRepositoryError> {
        let transferred = sqlx::query(
            "UPDATE playlists SET user_id = $2, updated_at = CURRENT_TIMESTAMP WHERE id = $1",
        )
        .bind(playlist_id.as_uuid())
        .bind(new_owner)
        .execute(&self.pool)
        .await
        .map_err(repository_error)?
        .rows_affected();

        Ok(transferred > 0)
    }

    async fn playlist_occurrences(
        &self,
        playlist_id: &PlaylistId,
    ) -> Result<Vec<PlaylistOccurrence>, TrackRepositoryError> {
        let rows = sqlx::query_as::<_, (Uuid, i32, Uuid)>(
            "SELECT entry_id, position, track_id FROM playlist_entries \
             WHERE playlist_id = $1 ORDER BY position",
        )
        .bind(playlist_id.as_uuid())
        .fetch_all(&self.pool)
        .await
        .map_err(repository_error)?;

        rows.into_iter().map(decode_occurrence).collect()
    }

    async fn write_playlist(
        &self,
        write: PlaylistWrite,
    ) -> Result<PlaylistWriteOutcome, TrackRepositoryError> {
        let name = match &write.metadata.name {
            Some(name) => Some(
                Playlist::validated_name(name).map_err(|_| TrackRepositoryError::InvalidData)?,
            ),
            None => None,
        };
        let (comment_set, comment) = match &write.metadata.comment {
            CommentPatch::Keep => (false, None),
            CommentPatch::Set(comment) => (
                true,
                Playlist::validated_comment(comment.as_deref())
                    .map_err(|_| TrackRepositoryError::InvalidData)?,
            ),
        };

        let mut transaction = self.pool.begin().await.map_err(repository_error)?;
        let locked = sqlx::query_scalar::<_, i64>(
            "SELECT revision FROM playlists WHERE id = $1 AND user_id = $2 FOR UPDATE",
        )
        .bind(write.playlist_id.as_uuid())
        .bind(write.principal)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(repository_error)?;

        let Some(current_revision) = locked else {
            transaction.rollback().await.map_err(repository_error)?;

            return Ok(PlaylistWriteOutcome::Missing);
        };

        if let Some(expected) = write.expected_revision
            && expected != current_revision
        {
            transaction.rollback().await.map_err(repository_error)?;

            return Ok(PlaylistWriteOutcome::RevisionConflict { current_revision });
        }

        let applied = apply_entries(&mut transaction, &write.playlist_id, &write.entries).await?;
        if let EntriesApplied::Rejected(error) = applied {
            transaction.rollback().await.map_err(repository_error)?;

            return Ok(PlaylistWriteOutcome::Rejected(error));
        }

        sqlx::query(
            "UPDATE playlists SET name = COALESCE($3, name), \
             comment = CASE WHEN $4 THEN $5 ELSE comment END, \
             public = COALESCE($6, public), revision = revision + 1, \
             updated_at = CURRENT_TIMESTAMP \
             WHERE id = $1 AND user_id = $2",
        )
        .bind(write.playlist_id.as_uuid())
        .bind(write.principal)
        .bind(&name)
        .bind(comment_set)
        .bind(&comment)
        .bind(write.metadata.public)
        .execute(&mut *transaction)
        .await
        .map_err(repository_error)?;

        let row = sqlx::query_as::<_, PlaylistRow>(&format!(
            "SELECT {PLAYLIST_COLUMNS} {PLAYLIST_SOURCE} WHERE p.id = $1"
        ))
        .bind(write.playlist_id.as_uuid())
        .fetch_one(&mut *transaction)
        .await
        .map_err(repository_error)?;
        let playlist = decode_playlist(row)?;

        transaction.commit().await.map_err(repository_error)?;

        Ok(PlaylistWriteOutcome::Applied(playlist))
    }

    async fn add_playlist_track(
        &self,
        owner: Uuid,
        playlist_id: &PlaylistId,
        track_id: &TrackId,
    ) -> Result<bool, TrackRepositoryError> {
        let outcome = self
            .write_playlist(PlaylistWrite {
                principal: owner,
                playlist_id: playlist_id.clone(),
                expected_revision: None,
                metadata: PlaylistMetadataPatch::default(),
                entries: PlaylistEntryChange::Apply {
                    removals: Vec::new(),
                    additions: vec![track_id.clone()],
                },
            })
            .await?;

        match outcome {
            PlaylistWriteOutcome::Applied(_) => Ok(true),
            PlaylistWriteOutcome::Missing | PlaylistWriteOutcome::RevisionConflict { .. } => {
                Ok(false)
            }
            PlaylistWriteOutcome::Rejected(_) => Err(TrackRepositoryError::InvalidData),
        }
    }
}
