use awa::{
    ClientBuilder, JobArgs, JobContext, JobError, JobKindDescriptor, JobResult, QueueDescriptor,
    Worker,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::platform::jobs::{Enqueued, JobsError, PendingJob};

use super::retry::{RetryPolicy, RetryableWorker};

pub const PROBE_QUEUE: &str = "whio.jobs.probe";
pub const PROBE_KIND: &str = "whio.jobs.probe.v1";
pub const PROBE_SLOW_KIND: &str = "whio.jobs.probe.slow.v1";
pub const PROBE_FAILING_KIND: &str = "whio.jobs.probe.fail.v1";

const PROBE_RESULT: &str = PROBE_KIND;
const PROBE_PRIORITY: i16 = 2;
const PROBE_MAX_ATTEMPTS: i16 = 5;
pub const PROBE_FAILING_MAX_ATTEMPTS: i16 = 5;
const PROBE_SLOW_MAX_ATTEMPTS: i16 = 5;

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ProbeV1 {
    pub(crate) operation_id: Uuid,
}

impl JobArgs for ProbeV1 {
    fn kind() -> &'static str {
        PROBE_KIND
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SlowProbeV1 {
    pub(crate) operation_id: Uuid,
    #[serde(default)]
    pub(crate) ignore_cancel: bool,
}

impl JobArgs for SlowProbeV1 {
    fn kind() -> &'static str {
        PROBE_SLOW_KIND
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct FailProbeV1 {
    pub(crate) operation_id: Uuid,
}

impl JobArgs for FailProbeV1 {
    fn kind() -> &'static str {
        PROBE_FAILING_KIND
    }
}

struct ProbeWorker;

#[async_trait::async_trait]
impl Worker for ProbeWorker {
    fn kind(&self) -> &'static str {
        PROBE_KIND
    }

    async fn perform(&self, ctx: &JobContext) -> Result<JobResult, JobError> {
        let args: ProbeV1 = serde_json::from_value(ctx.job.args.clone()).map_err(|error| {
            JobError::Terminal(format!("probe payload is not applicable: {error}"))
        })?;

        if applied_operation(ctx.job.progress.as_ref()) == Some(args.operation_id) {
            tracing::info!(
                job_id = ctx.job.id,
                operation_id = %args.operation_id,
                "probe job already applied"
            );
            return Ok(JobResult::Completed);
        }

        ctx.update_metadata(result_record(args.operation_id))
            .map_err(JobError::retryable)?;
        ctx.set_progress(100, "applied");
        ctx.flush_progress().await.map_err(JobError::retryable)?;

        tracing::info!(
            job_id = ctx.job.id,
            operation_id = %args.operation_id,
            "probe job applied"
        );

        Ok(JobResult::Completed)
    }
}

struct SlowProbeWorker;

#[async_trait::async_trait]
impl Worker for SlowProbeWorker {
    fn kind(&self) -> &'static str {
        PROBE_SLOW_KIND
    }

    async fn perform(&self, ctx: &JobContext) -> Result<JobResult, JobError> {
        let args: SlowProbeV1 = serde_json::from_value(ctx.job.args.clone()).map_err(|error| {
            JobError::Terminal(format!("slow probe payload is not applicable: {error}"))
        })?;

        if applied_operation(ctx.job.progress.as_ref()) == Some(args.operation_id) {
            tracing::info!(
                job_id = ctx.job.id,
                operation_id = %args.operation_id,
                "probe job already applied"
            );
            return Ok(JobResult::Completed);
        }

        apply_effect(ctx.pool(), args.operation_id)
            .await
            .map_err(JobError::retryable)?;
        ctx.update_metadata(result_record(args.operation_id))
            .map_err(JobError::retryable)?;
        ctx.set_progress(100, "applied");
        ctx.flush_progress().await.map_err(JobError::retryable)?;

        tracing::info!(
            job_id = ctx.job.id,
            operation_id = %args.operation_id,
            "probe job applied"
        );

        loop {
            let released: bool = sqlx::query_scalar(
                "SELECT EXISTS (SELECT 1 FROM public.probe_releases WHERE operation_id = $1)",
            )
            .bind(args.operation_id)
            .fetch_one(ctx.pool())
            .await
            .map_err(JobError::retryable)?;
            if released || ctx.is_cancelled() && !args.ignore_cancel {
                return Ok(JobResult::Completed);
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    }
}

struct FailProbeWorker;

#[async_trait::async_trait]
impl Worker for FailProbeWorker {
    fn kind(&self) -> &'static str {
        PROBE_FAILING_KIND
    }

    async fn perform(&self, ctx: &JobContext) -> Result<JobResult, JobError> {
        let args: FailProbeV1 = serde_json::from_value(ctx.job.args.clone()).map_err(|error| {
            JobError::Terminal(format!("failing probe payload is not applicable: {error}"))
        })?;
        record_retry_attempt(ctx, args.operation_id).await?;

        Err(JobError::retryable_msg("failing probe always fails"))
    }
}

fn result_record(operation_id: Uuid) -> serde_json::Value {
    serde_json::json!({
        PROBE_RESULT: {
            "operation_id": operation_id.to_string(),
            "transitions": 1,
        }
    })
}

fn applied_operation(progress: Option<&serde_json::Value>) -> Option<Uuid> {
    progress?
        .get("metadata")?
        .get(PROBE_RESULT)?
        .get("operation_id")?
        .as_str()?
        .parse()
        .ok()
}

async fn record_retry_attempt(ctx: &JobContext, operation_id: Uuid) -> Result<(), JobError> {
    let mut attempts = ctx
        .job
        .progress
        .as_ref()
        .and_then(|progress| progress.get("metadata"))
        .and_then(|metadata| metadata.get("probe_retry_attempts"))
        .and_then(serde_json::Value::as_array)
        .cloned()
        .unwrap_or_default();
    attempts.push(serde_json::json!({
        "operation_id": operation_id,
        "attempt": ctx.job.attempt,
        "at": chrono::Utc::now(),
    }));
    ctx.update_metadata(serde_json::json!({ "probe_retry_attempts": attempts }))
        .map_err(JobError::retryable)?;
    ctx.flush_progress().await.map_err(JobError::retryable)
}

async fn apply_effect(pool: &sqlx::PgPool, operation_id: Uuid) -> Result<(), sqlx::Error> {
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS public.probe_effects (
            operation_id UUID PRIMARY KEY,
            transitions INT NOT NULL
        )",
    )
    .execute(pool)
    .await?;
    sqlx::query("CREATE TABLE IF NOT EXISTS public.probe_releases (operation_id UUID PRIMARY KEY)")
        .execute(pool)
        .await?;

    sqlx::query(
        "INSERT INTO public.probe_effects (operation_id, transitions) VALUES ($1, 1)
         ON CONFLICT (operation_id)
         DO UPDATE SET transitions = public.probe_effects.transitions + 1",
    )
    .bind(operation_id)
    .execute(pool)
    .await?;

    Ok(())
}

