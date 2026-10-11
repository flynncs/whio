use std::path::PathBuf;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchText(String);

impl SearchText {
    pub fn new(value: &str) -> Option<Self> {
        let value = value.trim();

        if value.is_empty() {
            return None;
        }

        Some(Self(value.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CandidateLimit(u16);

impl CandidateLimit {
    pub fn new(limit: u16) -> Option<Self> {
        (1..=500).contains(&limit).then_some(Self(limit))
    }

    pub fn get(self) -> u16 {
        self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum TransferInput {
    SoulseekFile {
        peer: String,
        path: String,
        size_bytes: u64,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransferRequest {
    pub key: Uuid,
    pub input: TransferInput,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ReleaseCandidate {
    pub source: String,
    pub peer: PeerFacts,
    pub files: Vec<CandidateFile>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PeerFacts {
    pub queue_length: Option<u32>,
    pub upload_speed: Option<u64>,
    pub has_free_slot: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CandidateFile {
    pub input: TransferInput,
    pub name: String,
    pub facts: FileFacts,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct FileFacts {
    pub size_bytes: Option<u64>,
    pub extension: Option<String>,
    pub bitrate_kbps: Option<u32>,
    pub sample_rate_hz: Option<u32>,
    pub bit_depth: Option<u32>,
    pub duration_seconds: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransferHandle(String);

impl TransferHandle {
    pub fn new(value: String) -> Self {
        Self(value)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Slot<T> {
    Accepted(T),
    Rejected(Rejection),
    Missing,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rejection {
    Invalid,
    Conflict,
    Refused,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransferPhase {
    Queued,
    Active,
    ReportedComplete,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransferProgress {
    pub phase: TransferPhase,
    pub bytes_done: Option<u64>,
    pub bytes_total: Option<u64>,
    pub observed_at: DateTime<Utc>,
    pub delivered: Option<PathBuf>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendHealth {
    Ready,
    NotLoggedIn,
    Unavailable,
    CredentialsRejected,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum TransferError {
    #[error("the transfer backend is unavailable")]
    Unavailable,
    #[error("the transfer request is invalid")]
    InvalidRequest,
    #[error("the transfer backend does not support this request")]
    Unsupported,
    #[error("the transfer backend answered with a malformed body")]
    Malformed,
    #[error("the transfer backend rejected the credentials")]
    CredentialsRejected,
    #[error("the transfer backend failed internally")]
    Internal,
}

#[async_trait]
pub trait CandidateSearch: Send + Sync {
    async fn search(
        &self,
        query: &SearchText,
        limit: CandidateLimit,
    ) -> Result<Vec<ReleaseCandidate>, TransferError>;
}

#[async_trait]
pub trait RemoteTransfer: Send + Sync {
    async fn start(
        &self,
        requests: &[TransferRequest],
    ) -> Result<Vec<Slot<TransferHandle>>, TransferError>;

    async fn status(
        &self,
        handles: &[TransferHandle],
    ) -> Result<Vec<Slot<TransferProgress>>, TransferError>;

    async fn cancel(&self, handle: &TransferHandle, remove: bool) -> Result<(), TransferError>;

    async fn health(&self) -> BackendHealth;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MbId(Uuid);

impl MbId {
    pub fn new(id: Uuid) -> Self {
        Self(id)
    }

    pub fn as_uuid(&self) -> Uuid {
        self.0
    }

    pub fn parse(value: &str) -> Option<Self> {
        Uuid::parse_str(value).ok().map(Self)
    }
}

impl std::fmt::Display for MbId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}", self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MbRelease {
    pub id: MbId,
    pub artist: String,
    pub title: String,
    pub tracks: Vec<ExpectedTrack>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MbRecording {
    pub id: MbId,
    pub artist: String,
    pub title: String,
    pub length_ms: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ExpectedTrack {
    pub disc: u16,
    pub position: u16,
    pub title: String,
    pub length_ms: Option<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum LookupError {
    #[error("the lookup service is unavailable")]
    Unavailable,
    #[error("the lookup service answered with a malformed body")]
    Malformed,
}

#[async_trait]
pub trait MusicBrainzLookup: Send + Sync {
    async fn release_candidates(
        &self,
        artist: &str,
        title: &str,
    ) -> Result<Vec<MbRelease>, LookupError>;

    async fn release(&self, id: &MbId) -> Result<Option<MbRelease>, LookupError>;

    async fn recording_candidates(
        &self,
        artist: &str,
        title: &str,
    ) -> Result<Vec<MbRecording>, LookupError>;

    async fn recording(&self, id: &MbId) -> Result<Option<MbRecording>, LookupError>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AcquisitionTarget {
    Release(MbId),
    Recording(MbId),
}

impl AcquisitionTarget {
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Release(_) => "release",
            Self::Recording(_) => "recording",
        }
    }

    pub fn id(&self) -> MbId {
        match self {
            Self::Release(id) | Self::Recording(id) => *id,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportJob {
    pub target: AcquisitionTarget,
    pub path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportOutcome {
    pub items: Vec<ImportedItem>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ImportedItem {
    pub release_id: Option<String>,
    pub recording_id: Option<String>,
    pub path: PathBuf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ImportError {
    #[error("the importer exited with status {status:?}")]
    Failed { status: Option<i32> },
    #[error("the importer exceeded its deadline")]
    TimedOut,
    #[error("the importer stopped producing output")]
    Stalled,
    #[error("the importer is unavailable")]
    Unavailable,
}

#[async_trait]
pub trait Importer: Send + Sync {
    async fn import(&self, job: &ImportJob) -> Result<ImportOutcome, ImportError>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum OwnershipError {
    #[error("the library is unavailable")]
    Unavailable,
    #[error("the library could not answer the ownership query")]
    Failed,
}

#[async_trait]
pub trait LibraryOwnership: Send + Sync {
    async fn owns(&self, target: &AcquisitionTarget) -> Result<bool, OwnershipError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn candidate_limit_accepts_1_to_500() {
        assert!(CandidateLimit::new(0).is_none());
        assert_eq!(CandidateLimit::new(1).unwrap().get(), 1);
        assert_eq!(CandidateLimit::new(500).unwrap().get(), 500);
        assert!(CandidateLimit::new(501).is_none());
    }

    #[test]
    fn search_text_trims_and_rejects_blank() {
        assert_eq!(SearchText::new("  a b ").unwrap().as_str(), "a b");
        assert!(SearchText::new("   ").is_none());
        assert!(SearchText::new("").is_none());
    }
}
