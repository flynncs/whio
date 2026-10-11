use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::features::music_library::domain::{AlbumId, TrackId};

use super::ports::{
    AcquisitionTarget, CandidateFile, ExpectedTrack, ImportedItem, MbId, ReleaseCandidate,
    TransferInput,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OriginKind {
    Album,
    Track,
    Release,
    Recording,
}

impl OriginKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Album => "album",
            Self::Track => "track",
            Self::Release => "release",
            Self::Recording => "recording",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "album" => Some(Self::Album),
            "track" => Some(Self::Track),
            "release" => Some(Self::Release),
            "recording" => Some(Self::Recording),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AcquisitionOrigin {
    Album(AlbumId),
    Track(TrackId),
    Release(MbId),
    Recording(MbId),
}

impl AcquisitionOrigin {
    pub fn kind(&self) -> OriginKind {
        match self {
            Self::Album(_) => OriginKind::Album,
            Self::Track(_) => OriginKind::Track,
            Self::Release(_) => OriginKind::Release,
            Self::Recording(_) => OriginKind::Recording,
        }
    }

    pub fn id(&self) -> String {
        match self {
            Self::Album(id) => id.to_string(),
            Self::Track(id) => id.to_string(),
            Self::Release(id) | Self::Recording(id) => id.to_string(),
        }
    }

    pub fn key(&self) -> (OriginKind, String) {
        (self.kind(), self.id())
    }

    pub fn parse(kind: OriginKind, id: &str) -> Option<Self> {
        match kind {
            OriginKind::Album => id.parse::<AlbumId>().ok().map(Self::Album),
            OriginKind::Track => id.parse::<TrackId>().ok().map(Self::Track),
            OriginKind::Release => MbId::parse(id).map(Self::Release),
            OriginKind::Recording => MbId::parse(id).map(Self::Recording),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RequestState {
    Pending,
    Searching,
    Transferring,
    Importing,
    Completed,
    AlreadyOwned,
    ReleaseNotFound,
    ReleaseAmbiguous,
    NoCandidate,
    Exhausted,
    ImportMismatch,
}

impl RequestState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Searching => "searching",
            Self::Transferring => "transferring",
            Self::Importing => "importing",
            Self::Completed => "completed",
            Self::AlreadyOwned => "already_owned",
            Self::ReleaseNotFound => "release_not_found",
            Self::ReleaseAmbiguous => "release_ambiguous",
            Self::NoCandidate => "no_candidate",
            Self::Exhausted => "exhausted",
            Self::ImportMismatch => "import_mismatch",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "pending" => Some(Self::Pending),
            "searching" => Some(Self::Searching),
            "transferring" => Some(Self::Transferring),
            "importing" => Some(Self::Importing),
            "completed" => Some(Self::Completed),
            "already_owned" => Some(Self::AlreadyOwned),
            "release_not_found" => Some(Self::ReleaseNotFound),
            "release_ambiguous" => Some(Self::ReleaseAmbiguous),
            "no_candidate" => Some(Self::NoCandidate),
            "exhausted" => Some(Self::Exhausted),
            "import_mismatch" => Some(Self::ImportMismatch),
            _ => None,
        }
    }

    pub fn is_terminal(self) -> bool {
        !matches!(
            self,
            Self::Pending | Self::Searching | Self::Transferring | Self::Importing
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttemptState {
    Transferring,
    Importing,
    Imported,
    Failed,
}

impl AttemptState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Transferring => "transferring",
            Self::Importing => "importing",
            Self::Imported => "imported",
            Self::Failed => "failed",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "transferring" => Some(Self::Transferring),
            "importing" => Some(Self::Importing),
            "imported" => Some(Self::Imported),
            "failed" => Some(Self::Failed),
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Request {
    pub id: Uuid,
    pub origin: AcquisitionOrigin,
    pub target: Option<AcquisitionTarget>,
    pub artist: Option<String>,
    pub title: Option<String>,
    pub expected_tracks: Vec<ExpectedTrack>,
    pub candidates: Vec<ReleaseCandidate>,
    pub state: RequestState,
    pub reason: Option<String>,
    pub attempts: u16,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct NewRequest {
    pub origin: AcquisitionOrigin,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AttemptFile {
    pub key: String,
    pub file: CandidateFile,
}

impl AttemptFile {
    pub fn new(key: Uuid, file: CandidateFile) -> Self {
        Self {
            key: key.to_string(),
            file,
        }
    }

    pub fn key(&self) -> Uuid {
        Uuid::parse_str(&self.key).expect("an attempt key is a stored uuid")
    }

    pub fn input(&self) -> &TransferInput {
        &self.file.input
    }
}

#[derive(Debug, Clone)]
pub struct Attempt {
    pub request_id: Uuid,
    pub number: u16,
    pub state: AttemptState,
    pub reason: Option<String>,
    pub files: Vec<AttemptFile>,
    pub handles: Vec<Option<String>>,
    pub delivered: Vec<PathBuf>,
    pub imported: Vec<ImportedItem>,
    pub(crate) finish_intent: Option<FinishIntent>,
    pub created_at: DateTime<Utc>,
}

impl Attempt {
    pub fn delivered(&self) -> &[PathBuf] {
        &self.delivered
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct FinishIntent {
    pub update: AttemptUpdate,
    pub request_state: RequestState,
    pub request_reason: Option<String>,
    pub cancel: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AttemptUpdate {
    pub state: AttemptState,
    pub reason: Option<String>,
    pub handles: Vec<Option<String>>,
    pub delivered: Vec<PathBuf>,
    pub imported: Vec<ImportedItem>,
}