pub async fn enqueue(
    tx: &mut sqlx::PgConnection,
    operation_id: Uuid,
) -> Result<Enqueued, JobsError> {
    crate::platform::jobs::enqueue(
        tx,
        PendingJob {
            queue: PROBE_QUEUE,
            payload: &ProbeV1 { operation_id },
            priority: PROBE_PRIORITY,
            max_attempts: PROBE_MAX_ATTEMPTS,
            operation_id,
            coalesce: true,
        },
    )
    .await
}

pub async fn enqueue_coalesced(
    pool: &sqlx::PgPool,
    operation_id: Uuid,
) -> Result<Enqueued, JobsError> {
    crate::platform::jobs::enqueue_coalesced(
        pool,
        PendingJob {
            queue: PROBE_QUEUE,
            payload: &ProbeV1 { operation_id },
            priority: PROBE_PRIORITY,
            max_attempts: PROBE_MAX_ATTEMPTS,
            operation_id,
            coalesce: true,
        },
    )
    .await
}

pub async fn enqueue_slow(
    tx: &mut sqlx::PgConnection,
    operation_id: Uuid,
    ignore_cancel: bool,
) -> Result<Enqueued, JobsError> {
    crate::platform::jobs::enqueue(
        tx,
        PendingJob {
            queue: PROBE_QUEUE,
            payload: &SlowProbeV1 {
                operation_id,
                ignore_cancel,
            },
            priority: PROBE_PRIORITY,
            max_attempts: PROBE_SLOW_MAX_ATTEMPTS,
            operation_id,
            coalesce: true,
        },
    )
    .await
}

pub async fn enqueue_failing(
    tx: &mut sqlx::PgConnection,
    operation_id: Uuid,
) -> Result<Enqueued, JobsError> {
    crate::platform::jobs::enqueue(
        tx,
        PendingJob {
            queue: PROBE_QUEUE,
            payload: &FailProbeV1 { operation_id },
            priority: PROBE_PRIORITY,
            max_attempts: PROBE_FAILING_MAX_ATTEMPTS,
            operation_id,
            coalesce: false,
        },
    )
    .await
}

pub(crate) fn register(builder: ClientBuilder, retry_policy: RetryPolicy) -> ClientBuilder {
    builder
        .queue_descriptor(
            PROBE_QUEUE,
            QueueDescriptor {
                display_name: Some(PROBE_QUEUE.to_owned()),
                owner: Some("whio-core".to_owned()),
                tags: vec!["probe".to_owned()],
                ..QueueDescriptor::default()
            },
        )
        .job_kind_descriptor_kind(
            PROBE_KIND,
            JobKindDescriptor {
                display_name: Some(PROBE_KIND.to_owned()),
                owner: Some("whio-core".to_owned()),
                tags: vec!["probe".to_owned()],
                ..JobKindDescriptor::default()
            },
        )
        .register_worker(RetryableWorker::new(ProbeWorker, retry_policy))
        .job_kind_descriptor_kind(
            PROBE_SLOW_KIND,
            JobKindDescriptor {
                display_name: Some(PROBE_SLOW_KIND.to_owned()),
                owner: Some("whio-core".to_owned()),
                tags: vec!["probe".to_owned()],
                ..JobKindDescriptor::default()
            },
        )
        .register_worker(RetryableWorker::new(SlowProbeWorker, retry_policy))
        .job_kind_descriptor_kind(
            PROBE_FAILING_KIND,
            JobKindDescriptor {
                display_name: Some(PROBE_FAILING_KIND.to_owned()),
                owner: Some("whio-core".to_owned()),
                tags: vec!["probe".to_owned()],
                ..JobKindDescriptor::default()
            },
        )
        .register_worker(RetryableWorker::new(FailProbeWorker, retry_policy))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_recorded_result_names_its_operation() {
        let operation_id = Uuid::now_v7();
        let progress = serde_json::json!({ "metadata": result_record(operation_id) });

        assert_eq!(applied_operation(Some(&progress)), Some(operation_id));
        assert_eq!(
            applied_operation(Some(&serde_json::json!({
                "metadata": result_record(Uuid::now_v7())
            })))
            .map(|applied| applied == operation_id),
            Some(false)
        );
        assert_eq!(applied_operation(Some(&serde_json::json!({}))), None);
        assert_eq!(applied_operation(None), None);
    }
}
