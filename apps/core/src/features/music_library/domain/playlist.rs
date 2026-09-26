use std::{
    fmt::{self, Display},
    str::FromStr,
};

use uuid::Uuid;

use super::{TrackId, ValidationError, text::optional};

const MAX_NAME: usize = 500;
const MAX_COMMENT: usize = 2000;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PlaylistId(Uuid);

impl PlaylistId {
    pub fn new() -> Self {
        Self(Uuid::now_v7())
    }

    pub fn from_uuid(value: Uuid) -> Self {
        Self(value)
    }

    pub fn as_uuid(&self) -> &Uuid {
        &self.0
    }
}

impl Default for PlaylistId {
    fn default() -> Self {
        Self::new()
    }
}

impl Display for PlaylistId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}", self.0)
    }
}

impl FromStr for PlaylistId {
    type Err = uuid::Error;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Ok(Self(Uuid::parse_str(value)?))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PlaylistEntryId(Uuid);

impl PlaylistEntryId {
    pub fn new() -> Self {
        Self(Uuid::now_v7())
    }

    pub fn from_uuid(value: Uuid) -> Self {
        Self(value)
    }

    pub fn as_uuid(&self) -> &Uuid {
        &self.0
    }
}

impl Default for PlaylistEntryId {
    fn default() -> Self {
        Self::new()
    }
}

impl Display for PlaylistEntryId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}", self.0)
    }
}

impl FromStr for PlaylistEntryId {
    type Err = uuid::Error;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Ok(Self(Uuid::parse_str(value)?))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Playlist {
    id: PlaylistId,
    owner_id: Uuid,
    owner_username: String,
    name: String,
    comment: Option<String>,
    public: bool,
    revision: i64,
    created_at: chrono::DateTime<chrono::Utc>,
    updated_at: chrono::DateTime<chrono::Utc>,
    track_count: u32,
    duration_ms: u64,
}

impl Playlist {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        id: PlaylistId,
        owner_id: Uuid,
        owner_username: String,
        name: String,
        comment: Option<String>,
        public: bool,
        revision: i64,
        created_at: chrono::DateTime<chrono::Utc>,
        updated_at: chrono::DateTime<chrono::Utc>,
        track_count: u32,
        duration_ms: u64,
    ) -> Result<Self, ValidationError> {
        Ok(Self {
            id,
            owner_id,
            owner_username,
            name: Self::validated_name(&name)?,
            comment: Self::validated_comment(comment.as_deref())?,
            public,
            revision,
            created_at,
            updated_at,
            track_count,
            duration_ms,
        })
    }

    pub fn validated_name(name: &str) -> Result<String, ValidationError> {
        let name = name.trim().to_owned();
        if name.is_empty() {
            return Err(ValidationError::Empty { field: "name" });
        }
        if name.chars().count() > MAX_NAME {
            return Err(ValidationError::TooLong { field: "name" });
        }

        Ok(name)
    }

    pub fn validated_comment(comment: Option<&str>) -> Result<Option<String>, ValidationError> {
        optional("comment", comment.map(str::to_owned), MAX_COMMENT)
    }

    pub fn id(&self) -> &PlaylistId {
        &self.id
    }

    pub fn owner_id(&self) -> Uuid {
        self.owner_id
    }

    pub fn owner_username(&self) -> &str {
        &self.owner_username
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn comment(&self) -> Option<&str> {
        self.comment.as_deref()
    }

    pub fn is_public(&self) -> bool {
        self.public
    }

    pub fn revision(&self) -> i64 {
        self.revision
    }

    pub fn created_at(&self) -> chrono::DateTime<chrono::Utc> {
        self.created_at
    }

    pub fn updated_at(&self) -> chrono::DateTime<chrono::Utc> {
        self.updated_at
    }

    pub fn track_count(&self) -> u32 {
        self.track_count
    }

    pub fn duration_ms(&self) -> u64 {
        self.duration_ms
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlaylistOccurrence {
    entry_id: PlaylistEntryId,
    position: u32,
    track_id: TrackId,
}

impl PlaylistOccurrence {
    pub fn new(entry_id: PlaylistEntryId, position: u32, track_id: TrackId) -> Self {
        Self {
            entry_id,
            position,
            track_id,
        }
    }

    pub fn entry_id(&self) -> &PlaylistEntryId {
        &self.entry_id
    }

    pub fn position(&self) -> u32 {
        self.position
    }

    pub fn track_id(&self) -> &TrackId {
        &self.track_id
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OccurrenceSelector {
    Entry(PlaylistEntryId),
    Index(u32),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MoveTarget {
    Start,
    End,
    Before(u32),
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum CommentPatch {
    #[default]
    Keep,
    Set(Option<String>),
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PlaylistMetadataPatch {
    pub name: Option<String>,
    pub comment: CommentPatch,
    pub public: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlaylistEntryChange {
    Unchanged,
    Replace(Vec<TrackId>),
    Apply {
        removals: Vec<OccurrenceSelector>,
        additions: Vec<TrackId>,
    },
    Move {
        selector: OccurrenceSelector,
        target: MoveTarget,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlaylistWrite {
    pub principal: Uuid,
    pub playlist_id: PlaylistId,
    pub expected_revision: Option<i64>,
    pub metadata: PlaylistMetadataPatch,
    pub entries: PlaylistEntryChange,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PlaylistEntryError {
    #[error("track {track_id} is not in the library")]
    UnknownTrack { track_id: TrackId },
    #[error("entry {entry_id} is not in the playlist")]
    UnknownEntry { entry_id: PlaylistEntryId },
    #[error("playlist position {index} does not exist")]
    IndexOutOfRange { index: u32 },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlaylistWriteOutcome {
    Applied(Playlist),
    Missing,
    RevisionConflict { current_revision: i64 },
    Rejected(PlaylistEntryError),
}
