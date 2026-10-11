#[derive(Debug, thiserror::Error)]
pub enum TrackRepositoryError {
    #[error("track repository is unavailable")]
    Unavailable,
    #[error("track repository contained invalid data")]
    InvalidData,
    #[error("library {id} is not registered")]
    UnknownLibraryId { id: String },
    #[error("the track repository does not support this read")]
    Unsupported,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceFileFacts {
    pub path: Option<String>,
    pub size_bytes: u64,
    pub modified_at: Option<chrono::DateTime<chrono::Utc>>,
    pub missing_at: Option<chrono::DateTime<chrono::Utc>>,
    pub content_type: Option<String>,
    pub suffix: Option<String>,
    pub container: Option<String>,
    pub codec: Option<String>,
    pub bitrate_kbps: Option<u32>,
    pub sample_rate_hz: Option<u32>,
    pub bit_depth: Option<u8>,
    pub channels: Option<u8>,
    pub facts_version: u16,
    pub acoustid_fingerprint: Option<String>,
}
