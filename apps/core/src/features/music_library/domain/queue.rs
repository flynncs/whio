use std::{
    fmt::{self, Display},
    str::FromStr,
};

use uuid::Uuid;

use super::TrackId;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RestoreQueueEntryId(Uuid);

impl RestoreQueueEntryId {
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

impl Default for RestoreQueueEntryId {
    fn default() -> Self {
        Self::new()
    }
}

impl Display for RestoreQueueEntryId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}", self.0)
    }
}

impl FromStr for RestoreQueueEntryId {
    type Err = uuid::Error;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Ok(Self(Uuid::parse_str(value)?))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestoreQueueEntry {
    entry_id: RestoreQueueEntryId,
    position: u32,
    track_id: TrackId,
}

impl RestoreQueueEntry {
    pub fn new(entry_id: RestoreQueueEntryId, position: u32, track_id: TrackId) -> Self {
        Self {
            entry_id,
            position,
            track_id,
        }
    }

    pub fn entry_id(&self) -> &RestoreQueueEntryId {
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
pub struct RestoreQueueSnapshot {
    user_id: Uuid,
    changed_by: Uuid,
    changed_by_username: String,
    current_index: Option<u32>,
    position_ms: u64,
    revision: i64,
    changed_at: chrono::DateTime<chrono::Utc>,
    entries: Vec<RestoreQueueEntry>,
}

impl RestoreQueueSnapshot {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        user_id: Uuid,
        changed_by: Uuid,
        changed_by_username: String,
        current_index: Option<u32>,
        position_ms: u64,
        revision: i64,
        changed_at: chrono::DateTime<chrono::Utc>,
        entries: Vec<RestoreQueueEntry>,
    ) -> Self {
        Self {
            user_id,
            changed_by,
            changed_by_username,
            current_index,
            position_ms,
            revision,
            changed_at,
            entries,
        }
    }

    pub fn user_id(&self) -> Uuid {
        self.user_id
    }

    pub fn changed_by(&self) -> Uuid {
        self.changed_by
    }

    pub fn changed_by_username(&self) -> &str {
        &self.changed_by_username
    }

    pub fn current_index(&self) -> Option<u32> {
        self.current_index
    }

    pub fn position_ms(&self) -> u64 {
        self.position_ms
    }

    pub fn revision(&self) -> i64 {
        self.revision
    }

    pub fn changed_at(&self) -> chrono::DateTime<chrono::Utc> {
        self.changed_at
    }

    pub fn entries(&self) -> &[RestoreQueueEntry] {
        &self.entries
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestoreQueueWrite {
    pub user_id: Uuid,
    pub expected_revision: Option<i64>,
    pub entries: Vec<TrackId>,
    pub current_index: Option<u32>,
    pub position_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RestoreQueueValidationError {
    #[error("restore queue holds at most {max_entries} entries")]
    TooManyEntries { max_entries: usize },
    #[error("track {track_id} is not in the library")]
    UnknownTrack { track_id: TrackId },
    #[error("current index {index} is not within {entry_count} entries")]
    CurrentIndexOutOfRange { index: u32, entry_count: usize },
    #[error("a restore queue with {entry_count} entries must name a current index")]
    CurrentIndexMissing { entry_count: usize },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RestoreQueueWriteOutcome {
    Saved(RestoreQueueSnapshot),
    RevisionConflict { current_revision: i64 },
    Rejected(RestoreQueueValidationError),
}
