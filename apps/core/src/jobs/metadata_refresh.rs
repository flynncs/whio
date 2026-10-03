use std::sync::Arc;

use awa::{
    ClientBuilder, JobArgs, JobContext, JobError, JobKindDescriptor, JobResult, QueueDescriptor,
    Worker,
};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use uuid::Uuid;

use crate::features::metadata::{
    domain::MetadataEntityKind,
    refresh::{RefreshReport, RefreshRequest},
    service::{MetadataError, MetadataProfile, MetadataService, RefreshPhase, SubjectSource},
};
use crate::platform::jobs::{Enqueued, JobPhase, JobsError, PendingJob};

pub const METADATA_REFRESH_QUEUE: &str = "whio.metadata.refresh";
pub const METADATA_REFRESH_KIND: &str = "whio.metadata.refresh.v1";
pub const METADATA_REFRESH_MAX_ATTEMPTS: i16 = 5;
pub const METADATA_REFRESH_PRIORITY: i16 = 2;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MetadataRefreshV1 {
    pub operation_id: Uuid,
    pub entity_kind: String,
    pub entity_id: Uuid,
    pub identity_revision: String,
    pub language: String,
    pub profile: String,
}

impl MetadataRefreshV1 {
    pub fn for_artist(
        entity_id: Uuid,
        identity_revision: &str,
        language: &str,
        profile: &str,
    ) -> Self {
        let entity_kind = MetadataEntityKind::Artist.as_str();

        Self {
            operation_id: stable_operation_id(
                entity_kind,
                entity_id,
                identity_revision,
                language,
                profile,
            ),
            entity_kind: entity_kind.to_owned(),
            entity_id,
            identity_revision: identity_revision.to_owned(),
            language: language.to_owned(),
            profile: profile.to_owned(),
        }
    }
}

impl JobArgs for MetadataRefreshV1 {
    fn kind() -> &'static str {
        METADATA_REFRESH_KIND
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ObservedPhase {
    pub job_id: i64,
    pub phase: RefreshPhase,
}

pub async fn request(pool: &PgPool, payload: &MetadataRefreshV1) -> Result<Enqueued, JobsError> {
    crate::platform::jobs::enqueue_coalesced(
        pool,
        PendingJob {
            queue: METADATA_REFRESH_QUEUE,
            payload,
            priority: METADATA_REFRESH_PRIORITY,
            max_attempts: METADATA_REFRESH_MAX_ATTEMPTS,
            operation_id: payload.operation_id,
            coalesce: true,
        },
    )
    .await
}

pub async fn phase(pool: &PgPool, operation_id: Uuid) -> Result<Option<ObservedPhase>, JobsError> {
    crate::platform::jobs::phase(
        pool,
        METADATA_REFRESH_QUEUE,
        METADATA_REFRESH_KIND,
        operation_id,
    )
    .await
    .map(|observed| {
        observed.map(|observed| ObservedPhase {
            job_id: observed.job_id,
            phase: match observed.phase {
                JobPhase::Scheduled => RefreshPhase::Deferred,
                JobPhase::Available => RefreshPhase::Pending,
                JobPhase::Running => RefreshPhase::Running,
            },
        })
    })
}

fn stable_operation_id(
    entity_kind: &str,
    entity_id: Uuid,
    identity_revision: &str,
    language: &str,
    profile: &str,
) -> Uuid {
    let mut basis = String::new();

    for member in [
        entity_kind,
        &entity_id.to_string(),
        identity_revision,
        language,
        profile,
    ] {
        basis.push_str(member);
        basis.push('\u{1f}');
    }

    let digest = Sha256::digest(basis.as_bytes());
    let mut bytes = [0u8; 16];

    bytes.copy_from_slice(&digest[..16]);

    Uuid::from_bytes(bytes)
}

struct MetadataRefreshWorker;

#[async_trait::async_trait]
impl Worker for MetadataRefreshWorker {
    fn kind(&self) -> &'static str {
        METADATA_REFRESH_KIND
    }

