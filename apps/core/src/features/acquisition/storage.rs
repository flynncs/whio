use std::collections::HashMap;

use serde::{Serialize, de::DeserializeOwned};
use sqlx::{PgPool, Row, postgres::PgRow};
use uuid::Uuid;

use crate::jobs::acquisition::{
    ACQUISITION_MAX_ATTEMPTS, ACQUISITION_PRIORITY, ACQUISITION_QUEUE, AcquisitionAdvanceV1,
};
use crate::platform::jobs::{PendingJob, enqueue};

use super::domain::{
    AcquisitionOrigin, Attempt, AttemptFile, AttemptState, AttemptUpdate, FinishIntent, OriginKind,
    Request, RequestState,
};
use super::ports::{AcquisitionTarget, ExpectedTrack, MbId, ReleaseCandidate};

const REQUEST_COLUMNS: &str = "id, origin_kind, origin_id, target_kind, target_id, artist, title, \
     expected_tracks, candidates, state, reason, attempts, created_at, updated_at";

const TERMINAL_STATES: &str = "'completed', 'already_owned', 'release_not_found', \
     'release_ambiguous', 'no_candidate', 'exhausted', 'import_mismatch'";

#[derive(Debug, thiserror::Error)]
pub enum StorageError {
    #[error("acquisition storage failed")]
    Database(#[source] sqlx::Error),

    #[error("job enqueue failed")]
    Enqueue(#[source] crate::platform::jobs::JobsError),

    #[error("acquisition storage is inconsistent")]
    Inconsistent,
}

pub struct PostgresAcquisitionStore {
    pool: PgPool,
    #[cfg(test)]
    claim_hook: Option<ClaimHook>,
}

#[cfg(test)]
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum ClaimBoundary {
    BeforeInsert,
    AfterInsert,
}

#[cfg(test)]
pub(crate) type ClaimHook = std::sync::Arc<
    dyn Fn(Uuid, ClaimBoundary) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>
        + Send
        + Sync,
>;

impl PostgresAcquisitionStore {
    pub fn new(pool: PgPool) -> Self {
        Self {
            pool,
            #[cfg(test)]
            claim_hook: None,
        }
    }

    #[cfg(test)]
    pub(crate) fn with_claim_hook(mut self, hook: ClaimHook) -> Self {
        self.claim_hook = Some(hook);
        self
    }

    pub async fn find_request(&self, id: Uuid) -> Result<Option<Request>, StorageError> {
        let row = sqlx::query(&format!(
            "SELECT {REQUEST_COLUMNS} FROM acquisition_requests WHERE id = $1"
        ))
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .map_err(StorageError::Database)?;

        row.as_ref().map(request_from_row).transpose()
    }

    pub async fn find_live_by_origin(
        &self,
        kind: OriginKind,
        id: &str,
    ) -> Result<Option<Request>, StorageError> {
        let row = sqlx::query(&format!(
            "SELECT {REQUEST_COLUMNS} FROM acquisition_requests \
             WHERE origin_kind = $1 AND origin_id = $2 AND state NOT IN ({TERMINAL_STATES}) \
             ORDER BY created_at DESC LIMIT 1"
        ))
        .bind(kind.as_str())
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .map_err(StorageError::Database)?;

        row.as_ref().map(request_from_row).transpose()
    }

    pub async fn list_latest_by_origins(
        &self,
        origin_ids: &[String],
    ) -> Result<Vec<Request>, StorageError> {
        let rows = sqlx::query(&format!(
            "SELECT {REQUEST_COLUMNS} FROM acquisition_requests \
             WHERE origin_id = ANY($1) ORDER BY created_at DESC"
        ))
        .bind(origin_ids)
        .fetch_all(&self.pool)
        .await
        .map_err(StorageError::Database)?;

        let mut latest: HashMap<String, Request> = HashMap::new();
        for row in rows {
            let request = request_from_row(&row)?;

            latest.entry(request.origin.id()).or_insert(request);
        }

        Ok(origin_ids
            .iter()
            .filter_map(|id| latest.remove(id))
            .collect())
    }

    pub async fn insert_request(
        &self,
        id: Uuid,
        origin: &AcquisitionOrigin,
    ) -> Result<Option<Request>, StorageError> {
        let (kind, origin_id) = origin.key();
        let mut transaction = self.pool.begin().await.map_err(StorageError::Database)?;

        let inserted = sqlx::query(
            "INSERT INTO acquisition_requests (id, origin_kind, origin_id, state) \
             VALUES ($1, $2, $3, 'pending') ON CONFLICT DO NOTHING",
        )
        .bind(id)
        .bind(kind.as_str())
        .bind(origin_id)
        .execute(&mut *transaction)
        .await;

        match inserted {
            Ok(result) if result.rows_affected() == 0 => {
                transaction.commit().await.map_err(StorageError::Database)?;

                return Ok(None);
            }
            Ok(_) => {}
            Err(sqlx::Error::Database(database)) if database.code().as_deref() == Some("23505") => {
                transaction
                    .rollback()
                    .await
                    .map_err(StorageError::Database)?;

                return Ok(None);
            }
            Err(error) => return Err(StorageError::Database(error)),
        }

        enqueue(
            &mut transaction,
            PendingJob {
                queue: ACQUISITION_QUEUE,
                payload: &AcquisitionAdvanceV1::for_request(id),
                priority: ACQUISITION_PRIORITY,
                max_attempts: ACQUISITION_MAX_ATTEMPTS,
                operation_id: id,
                coalesce: true,
            },
        )
        .await
        .map_err(StorageError::Enqueue)?;

        transaction.commit().await.map_err(StorageError::Database)?;

        self.find_request(id).await
    }

    pub async fn insert_attempt_and_dispatch(
        &self,
        id: Uuid,
        number: u16,
        files: &[AttemptFile],
    ) -> Result<(), StorageError> {
        self.dispatch(id, number, files, None).await
    }

    pub async fn set_candidates_and_dispatch(
        &self,
        id: Uuid,
        candidates: &[ReleaseCandidate],
        number: u16,
        files: &[AttemptFile],
    ) -> Result<(), StorageError> {
        self.dispatch(id, number, files, Some(candidates)).await
    }

    async fn dispatch(
        &self,
        id: Uuid,
        number: u16,
        files: &[AttemptFile],
        candidates: Option<&[ReleaseCandidate]>,
    ) -> Result<(), StorageError> {
        let attempt = i16::try_from(number).unwrap_or(i16::MAX);
        let mut transaction = self.pool.begin().await.map_err(StorageError::Database)?;

        if let Some(candidates) = candidates {
            sqlx::query(
                "UPDATE acquisition_requests SET candidates = $2, updated_at = CURRENT_TIMESTAMP \
                 WHERE id = $1",
            )
            .bind(id)
            .bind(json(&candidates)?)
            .execute(&mut *transaction)
            .await
            .map_err(StorageError::Database)?;
        }

        sqlx::query(
            "INSERT INTO acquisition_attempts (request_id, attempt, state, files) \
             VALUES ($1, $2, 'transferring', $3) ON CONFLICT (request_id, attempt) DO NOTHING",
        )
        .bind(id)
        .bind(attempt)
        .bind(json(&files)?)
        .execute(&mut *transaction)
        .await
        .map_err(StorageError::Database)?;

        sqlx::query(
            "UPDATE acquisition_requests SET state = 'transferring', reason = NULL, \
             attempts = $2, updated_at = CURRENT_TIMESTAMP WHERE id = $1",
        )
        .bind(id)
        .bind(attempt)
        .execute(&mut *transaction)
        .await
        .map_err(StorageError::Database)?;

        transaction.commit().await.map_err(StorageError::Database)
    }

    pub async fn record_attempt(
        &self,
        id: Uuid,
        number: u16,
        update: &AttemptUpdate,
        request_state: RequestState,
        request_reason: Option<&str>,
    ) -> Result<(), StorageError> {
        let outcome = FinishIntent {
            update: update.clone(),
            request_state,
            request_reason: request_reason.map(str::to_owned),
            cancel: false,
        };
        self.record(id, number, &outcome, false).await
    }

    pub(crate) async fn persist_finish(
        &self,
        id: Uuid,
        number: u16,
        intent: &FinishIntent,
    ) -> Result<FinishIntent, StorageError> {
        let stored: String = sqlx::query_scalar(
            "UPDATE acquisition_attempts SET finish_intent = COALESCE(finish_intent, $3), \
             updated_at = CURRENT_TIMESTAMP WHERE request_id = $1 AND attempt = $2 \
             RETURNING finish_intent",
        )
        .bind(id)
        .bind(i16::try_from(number).unwrap_or(i16::MAX))
        .bind(json(intent)?)
        .fetch_one(&self.pool)
        .await
        .map_err(StorageError::Database)?;

        parse(&stored)
    }

    pub(crate) async fn complete_finish(
        &self,
        id: Uuid,
        number: u16,
        intent: &FinishIntent,
    ) -> Result<(), StorageError> {
        self.record(id, number, intent, true).await
    }

    async fn record(
        &self,
        id: Uuid,
        number: u16,
        outcome: &FinishIntent,
        finish: bool,
    ) -> Result<(), StorageError> {
        let attempt = i16::try_from(number).unwrap_or(i16::MAX);
        let update = &outcome.update;
        let mut transaction = self.pool.begin().await.map_err(StorageError::Database)?;

        sqlx::query(
            "UPDATE acquisition_attempts SET state = $3, reason = $4, handles = $5, \
             delivered = $6, imported = $7, \
             finish_intent = CASE WHEN $8 THEN NULL ELSE finish_intent END, \
             updated_at = CURRENT_TIMESTAMP WHERE request_id = $1 AND attempt = $2",
        )
        .bind(id)
        .bind(attempt)
        .bind(update.state.as_str())
        .bind(update.reason.as_deref())
        .bind(json(&update.handles)?)
        .bind(json(&update.delivered)?)
        .bind(json(&update.imported)?)
        .bind(finish)
        .execute(&mut *transaction)
        .await
        .map_err(StorageError::Database)?;

        sqlx::query(
            "UPDATE acquisition_requests SET state = $3, reason = $4, attempts = $2, \
             updated_at = CURRENT_TIMESTAMP WHERE id = $1",
        )
        .bind(id)
        .bind(attempt)
        .bind(outcome.request_state.as_str())
        .bind(outcome.request_reason.as_deref())
        .execute(&mut *transaction)
        .await
        .map_err(StorageError::Database)?;

        if finish {
            sqlx::query("DELETE FROM acquisition_claims WHERE request_id = $1 AND attempt = $2")
                .bind(id)
                .bind(attempt)
                .execute(&mut *transaction)
                .await
                .map_err(StorageError::Database)?;
        }

        transaction.commit().await.map_err(StorageError::Database)
    }

    pub async fn claim_importing(
        &self,
        id: Uuid,
        number: u16,
        update: &AttemptUpdate,
        paths: &[String],
    ) -> Result<bool, StorageError> {
        let attempt = i16::try_from(number).unwrap_or(i16::MAX);
        let mut transaction = self.pool.begin().await.map_err(StorageError::Database)?;

        sqlx::query(
            "UPDATE acquisition_attempts SET state = $3, reason = $4, handles = $5, \
             delivered = $6, imported = $7, updated_at = CURRENT_TIMESTAMP \
             WHERE request_id = $1 AND attempt = $2",
        )
        .bind(id)
        .bind(attempt)
        .bind(update.state.as_str())
        .bind(update.reason.as_deref())
        .bind(json(&update.handles)?)
        .bind(json(&update.delivered)?)
        .bind(json(&update.imported)?)
        .execute(&mut *transaction)
        .await
        .map_err(StorageError::Database)?;

        for path in paths {
            #[cfg(test)]
            if let Some(hook) = &self.claim_hook {
                hook(id, ClaimBoundary::BeforeInsert).await;
            }
            let inserted = sqlx::query(
                "INSERT INTO acquisition_claims (path, request_id, attempt) \
                 VALUES ($1, $2, $3) ON CONFLICT (path) DO NOTHING",
            )
            .bind(path)
            .bind(id)
            .bind(attempt)
            .execute(&mut *transaction)
            .await
            .map_err(StorageError::Database)?;

            #[cfg(test)]
            if let Some(hook) = &self.claim_hook {
                hook(id, ClaimBoundary::AfterInsert).await;
            }
            if inserted.rows_affected() == 0 {
                transaction
                    .rollback()
                    .await
                    .map_err(StorageError::Database)?;

                return Ok(false);
            }
        }

        sqlx::query(
            "UPDATE acquisition_requests SET state = $3, reason = $4, attempts = $2, \
             updated_at = CURRENT_TIMESTAMP WHERE id = $1",
        )
        .bind(id)
        .bind(attempt)
        .bind(RequestState::Importing.as_str())
        .bind(Option::<&str>::None)
        .execute(&mut *transaction)
        .await
        .map_err(StorageError::Database)?;

        transaction.commit().await.map_err(StorageError::Database)?;

        Ok(true)
    }

    pub async fn claim_paths_ignore_conflicts(
        &self,
        id: Uuid,
        number: u16,
        paths: &[String],
    ) -> Result<(), StorageError> {
        let attempt = i16::try_from(number).unwrap_or(i16::MAX);

        for path in paths {
            sqlx::query(
                "INSERT INTO acquisition_claims (path, request_id, attempt) \
                 VALUES ($1, $2, $3) ON CONFLICT (path) DO NOTHING",
            )
            .bind(path)
            .bind(id)
            .bind(attempt)
            .execute(&self.pool)
            .await
            .map_err(StorageError::Database)?;
        }

        Ok(())
    }

    pub async fn claimed_paths(&self, id: Uuid, number: u16) -> Result<Vec<String>, StorageError> {
        sqlx::query_scalar::<_, String>(
            "SELECT path FROM acquisition_claims WHERE request_id = $1 AND attempt = $2 \
             ORDER BY path",
        )
        .bind(id)
        .bind(i16::try_from(number).unwrap_or(i16::MAX))
        .fetch_all(&self.pool)
        .await
        .map_err(StorageError::Database)
    }

    pub async fn set_target(
        &self,
        id: Uuid,
        target: &AcquisitionTarget,
        artist: &str,
        title: &str,
        expected_tracks: &[ExpectedTrack],
    ) -> Result<(), StorageError> {
        let (kind, target_id) = target_parts(target);

        sqlx::query(
            "UPDATE acquisition_requests SET target_kind = $2, target_id = $3, artist = $4, \
             title = $5, expected_tracks = $6, updated_at = CURRENT_TIMESTAMP WHERE id = $1",
        )
        .bind(id)
        .bind(kind)
        .bind(target_id)
        .bind(artist)
        .bind(title)
        .bind(json(&expected_tracks)?)
        .execute(&self.pool)
        .await
        .map_err(StorageError::Database)?;

        Ok(())
    }

    pub async fn set_candidates(
        &self,
        id: Uuid,
        candidates: &[ReleaseCandidate],
    ) -> Result<(), StorageError> {
        sqlx::query(
            "UPDATE acquisition_requests SET candidates = $2, updated_at = CURRENT_TIMESTAMP \
             WHERE id = $1",
        )
        .bind(id)
        .bind(json(&candidates)?)
        .execute(&self.pool)
        .await
        .map_err(StorageError::Database)?;

        Ok(())
    }

    pub async fn set_state(&self, id: Uuid, state: RequestState) -> Result<(), StorageError> {
        sqlx::query(
            "UPDATE acquisition_requests SET state = $2, reason = NULL, \
             updated_at = CURRENT_TIMESTAMP WHERE id = $1",
        )
        .bind(id)
        .bind(state.as_str())
        .execute(&self.pool)
        .await
        .map_err(StorageError::Database)?;

        Ok(())
    }

    pub async fn set_terminal(
        &self,
        id: Uuid,
        state: RequestState,
        reason: Option<&str>,
    ) -> Result<(), StorageError> {
        sqlx::query(
            "UPDATE acquisition_requests SET state = $2, reason = $3, \
             updated_at = CURRENT_TIMESTAMP WHERE id = $1",
        )
        .bind(id)
        .bind(state.as_str())
        .bind(reason)
        .execute(&self.pool)
        .await
        .map_err(StorageError::Database)?;

        Ok(())
    }

    pub async fn set_attempts(&self, id: Uuid, attempts: u16) -> Result<(), StorageError> {
        sqlx::query(
            "UPDATE acquisition_requests SET attempts = $2, updated_at = CURRENT_TIMESTAMP \
             WHERE id = $1",
        )
        .bind(id)
        .bind(i16::try_from(attempts).unwrap_or(i16::MAX))
        .execute(&self.pool)
        .await
        .map_err(StorageError::Database)?;

        Ok(())
    }

    pub async fn insert_attempt(
        &self,
        id: Uuid,
        number: u16,
        files: &[AttemptFile],
    ) -> Result<(), StorageError> {
        sqlx::query(
            "INSERT INTO acquisition_attempts (request_id, attempt, state, files) \
             VALUES ($1, $2, 'transferring', $3) ON CONFLICT (request_id, attempt) DO NOTHING",
        )
        .bind(id)
        .bind(i16::try_from(number).unwrap_or(i16::MAX))
        .bind(json(&files)?)
        .execute(&self.pool)
        .await
        .map_err(StorageError::Database)?;

        Ok(())
    }

    pub async fn update_attempt(
        &self,
        id: Uuid,
        number: u16,
        update: &AttemptUpdate,
    ) -> Result<(), StorageError> {
        sqlx::query(
            "UPDATE acquisition_attempts SET state = $3, reason = $4, handles = $5, \
             delivered = $6, imported = $7, updated_at = CURRENT_TIMESTAMP \
             WHERE request_id = $1 AND attempt = $2",
        )
        .bind(id)
        .bind(i16::try_from(number).unwrap_or(i16::MAX))
        .bind(update.state.as_str())
        .bind(update.reason.as_deref())
        .bind(json(&update.handles)?)
        .bind(json(&update.delivered)?)
        .bind(json(&update.imported)?)
        .execute(&self.pool)
        .await
        .map_err(StorageError::Database)?;

        Ok(())
    }

    pub async fn load_attempt(
        &self,
        id: Uuid,
        number: u16,
    ) -> Result<Option<Attempt>, StorageError> {
        let row = sqlx::query(
            "SELECT request_id, attempt, state, reason, files, handles, delivered, imported, \
             finish_intent, created_at FROM acquisition_attempts WHERE request_id = $1 AND attempt = $2",
        )
        .bind(id)
        .bind(i16::try_from(number).unwrap_or(i16::MAX))
        .fetch_optional(&self.pool)
        .await
        .map_err(StorageError::Database)?;

        row.as_ref().map(attempt_from_row).transpose()
    }

    pub async fn load_current_attempt(&self, id: Uuid) -> Result<Option<Attempt>, StorageError> {
        let row = sqlx::query(
            "SELECT request_id, attempt, state, reason, files, handles, delivered, imported, \
             finish_intent, created_at FROM acquisition_attempts WHERE request_id = $1 \
             ORDER BY attempt DESC LIMIT 1",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .map_err(StorageError::Database)?;

        row.as_ref().map(attempt_from_row).transpose()
    }
}

fn target_parts(target: &AcquisitionTarget) -> (&'static str, String) {
    match target {
        AcquisitionTarget::Release(id) => ("release", id.to_string()),
        AcquisitionTarget::Recording(id) => ("recording", id.to_string()),
    }
}

fn json<T: Serialize>(value: &T) -> Result<String, StorageError> {
    serde_json::to_string(value).map_err(|_| StorageError::Inconsistent)
}

fn parse<T: DeserializeOwned>(value: &str) -> Result<T, StorageError> {
    serde_json::from_str(value).map_err(|_| StorageError::Inconsistent)
}

fn request_from_row(row: &PgRow) -> Result<Request, StorageError> {
    let kind = row
        .try_get::<String, _>("origin_kind")
        .map_err(StorageError::Database)?;
    let origin_id = row
        .try_get::<String, _>("origin_id")
        .map_err(StorageError::Database)?;
    let origin = OriginKind::parse(&kind)
        .and_then(|kind| AcquisitionOrigin::parse(kind, &origin_id))
        .ok_or(StorageError::Inconsistent)?;
    let target = match row
        .try_get::<Option<String>, _>("target_kind")
        .map_err(StorageError::Database)?
    {
        Some(kind) => {
            let id = row
                .try_get::<Option<String>, _>("target_id")
                .map_err(StorageError::Database)?
                .ok_or(StorageError::Inconsistent)?;
            let id = MbId::parse(&id).ok_or(StorageError::Inconsistent)?;

            Some(match kind.as_str() {
                "release" => AcquisitionTarget::Release(id),
                "recording" => AcquisitionTarget::Recording(id),
                _ => return Err(StorageError::Inconsistent),
            })
        }
        None => None,
    };

    Ok(Request {
        id: row.try_get("id").map_err(StorageError::Database)?,
        origin,
        target,
        artist: row.try_get("artist").map_err(StorageError::Database)?,
        title: row.try_get("title").map_err(StorageError::Database)?,
        expected_tracks: parse(
            &row.try_get::<String, _>("expected_tracks")
                .map_err(StorageError::Database)?,
        )?,
        candidates: parse(
            &row.try_get::<String, _>("candidates")
                .map_err(StorageError::Database)?,
        )?,
        state: row
            .try_get::<String, _>("state")
            .map_err(StorageError::Database)
            .and_then(|state| RequestState::parse(&state).ok_or(StorageError::Inconsistent))?,
        reason: row.try_get("reason").map_err(StorageError::Database)?,
        attempts: u16::try_from(
            row.try_get::<i32, _>("attempts")
                .map_err(StorageError::Database)?,
        )
        .map_err(|_| StorageError::Inconsistent)?,
        created_at: row.try_get("created_at").map_err(StorageError::Database)?,
        updated_at: row.try_get("updated_at").map_err(StorageError::Database)?,
    })
}

fn attempt_from_row(row: &PgRow) -> Result<Attempt, StorageError> {
    Ok(Attempt {
        request_id: row.try_get("request_id").map_err(StorageError::Database)?,
        number: u16::try_from(
            row.try_get::<i16, _>("attempt")
                .map_err(StorageError::Database)?,
        )
        .map_err(|_| StorageError::Inconsistent)?,
        state: row
            .try_get::<String, _>("state")
            .map_err(StorageError::Database)
            .and_then(|state| AttemptState::parse(&state).ok_or(StorageError::Inconsistent))?,
        reason: row.try_get("reason").map_err(StorageError::Database)?,
        files: parse(
            &row.try_get::<String, _>("files")
                .map_err(StorageError::Database)?,
        )?,
        handles: parse(
            &row.try_get::<String, _>("handles")
                .map_err(StorageError::Database)?,
        )?,
        delivered: parse(
            &row.try_get::<String, _>("delivered")
                .map_err(StorageError::Database)?,
        )?,
        imported: parse(
            &row.try_get::<String, _>("imported")
                .map_err(StorageError::Database)?,
        )?,
        finish_intent: row
            .try_get::<Option<String>, _>("finish_intent")
            .map_err(StorageError::Database)?
            .as_deref()
            .map(parse)
            .transpose()?,
        created_at: row.try_get("created_at").map_err(StorageError::Database)?,
    })
}
