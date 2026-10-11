use std::sync::Arc;

use awa::{
    ClientBuilder, JobArgs, JobContext, JobError, JobKindDescriptor, JobResult, QueueDescriptor,
    Worker,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::retry::{RetryPolicy, RetryableWorker};

use crate::features::acquisition::service::{AcquisitionError, AcquisitionService, Step};

pub const ACQUISITION_QUEUE: &str = "whio.acquisition";
pub const ACQUISITION_KIND: &str = "whio.acquisition.advance.v1";
pub const ACQUISITION_PRIORITY: i16 = 2;
pub const ACQUISITION_MAX_ATTEMPTS: i16 = 8;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AcquisitionAdvanceV1 {
    pub operation_id: Uuid,
    pub request_id: Uuid,
}

impl AcquisitionAdvanceV1 {
    pub fn for_request(request_id: Uuid) -> Self {
        Self {
            operation_id: request_id,
            request_id,
        }
    }
}

impl JobArgs for AcquisitionAdvanceV1 {
    fn kind() -> &'static str {
        ACQUISITION_KIND
    }
}

struct AcquisitionWorker;

#[async_trait::async_trait]
impl Worker for AcquisitionWorker {
    fn kind(&self) -> &'static str {
        ACQUISITION_KIND
    }

    async fn perform(&self, ctx: &JobContext) -> Result<JobResult, JobError> {
        let args: AcquisitionAdvanceV1 =
            serde_json::from_value(ctx.job.args.clone()).map_err(|error| {
                JobError::Terminal(format!("acquisition payload is not applicable: {error}"))
            })?;
        let service = ctx
            .extract::<Arc<AcquisitionService>>()
            .ok_or_else(|| JobError::terminal("the acquisition service is not registered"))?;

        match service.advance(args.request_id).await {
            Ok(Step::Wait(duration)) => Ok(JobResult::Snooze(duration)),
            Ok(Step::Done) => Ok(JobResult::Completed),
            Err(AcquisitionError::Unavailable) => Err(JobError::retryable_msg(
                "an acquisition provider is unavailable",
            )),
            Err(AcquisitionError::Repository(error)) => {
                tracing::error!(%error, "acquisition storage failed");

                Err(JobError::terminal("acquisition storage failed"))
            }
            Err(AcquisitionError::NotFound) => {
                Ok(JobResult::Cancel("the request no longer exists".to_owned()))
            }
            Err(AcquisitionError::Failed) => Err(JobError::terminal(
                "an acquisition provider rejected the request",
            )),
        }
    }
}

pub(crate) fn register(builder: ClientBuilder, retry_policy: RetryPolicy) -> ClientBuilder {
    builder
        .queue_descriptor(
            ACQUISITION_QUEUE,
            QueueDescriptor {
                display_name: Some(ACQUISITION_QUEUE.to_owned()),
                owner: Some("whio-core".to_owned()),
                tags: vec!["acquisition".to_owned()],
                ..QueueDescriptor::default()
            },
        )
        .job_kind_descriptor_kind(
            ACQUISITION_KIND,
            JobKindDescriptor {
                display_name: Some(ACQUISITION_KIND.to_owned()),
                owner: Some("whio-core".to_owned()),
                tags: vec!["acquisition".to_owned()],
                ..JobKindDescriptor::default()
            },
        )
        .register_worker(RetryableWorker::new(AcquisitionWorker, retry_policy))
}
