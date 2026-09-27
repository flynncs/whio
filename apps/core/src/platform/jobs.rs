use awa::{AwaError, InsertOpts, JobArgs, UniqueOpts, insert_with};
use sqlx::PgConnection;
use thiserror::Error;
use uuid::Uuid;

pub const NON_TERMINAL_UNIQUE_STATES: u8 = 0b0001_0111;

#[derive(Debug, Error)]
pub enum JobsError {
    #[error("job enqueue failed")]
    Enqueue(#[source] AwaError),

    #[error("enqueue savepoint failed")]
    Savepoint(#[source] sqlx::Error),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Enqueued {
    Inserted { job_id: i64 },
    Coalesced,
}

pub struct PendingJob<'a, T> {
    pub queue: &'a str,
    pub payload: &'a T,
    pub priority: i16,
    pub max_attempts: i16,
    pub operation_id: Uuid,
    pub coalesce: bool,
}

const COALESCE_SAVEPOINT: &str = "whio_job_enqueue";

pub async fn enqueue<T: JobArgs>(
    tx: &mut PgConnection,
    job: PendingJob<'_, T>,
) -> Result<Enqueued, JobsError> {
    let mut options = InsertOpts {
        queue: job.queue.to_owned(),
        priority: job.priority,
        max_attempts: job.max_attempts,
        metadata: serde_json::json!({ "operation_id": job.operation_id.to_string() }),
        ..InsertOpts::default()
    };
    if job.coalesce {
        options.unique = Some(UniqueOpts {
            by_queue: true,
            by_args: true,
            by_period: None,
            states: NON_TERMINAL_UNIQUE_STATES,
        });

        savepoint(tx, "SAVEPOINT").await?;
    }

    match insert_with(&mut *tx, job.payload, options).await {
        Ok(row) => {
            if job.coalesce {
                savepoint(tx, "RELEASE SAVEPOINT").await?;
            }
            Ok(Enqueued::Inserted { job_id: row.id })
        }
        Err(AwaError::UniqueConflict { .. }) if job.coalesce => {
            savepoint(tx, "ROLLBACK TO SAVEPOINT").await?;
            savepoint(tx, "RELEASE SAVEPOINT").await?;
            Ok(Enqueued::Coalesced)
        }
        Err(error) => Err(JobsError::Enqueue(error)),
    }
}

async fn savepoint(tx: &mut PgConnection, statement: &'static str) -> Result<(), JobsError> {
    sqlx::query(&format!("{statement} {COALESCE_SAVEPOINT}"))
        .execute(&mut *tx)
        .await
        .map_err(JobsError::Savepoint)?;
    Ok(())
}
