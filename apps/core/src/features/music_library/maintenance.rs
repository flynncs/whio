use std::sync::Arc;

use super::{
    domain::{
        DuplicateCandidate, EvictionReport, ProviderDuplicateReport, RetentionReport, SplitArtist,
    },
    storage::contracts::{DuplicateReport, Retention, TrackRepositoryError},
};

#[derive(Debug, thiserror::Error)]
pub enum MaintenanceError {
    #[error("music library is unavailable")]
    Unavailable,
    #[error("music library contains invalid data")]
    InvalidData,
}

impl From<TrackRepositoryError> for MaintenanceError {
    fn from(error: TrackRepositoryError) -> Self {
        match error {
            TrackRepositoryError::Unavailable => Self::Unavailable,
            TrackRepositoryError::InvalidData | TrackRepositoryError::UnknownLibraryId { .. } => {
                Self::InvalidData
            }
        }
    }
}

pub struct MaintenanceService {
    reports: Arc<dyn DuplicateReport>,
    retention: Arc<dyn Retention>,
}

impl MaintenanceService {
    pub(super) fn new(reports: Arc<dyn DuplicateReport>, retention: Arc<dyn Retention>) -> Self {
        Self { reports, retention }
    }

    pub async fn duplicates(
        &self,
        limit: u32,
    ) -> Result<Vec<DuplicateCandidate>, MaintenanceError> {
        self.reports
            .duplicate_candidates(limit)
            .await
            .map_err(Into::into)
    }

    pub async fn split_artists(&self, limit: u32) -> Result<Vec<SplitArtist>, MaintenanceError> {
        self.reports.split_artists(limit).await.map_err(Into::into)
    }

    pub async fn retention_report(&self) -> Result<RetentionReport, MaintenanceError> {
        self.retention.retention_report().await.map_err(Into::into)
    }

    pub async fn evict_unengaged(&self) -> Result<EvictionReport, MaintenanceError> {
        self.retention.evict_unengaged().await.map_err(Into::into)
    }

    #[tracing::instrument(skip_all, fields(operation = "provider_duplicate_scan"))]
    pub async fn provider_duplicates(&self) -> Result<ProviderDuplicateReport, MaintenanceError> {
        self.retention
            .provider_duplicates()
            .await
            .map_err(Into::into)
    }

    #[tracing::instrument(skip_all, fields(operation = "provider_duplicate_delete"))]
    pub async fn delete_provider_duplicates(
        &self,
    ) -> Result<ProviderDuplicateReport, MaintenanceError> {
        self.retention
            .delete_provider_duplicates()
            .await
            .map_err(Into::into)
    }
}
