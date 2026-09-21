use std::{
    fmt::{self, Display},
    str::FromStr,
};

use uuid::Uuid;

use super::{ValidationError, text::optional};

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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Playlist {
    id: PlaylistId,
    name: String,
    comment: Option<String>,
    public: bool,
    created_at: chrono::DateTime<chrono::Utc>,
    updated_at: chrono::DateTime<chrono::Utc>,
    track_count: u32,
}

impl Playlist {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        id: PlaylistId,
        name: String,
        comment: Option<String>,
        public: bool,
        created_at: chrono::DateTime<chrono::Utc>,
        updated_at: chrono::DateTime<chrono::Utc>,
        track_count: u32,
    ) -> Result<Self, ValidationError> {
        let name = name.trim().to_owned();
        if name.is_empty() {
            return Err(ValidationError::Empty { field: "name" });
        }
        if name.chars().count() > MAX_NAME {
            return Err(ValidationError::TooLong { field: "name" });
        }

        Ok(Self {
            id,
            name,
            comment: optional("comment", comment, MAX_COMMENT)?,
            public,
            created_at,
            updated_at,
            track_count,
        })
    }

    pub fn id(&self) -> &PlaylistId {
        &self.id
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

    pub fn created_at(&self) -> chrono::DateTime<chrono::Utc> {
        self.created_at
    }

    pub fn updated_at(&self) -> chrono::DateTime<chrono::Utc> {
        self.updated_at
    }

    pub fn track_count(&self) -> u32 {
        self.track_count
    }
}
