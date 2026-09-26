use std::sync::Arc;

use crate::features::music_library::{
    domain::{SourceIdentity, TrackId},
    storage::contracts::{SourceLookup, TrackRepositoryError},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum SourceQueryError {
    #[error("music library is unavailable")]
    Unavailable,
    #[error("music library contains invalid source data")]
    InvalidData,
}

impl From<TrackRepositoryError> for SourceQueryError {
    fn from(error: TrackRepositoryError) -> Self {
        match error {
            TrackRepositoryError::Unavailable => Self::Unavailable,
            TrackRepositoryError::InvalidData => Self::InvalidData,
        }
    }
}

/// Recorded library facts, not a promise that a file can still be opened.
pub struct RecordedLocalFile {
    pub path: Option<String>,
    pub missing: bool,
    pub content_type: Option<String>,
    pub suffix: Option<String>,
    pub container: Option<String>,
    pub codec: Option<String>,
    pub bitrate_kbps: Option<u32>,
    pub sample_rate_hz: Option<u32>,
    pub bit_depth: Option<u8>,
    pub channels: Option<u8>,
}

pub struct SourceQueries {
    reader: Arc<dyn SourceLookup>,
}

impl SourceQueries {
    pub fn new(reader: Arc<dyn SourceLookup>) -> Self {
        Self { reader }
    }

    pub async fn candidates(
        &self,
        track: &TrackId,
    ) -> Result<Vec<SourceIdentity>, SourceQueryError> {
        self.reader.find_sources(track).await.map_err(Into::into)
    }

    pub async fn recorded_local_file(
        &self,
        source: &SourceIdentity,
    ) -> Result<Option<RecordedLocalFile>, SourceQueryError> {
        self.reader
            .find_source_file_facts(source)
            .await
            .map(|facts| {
                facts.map(|facts| RecordedLocalFile {
                    path: facts.path,
                    missing: facts.missing_at.is_some(),
                    content_type: facts.content_type,
                    suffix: facts.suffix,
                    container: facts.container,
                    codec: facts.codec,
                    bitrate_kbps: facts.bitrate_kbps,
                    sample_rate_hz: facts.sample_rate_hz,
                    bit_depth: facts.bit_depth,
                    channels: facts.channels,
                })
            })
            .map_err(Into::into)
    }
}
