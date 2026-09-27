use awa::{
    ClientBuilder, JobArgs, JobContext, JobError, JobKindDescriptor, JobResult, QueueDescriptor,
    Worker,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::platform::jobs::{Enqueued, JobsError, PendingJob};

pub const PROBE_QUEUE: &str = "whio.jobs.probe";
pub const PROBE_KIND: &str = "whio.jobs.probe.v1";

const PROBE_RESULT: &str = "whio.jobs.probe.v1";
const PROBE_PRIORITY: i16 = 2;
const PROBE_MAX_ATTEMPTS: i16 = 5;

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

pub(crate) fn register(builder: ClientBuilder) -> ClientBuilder {
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
        .register_worker(ProbeWorker)
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
