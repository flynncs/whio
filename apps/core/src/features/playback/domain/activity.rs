use chrono::{DateTime, Utc};
use uuid::Uuid;

use crate::features::music_library::domain::{ListenSessionId, TrackId};

const MAX_CLIENT_LENGTH: usize = 128;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ActivityInputError {
    #[error("{field} must not be empty")]
    Empty { field: &'static str },

    #[error("{field} is longer than {MAX_CLIENT_LENGTH} characters")]
    TooLong { field: &'static str },

    #[error("playback rate must be a finite positive multiplier")]
    InvalidRate,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PlaybackClient(String);

impl PlaybackClient {
    pub fn new(value: String) -> Result<Self, ActivityInputError> {
        if value.is_empty() {
            return Err(ActivityInputError::Empty { field: "client" });
        }

        if value.chars().count() > MAX_CLIENT_LENGTH {
            return Err(ActivityInputError::TooLong { field: "client" });
        }

        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PlaybackRate(f64);

impl PlaybackRate {
    pub fn new(value: f64) -> Result<Self, ActivityInputError> {
        if !value.is_finite() || value <= 0.0 {
            return Err(ActivityInputError::InvalidRate);
        }

        Ok(Self(value))
    }

    pub fn get(self) -> f64 {
        self.0
    }
}

impl Default for PlaybackRate {
    fn default() -> Self {
        Self(1.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlaybackState {
    Starting,
    Playing,
    Paused,
    Stopped,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PlaybackReport {
    principal: Uuid,
    client: PlaybackClient,
    track_id: TrackId,
    state: PlaybackState,
    position_ms: u64,
    playback_rate: PlaybackRate,
    listen_at: DateTime<Utc>,
    ignore_scrobble: bool,
}

impl PlaybackReport {
    pub fn new(
        principal: Uuid,
        client: PlaybackClient,
        track_id: TrackId,
        state: PlaybackState,
        position_ms: u64,
        playback_rate: PlaybackRate,
        listen_at: DateTime<Utc>,
    ) -> Self {
        Self {
            principal,
            client,
            track_id,
            state,
            position_ms,
            playback_rate,
            listen_at,
            ignore_scrobble: false,
        }
    }

    pub fn with_ignored_scrobble(mut self, ignore_scrobble: bool) -> Self {
        self.ignore_scrobble = ignore_scrobble;
        self
    }

    pub fn principal(&self) -> Uuid {
        self.principal
    }

    pub fn client(&self) -> &PlaybackClient {
        &self.client
    }

    pub fn track_id(&self) -> &TrackId {
        &self.track_id
    }

    pub fn state(&self) -> PlaybackState {
        self.state
    }

    pub fn position_ms(&self) -> u64 {
        self.position_ms
    }

    pub fn playback_rate(&self) -> PlaybackRate {
        self.playback_rate
    }

    pub fn listen_at(&self) -> DateTime<Utc> {
        self.listen_at
    }

    pub fn ignore_scrobble(&self) -> bool {
        self.ignore_scrobble
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ActivitySession {
    principal: Uuid,
    client: PlaybackClient,
    session_id: ListenSessionId,
    track_id: TrackId,
    state: PlaybackState,
    position_ms: u64,
    start_position_ms: u64,
    playback_rate: PlaybackRate,
    listened_ms: u64,
    started_at: DateTime<Utc>,
    last_event_at: DateTime<Utc>,
    submitted: bool,
}

impl ActivitySession {
    pub(crate) fn start(report: &PlaybackReport, session_id: ListenSessionId) -> Self {
        Self {
            principal: report.principal(),
            client: report.client().clone(),
            session_id,
            track_id: report.track_id().clone(),
            state: report.state(),
            position_ms: report.position_ms(),
            start_position_ms: report.position_ms(),
            playback_rate: report.playback_rate(),
            listened_ms: 0,
            started_at: report.listen_at(),
            last_event_at: report.listen_at(),
            submitted: false,
        }
    }

    pub fn principal(&self) -> Uuid {
        self.principal
    }

    pub fn client(&self) -> &PlaybackClient {
        &self.client
    }

    pub fn session_id(&self) -> &ListenSessionId {
        &self.session_id
    }

    pub fn track_id(&self) -> &TrackId {
        &self.track_id
    }

    pub fn state(&self) -> PlaybackState {
        self.state
    }

    pub fn position_ms(&self) -> u64 {
        self.position_ms
    }

    pub fn start_position_ms(&self) -> u64 {
        self.start_position_ms
    }

    pub fn playback_rate(&self) -> PlaybackRate {
        self.playback_rate
    }

    pub fn listened_ms(&self) -> u64 {
        self.listened_ms
    }

    pub fn started_at(&self) -> DateTime<Utc> {
        self.started_at
    }

    pub fn last_event_at(&self) -> DateTime<Utc> {
        self.last_event_at
    }

    pub fn submitted(&self) -> bool {
        self.submitted
    }

    pub fn idle_ms(&self, at: DateTime<Utc>) -> u64 {
        elapsed_ms(self.last_event_at, at)
    }

    pub fn position_at(&self, at: DateTime<Utc>) -> u64 {
        if self.state != PlaybackState::Playing {
            return self.position_ms;
        }

        self.position_ms.saturating_add(played_ms(
            elapsed_ms(self.last_event_at, at),
            self.playback_rate,
        ))
    }

    pub fn positioned_at(&self, at: DateTime<Utc>) -> Self {
        let mut session = self.clone();
        session.position_ms = session.position_at(at);
        session
    }

    pub(crate) fn advance(
        &mut self,
        state: PlaybackState,
        position_ms: u64,
        playback_rate: PlaybackRate,
        at: DateTime<Utc>,
    ) {
        if self.state == PlaybackState::Playing {
            self.listened_ms = self.listened_ms.saturating_add(played_ms(
                elapsed_ms(self.last_event_at, at),
                self.playback_rate,
            ));
        }

        self.state = state;
        self.position_ms = position_ms;
        self.playback_rate = playback_rate;

        if at > self.last_event_at {
            self.last_event_at = at;
        }
    }

    pub(crate) fn mark_submitted(&mut self) {
        self.submitted = true;
    }

    pub(crate) fn clear_submitted(&mut self) {
        self.submitted = false;
    }
}

pub(crate) fn elapsed_ms(from: DateTime<Utc>, to: DateTime<Utc>) -> u64 {
    (to - from).num_milliseconds().max(0) as u64
}

fn played_ms(elapsed_ms: u64, rate: PlaybackRate) -> u64 {
    (elapsed_ms as f64 * rate.get()).round() as u64
}
