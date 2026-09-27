use async_trait::async_trait;
use sqlx::PgPool;
use uuid::Uuid;

use crate::features::music_library::{
    domain::{
        ListenCount, ListenExtent, ListenOrder, ListenOrigin, ListenOutcome, ListenPage,
        ListenQuery, ListenRecord, ListenSessionId, ListenSubmission, ListenWindow, TrackId,
    },
    storage::contracts::{ListenStore, TrackRepositoryError},
};

use super::PgStore;
use super::decode::repository_error;

const COUNTS_AS_PLAY: &str = "COALESCE(qualified, TRUE)";

const LISTEN_COLUMNS: &str =
    "origin, track_id, listen_at, session_id, start_position_ms, listened_ms, qualified";

type ListenRow = (
    String,
    Uuid,
    chrono::DateTime<chrono::Utc>,
    Option<String>,
    Option<i64>,
    Option<i64>,
    Option<bool>,
);

fn order_clause(order: ListenOrder) -> &'static str {
    match order {
        ListenOrder::NewestFirst => "DESC",
        ListenOrder::OldestFirst => "ASC",
    }
}

fn decode_listen(row: ListenRow) -> Result<ListenRecord, TrackRepositoryError> {
    let origin = ListenOrigin::parse(&row.0).ok_or(TrackRepositoryError::InvalidData)?;
    let session_id = row
        .3
        .map(ListenSessionId::new)
        .transpose()
        .map_err(|_| TrackRepositoryError::InvalidData)?;
    let extent = match (row.4, row.5, row.6) {
        (Some(start_position_ms), Some(listened_ms), Some(qualified)) => Some(ListenExtent::new(
            u64::try_from(start_position_ms).map_err(|_| TrackRepositoryError::InvalidData)?,
            u64::try_from(listened_ms).map_err(|_| TrackRepositoryError::InvalidData)?,
            qualified,
        )),
        (None, None, None) => None,
        _ => return Err(TrackRepositoryError::InvalidData),
    };

    Ok(ListenRecord::new(
        origin,
        TrackId::from_uuid(row.1),
        row.2,
        session_id,
        extent,
    ))
}

async fn listen_marker(
    pool: &PgPool,
    principal: Uuid,
) -> Result<Option<chrono::DateTime<chrono::Utc>>, TrackRepositoryError> {
    sqlx::query_scalar::<_, chrono::DateTime<chrono::Utc>>(
        "SELECT started_at FROM listen_marks WHERE user_id = $1",
    )
    .bind(principal)
    .fetch_optional(pool)
    .await
    .map_err(repository_error)
}

#[async_trait]
impl ListenStore for PgStore {
    async fn submit_listen(
        &self,
        submission: &ListenSubmission,
    ) -> Result<ListenOutcome, TrackRepositoryError> {
        let extent = submission.extent();
        let start_position_ms = extent
            .map(|extent| i64::try_from(extent.start_position_ms()))
            .transpose()
            .map_err(|_| TrackRepositoryError::InvalidData)?;
        let listened_ms = extent
            .map(|extent| i64::try_from(extent.listened_ms()))
            .transpose()
            .map_err(|_| TrackRepositoryError::InvalidData)?;
        let qualified = extent.map(|extent| extent.qualified());

        let mut transaction = self.pool.begin().await.map_err(repository_error)?;

        sqlx::query(
            "INSERT INTO listen_marks (user_id, started_at) \
             VALUES ($1, LEAST($2, CURRENT_TIMESTAMP)) \
             ON CONFLICT (user_id) DO NOTHING",
        )
        .bind(submission.principal())
        .bind(submission.listen_at())
        .execute(&mut *transaction)
        .await
        .map_err(repository_error)?;

        let inserted = sqlx::query(
            "INSERT INTO listens \
             (user_id, origin, track_id, listen_at, session_id, start_position_ms, listened_ms, qualified) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8) \
             ON CONFLICT DO NOTHING",
        )
        .bind(submission.principal())
        .bind(submission.origin().as_str())
        .bind(submission.track_id().as_uuid())
        .bind(submission.listen_at())
        .bind(submission.session_id().map(ListenSessionId::as_str))
        .bind(start_position_ms)
        .bind(listened_ms)
        .bind(qualified)
        .execute(&mut *transaction)
        .await
        .map_err(repository_error)?;

        if inserted.rows_affected() == 0 {
            transaction.commit().await.map_err(repository_error)?;

            return Ok(ListenOutcome::Duplicate);
        }

        if submission.counts_as_play() {
            sqlx::query(
                "INSERT INTO track_play_stats (user_id, track_id, play_count, last_played_at) \
                 VALUES ($1, $2, 1, $3) \
                 ON CONFLICT (user_id, track_id) DO UPDATE \
                 SET play_count = track_play_stats.play_count + 1, \
                     last_played_at = EXCLUDED.last_played_at",
            )
            .bind(submission.principal())
            .bind(submission.track_id().as_uuid())
            .bind(submission.listen_at())
            .execute(&mut *transaction)
            .await
            .map_err(repository_error)?;
        }

        transaction.commit().await.map_err(repository_error)?;

        Ok(ListenOutcome::Recorded)
    }

    async fn listen_history(
        &self,
        principal: Uuid,
        query: &ListenQuery,
    ) -> Result<ListenPage, TrackRepositoryError> {
        let window = query.window();
        let marker = listen_marker(&self.pool, principal).await?;
        let coverage = window.coverage(marker);
        let Some(start) = window.covered_start(marker) else {
            return Ok(ListenPage::new(Vec::new(), coverage));
        };

        let rows = sqlx::query_as::<_, ListenRow>(&format!(
            "SELECT {LISTEN_COLUMNS} FROM listens \
             WHERE user_id = $1 AND listen_at >= $2 \
               AND ($3::TIMESTAMPTZ IS NULL OR listen_at < $3) \
             ORDER BY listen_at {order}, origin, track_id, session_id \
             LIMIT $4 OFFSET $5",
            order = order_clause(query.order()),
        ))
        .bind(principal)
        .bind(start)
        .bind(window.until())
        .bind(i64::from(query.limit().get()))
        .bind(i64::from(query.offset()))
        .fetch_all(&self.pool)
        .await
        .map_err(repository_error)?;

        let entries = rows
            .into_iter()
            .map(decode_listen)
            .collect::<Result<Vec<_>, _>>()?;

        Ok(ListenPage::new(entries, coverage))
    }

    async fn listen_count(
        &self,
        principal: Uuid,
        window: ListenWindow,
        track: Option<&TrackId>,
    ) -> Result<ListenCount, TrackRepositoryError> {
        let marker = listen_marker(&self.pool, principal).await?;
        let coverage = window.coverage(marker);
        let Some(start) = window.covered_start(marker) else {
            return Ok(ListenCount::new(0, coverage));
        };

        let count = sqlx::query_scalar::<_, i64>(&format!(
            "SELECT COUNT(*) FROM listens \
             WHERE user_id = $1 AND listen_at >= $2 \
               AND ($3::TIMESTAMPTZ IS NULL OR listen_at < $3) \
               AND ($4::UUID IS NULL OR track_id = $4) \
               AND {COUNTS_AS_PLAY}",
        ))
        .bind(principal)
        .bind(start)
        .bind(window.until())
        .bind(track.map(TrackId::as_uuid))
        .fetch_one(&self.pool)
        .await
        .map_err(repository_error)?;

        Ok(ListenCount::new(
            u64::try_from(count).map_err(|_| TrackRepositoryError::InvalidData)?,
            coverage,
        ))
    }
}
