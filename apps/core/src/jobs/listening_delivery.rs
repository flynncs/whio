use std::sync::Arc;
use std::time::Duration;

use awa::{
    ClientBuilder, JobArgs, JobContext, JobError, JobKindDescriptor, JobResult, QueueDescriptor,
    Worker,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::features::music_library::delivery::{
    DeliveryError, DeliveryOutcome, DeliveryService, ListenIdentity, RATE_LIMIT_RETRY_AFTER,
    SNOOZE_DEADLINE,
};

pub const LISTEN_DELIVERY_QUEUE: &str = "whio.listening.delivery";
pub const LISTEN_DELIVERY_KIND: &str = "whio.listening.deliver.v1";
pub const LISTEN_DELIVERY_MAX_ATTEMPTS: i16 = 8;
pub const LISTEN_DELIVERY_PRIORITY: i16 = 2;
pub const LISTEN_DELIVERY_DEADLINE: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ListeningDeliveryV1 {
    pub operation_id: Uuid,
    pub delivery_id: Uuid,
}

impl ListeningDeliveryV1 {
    pub fn for_delivery(listen: &ListenIdentity, destination_id: Uuid, delivery_id: Uuid) -> Self {
        Self {
            operation_id: stable_operation_id(listen, destination_id),
            delivery_id,
        }
    }
}

impl JobArgs for ListeningDeliveryV1 {
    fn kind() -> &'static str {
        LISTEN_DELIVERY_KIND
    }
}

fn stable_operation_id(listen: &ListenIdentity, destination_id: Uuid) -> Uuid {
    let mut basis = String::new();

    for member in [
        listen.user_id.to_string(),
        listen.origin.clone(),
        listen.track_id.to_string(),
        listen.listen_at.timestamp_micros().to_string(),
        listen.session_key.clone(),
        destination_id.to_string(),
    ] {
        basis.push_str(&member);
        basis.push('\u{1f}');
    }

    let digest = Sha256::digest(basis.as_bytes());
    let mut bytes = [0u8; 16];

    bytes.copy_from_slice(&digest[..16]);

    Uuid::from_bytes(bytes)
}

struct ListeningDeliveryWorker;

#[async_trait::async_trait]
impl Worker for ListeningDeliveryWorker {
    fn kind(&self) -> &'static str {
        LISTEN_DELIVERY_KIND
    }

    async fn perform(&self, ctx: &JobContext) -> Result<JobResult, JobError> {
        let args: ListeningDeliveryV1 =
            serde_json::from_value(ctx.job.args.clone()).map_err(|error| {
                JobError::Terminal(format!(
                    "listen delivery payload is not applicable: {error}"
                ))
            })?;
        let service = ctx
            .extract::<Arc<DeliveryService>>()
            .ok_or_else(|| JobError::terminal("the listen delivery service is not registered"))?;

        match service.deliver(args.delivery_id).await {
            Ok(DeliveryOutcome::Delivered)
            | Ok(DeliveryOutcome::Ignored)
            | Ok(DeliveryOutcome::Rejected)
            | Ok(DeliveryOutcome::AlreadyTerminal) => Ok(JobResult::Completed),
            Ok(DeliveryOutcome::Retryable) => Err(JobError::retryable_msg(
                "the listen provider did not answer",
            )),
            Err(DeliveryError::RateLimited { retry_after }) => {
                let delay = retry_after.unwrap_or(RATE_LIMIT_RETRY_AFTER);
                let deadline = ctx.job.created_at
                    + chrono::Duration::from_std(SNOOZE_DEADLINE)
                        .map_err(|error| JobError::terminal(error.to_string()))?;
                let remaining = deadline
                    .signed_duration_since(chrono::Utc::now())
                    .to_std()
                    .unwrap_or_default();

                Ok(JobResult::Snooze(
                    delay.min(remaining).max(Duration::from_secs(1)),
                ))
            }
            Err(DeliveryError::ProviderUnavailable) => Err(JobError::retryable_msg(
                "the listen provider is unavailable",
            )),
            Err(DeliveryError::Repository(error)) => Err(JobError::retryable_msg(error)),
            Err(DeliveryError::Disabled) => {
                Ok(JobResult::Cancel("the destination is disabled".to_owned()))
            }
            Err(DeliveryError::CredentialMissing) => Ok(JobResult::Cancel(
                "the credential store is not configured".to_owned(),
            )),
            Err(DeliveryError::CredentialRejected) => Err(JobError::terminal(
                "the destination rejected the credential",
            )),
            Err(DeliveryError::InvalidData) => Err(JobError::terminal(
                "the accepted listen cannot be delivered",
            )),
            Err(DeliveryError::Unavailable) => {
                Err(JobError::terminal("listen delivery is not configured"))
            }
        }
    }
}

pub(crate) fn register(builder: ClientBuilder) -> ClientBuilder {
    builder
        .queue_descriptor(
            LISTEN_DELIVERY_QUEUE,
            QueueDescriptor {
                display_name: Some(LISTEN_DELIVERY_QUEUE.to_owned()),
                owner: Some("whio-core".to_owned()),
                tags: vec!["listening".to_owned()],
                ..QueueDescriptor::default()
            },
        )
        .job_kind_descriptor_kind(
            LISTEN_DELIVERY_KIND,
            JobKindDescriptor {
                display_name: Some(LISTEN_DELIVERY_KIND.to_owned()),
                owner: Some("whio-core".to_owned()),
                tags: vec!["listening".to_owned()],
                ..JobKindDescriptor::default()
            },
        )
        .register_worker(ListeningDeliveryWorker)
}
