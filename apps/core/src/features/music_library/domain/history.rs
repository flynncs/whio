use chrono::{DateTime, Utc};
use uuid::Uuid;

use super::{PageLimit, TrackId, ValidationError};

const MAX_SESSION_ID: usize = 128;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ListenOrigin {
    SubsonicScrobble,
    Native,
    ReportPlayback,
}

impl ListenOrigin {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::SubsonicScrobble => "subsonic_scrobble",
            Self::Native => "native",
            Self::ReportPlayback => "report_playback",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "subsonic_scrobble" => Some(Self::SubsonicScrobble),
            "native" => Some(Self::Native),
            "report_playback" => Some(Self::ReportPlayback),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ListenSessionId(String);

impl ListenSessionId {
    pub fn new(value: String) -> Result<Self, ValidationError> {
        if value.is_empty() {
            return Err(ValidationError::Empty {
                field: "session_id",
            });
        }

        if value.chars().count() > MAX_SESSION_ID {
            return Err(ValidationError::TooLong {
                field: "session_id",
            });
        }

        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ListenExtent {
    start_position_ms: u64,
    listened_ms: u64,
    qualified: bool,
}

impl ListenExtent {
    pub fn new(start_position_ms: u64, listened_ms: u64, qualified: bool) -> Self {
        Self {
            start_position_ms,
            listened_ms,
            qualified,
        }
    }

    pub fn start_position_ms(&self) -> u64 {
        self.start_position_ms
    }

    pub fn listened_ms(&self) -> u64 {
        self.listened_ms
    }

    pub fn qualified(&self) -> bool {
        self.qualified
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListenSubmission {
    principal: Uuid,
    origin: ListenOrigin,
    track_id: TrackId,
    listen_at: DateTime<Utc>,
    session_id: Option<ListenSessionId>,
    extent: Option<ListenExtent>,
}

impl ListenSubmission {
    pub fn new(
        principal: Uuid,
        origin: ListenOrigin,
        track_id: TrackId,
        listen_at: DateTime<Utc>,
        session_id: Option<ListenSessionId>,
        extent: Option<ListenExtent>,
    ) -> Self {
        Self {
            principal,
            origin,
            track_id,
            listen_at,
            session_id,
            extent,
        }
    }

    pub fn principal(&self) -> Uuid {
        self.principal
    }

    pub fn origin(&self) -> ListenOrigin {
        self.origin
    }

    pub fn track_id(&self) -> &TrackId {
        &self.track_id
    }

    pub fn listen_at(&self) -> DateTime<Utc> {
        self.listen_at
    }

    pub fn session_id(&self) -> Option<&ListenSessionId> {
        self.session_id.as_ref()
    }

    pub fn extent(&self) -> Option<ListenExtent> {
        self.extent
    }

    pub fn counts_as_play(&self) -> bool {
        self.extent.is_none_or(|extent| extent.qualified)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ListenWindow {
    since: Option<DateTime<Utc>>,
    until: Option<DateTime<Utc>>,
}

impl ListenWindow {
    pub fn new(since: Option<DateTime<Utc>>, until: Option<DateTime<Utc>>) -> Self {
        Self { since, until }
    }

    pub fn unbounded() -> Self {
        Self::default()
    }

    pub fn is_unbounded(&self) -> bool {
        self.since.is_none() && self.until.is_none()
    }

    pub fn since(&self) -> Option<DateTime<Utc>> {
        self.since
    }

    pub fn until(&self) -> Option<DateTime<Utc>> {
        self.until
    }

    pub fn covered_start(&self, marker: Option<DateTime<Utc>>) -> Option<DateTime<Utc>> {
        let marker = marker?;

        if self.until.is_some_and(|until| until <= marker) {
            return None;
        }

        Some(self.since.filter(|since| *since > marker).unwrap_or(marker))
    }

    pub fn coverage(&self, marker: Option<DateTime<Utc>>) -> ListenCoverage {
        match self.covered_start(marker) {
            Some(_) => ListenCoverage::new(marker),
            None => ListenCoverage::new(None),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ListenCoverage {
    since: Option<DateTime<Utc>>,
}

impl ListenCoverage {
    pub fn new(since: Option<DateTime<Utc>>) -> Self {
        Self { since }
    }

    pub fn since(&self) -> Option<DateTime<Utc>> {
        self.since
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ListenOrder {
    #[default]
    NewestFirst,
    OldestFirst,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ListenQuery {
    window: ListenWindow,
    order: ListenOrder,
    limit: PageLimit,
    offset: u32,
}

impl ListenQuery {
    pub fn new(window: ListenWindow, order: ListenOrder, limit: PageLimit, offset: u32) -> Self {
        Self {
            window,
            order,
            limit,
            offset,
        }
    }

    pub fn window(&self) -> ListenWindow {
        self.window
    }

    pub fn order(&self) -> ListenOrder {
        self.order
    }

    pub fn limit(&self) -> PageLimit {
        self.limit
    }

    pub fn offset(&self) -> u32 {
        self.offset
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListenRecord {
    origin: ListenOrigin,
    track_id: TrackId,
    listen_at: DateTime<Utc>,
    session_id: Option<ListenSessionId>,
    extent: Option<ListenExtent>,
}

impl ListenRecord {
    pub fn new(
        origin: ListenOrigin,
        track_id: TrackId,
        listen_at: DateTime<Utc>,
        session_id: Option<ListenSessionId>,
        extent: Option<ListenExtent>,
    ) -> Self {
        Self {
            origin,
            track_id,
            listen_at,
            session_id,
            extent,
        }
    }

    pub fn origin(&self) -> ListenOrigin {
        self.origin
    }

    pub fn track_id(&self) -> &TrackId {
        &self.track_id
    }

    pub fn listen_at(&self) -> DateTime<Utc> {
        self.listen_at
    }

    pub fn session_id(&self) -> Option<&ListenSessionId> {
        self.session_id.as_ref()
    }

    pub fn extent(&self) -> Option<ListenExtent> {
        self.extent
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListenPage {
    entries: Vec<ListenRecord>,
    coverage: ListenCoverage,
}

impl ListenPage {
    pub fn new(entries: Vec<ListenRecord>, coverage: ListenCoverage) -> Self {
        Self { entries, coverage }
    }

    pub fn entries(&self) -> &[ListenRecord] {
        &self.entries
    }

    pub fn coverage(&self) -> ListenCoverage {
        self.coverage
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ListenCount {
    count: u64,
    coverage: ListenCoverage,
}

impl ListenCount {
    pub fn new(count: u64, coverage: ListenCoverage) -> Self {
        Self { count, coverage }
    }

    pub fn count(&self) -> u64 {
        self.count
    }

    pub fn coverage(&self) -> ListenCoverage {
        self.coverage
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ListenOutcome {
    Recorded,
    Duplicate,
}
