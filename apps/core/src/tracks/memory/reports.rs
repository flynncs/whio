use async_trait::async_trait;

use crate::tracks::DuplicateReport;

use super::InMemoryTrackRepository;

#[async_trait]
impl DuplicateReport for InMemoryTrackRepository {}
