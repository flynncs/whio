use async_trait::async_trait;
use uuid::Uuid;

use crate::features::metadata::domain::{
    AttemptError, FieldId, FieldWrite, MetadataEntityKind, SubjectFacts, SubjectKey,
};
use crate::features::music_library::domain::EnrichmentSubject;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommitOutcome {
    Written,
    Obsolete,
    SubjectGone,
}

#[derive(Debug, thiserror::Error)]
pub enum MetadataRepositoryError {
    #[error("metadata repository is unavailable")]
    Unavailable,
    #[error("metadata repository contained invalid data")]
    InvalidData,
}

#[async_trait]
pub trait MetadataStore: Send + Sync {
    async fn read_subject(&self, key: &SubjectKey)
    -> Result<SubjectFacts, MetadataRepositoryError>;

    async fn commit_field(
        &self,
        subject: &EnrichmentSubject,
        write: &FieldWrite,
    ) -> Result<CommitOutcome, MetadataRepositoryError>;

    async fn record_failure(
        &self,
        subject: &EnrichmentSubject,
        field: FieldId,
        language: &str,
        source_provider_id: &str,
        error: &AttemptError,
    ) -> Result<(), MetadataRepositoryError>;

    async fn delete_subject_facts(
        &self,
        kind: MetadataEntityKind,
        ids: &[Uuid],
    ) -> Result<u64, MetadataRepositoryError>;
}