    async fn perform(&self, ctx: &JobContext) -> Result<JobResult, JobError> {
        let args: MetadataRefreshV1 =
            serde_json::from_value(ctx.job.args.clone()).map_err(|error| {
                JobError::Terminal(format!(
                    "metadata refresh payload is not applicable: {error}"
                ))
            })?;
        let service = ctx
            .extract::<Arc<MetadataService>>()
            .ok_or_else(|| JobError::terminal("the metadata refresh service is not registered"))?;
        let subjects = ctx
            .extract::<Arc<dyn SubjectSource>>()
            .ok_or_else(|| JobError::terminal("the metadata subject source is not registered"))?;
        let kind = MetadataEntityKind::parse(&args.entity_kind).ok_or_else(|| {
            JobError::terminal(format!(
                "unknown metadata entity kind `{}`",
                args.entity_kind
            ))
        })?;
        let profile = MetadataProfile::parse(&args.profile).ok_or_else(|| {
            JobError::terminal(format!("unknown metadata profile `{}`", args.profile))
        })?;

        let subject = subjects
            .enrichment_subject(kind, args.entity_id)
            .await
            .map_err(|error| job_error(&error))?;

        let Some(subject) = subject else {
            return Ok(JobResult::Cancel(
                "the metadata subject no longer exists".to_owned(),
            ));
        };

        let request = RefreshRequest {
            subject,
            profile,
            language: args.language,
            requested_at: ctx.job.created_at,
            now: Utc::now(),
        };

        match service.refresh(&request).await {
            Ok(RefreshReport::Applied { .. }) => Ok(JobResult::Completed),
            Ok(RefreshReport::Deferred { retry_after, .. }) => Ok(JobResult::Snooze(retry_after)),
            Ok(RefreshReport::IdentityUnestablished) => {
                Ok(JobResult::Cancel("identity_unestablished".to_owned()))
            }
            Err(error) => Err(job_error(&error)),
        }
    }
}

fn job_error(error: &MetadataError) -> JobError {
    match error {
        MetadataError::Unavailable | MetadataError::ProviderUnavailable => {
            JobError::retryable_msg(error)
        }
        MetadataError::Disabled | MetadataError::InvalidData | MetadataError::DisabledSource => {
            JobError::terminal(error.to_string())
        }
    }
}

pub(crate) fn register(builder: ClientBuilder) -> ClientBuilder {
    builder
        .queue_descriptor(
            METADATA_REFRESH_QUEUE,
            QueueDescriptor {
                display_name: Some(METADATA_REFRESH_QUEUE.to_owned()),
                owner: Some("whio-core".to_owned()),
                tags: vec!["metadata".to_owned()],
                ..QueueDescriptor::default()
            },
        )
        .job_kind_descriptor_kind(
            METADATA_REFRESH_KIND,
            JobKindDescriptor {
                display_name: Some(METADATA_REFRESH_KIND.to_owned()),
                owner: Some("whio-core".to_owned()),
                tags: vec!["metadata".to_owned()],
                ..JobKindDescriptor::default()
            },
        )
        .register_worker(MetadataRefreshWorker)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_payload_is_stable_for_one_subject_and_profile() {
        let entity_id = Uuid::now_v7();

        let first = MetadataRefreshV1::for_artist(entity_id, "revision", "en", "artist-display.v1");
        let second =
            MetadataRefreshV1::for_artist(entity_id, "revision", "en", "artist-display.v1");

        assert_eq!(first, second);
        assert_eq!(first.operation_id, second.operation_id);
    }

    #[test]
    fn the_payload_changes_with_every_coalescing_member() {
        let entity_id = Uuid::now_v7();
        let base = MetadataRefreshV1::for_artist(entity_id, "revision", "en", "artist-display.v1");

        for changed in [
            MetadataRefreshV1::for_artist(entity_id, "other", "en", "artist-display.v1"),
            MetadataRefreshV1::for_artist(entity_id, "revision", "fr", "artist-display.v1"),
            MetadataRefreshV1::for_artist(entity_id, "revision", "en", "artist-other.v1"),
            MetadataRefreshV1::for_artist(Uuid::now_v7(), "revision", "en", "artist-display.v1"),
        ] {
            assert_ne!(base.operation_id, changed.operation_id);
        }

        assert_eq!(base.entity_kind, MetadataEntityKind::Artist.as_str());
        assert_eq!(base.profile, "artist-display.v1");
        assert_eq!(base.language, "en");
    }
}
