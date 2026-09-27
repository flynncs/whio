use std::{collections::HashMap, collections::hash_map::Entry, sync::Arc};

use chrono::{DateTime, Utc};
use parking_lot::Mutex;
use thiserror::Error;
use uuid::Uuid;

use crate::features::music_library::{
    catalogue::{CatalogueError, LibraryService},
    domain::{
        ListenExtent, ListenOrigin, ListenOutcome, ListenSessionId, ListenSubmission, TrackId,
    },
    history::{ListenError, ListenService},
};

use crate::features::playback::domain::{
    ActivitySession, PlaybackClient, PlaybackReport, PlaybackState,
};

use super::{MAX_LIVE_SESSIONS_PER_PRINCIPAL, QUALIFICATION_LIMIT_MS, SESSION_IDLE_TTL_MS};

#[derive(Debug, Error)]
pub enum ActivityError {
    #[error("playback report named an unknown track")]
    UnknownTrack,

    #[error("live session identity is invalid")]
    SessionIdentity,

    #[error("music library read failed")]
    Catalogue(#[source] CatalogueError),

    #[error("listen submission failed")]
    Submission(#[source] ListenError),
}

pub struct PlaybackActivityService {
    listens: Arc<ListenService>,
    library: Arc<LibraryService>,
    sessions: Mutex<LiveSessions>,
}

impl PlaybackActivityService {
    pub fn new(listens: Arc<ListenService>, library: Arc<LibraryService>) -> Self {
        Self {
            listens,
            library,
            sessions: Mutex::new(LiveSessions::default()),
        }
    }

    pub async fn report(&self, report: &PlaybackReport) -> Result<ActivitySession, ActivityError> {
        let Some(track) = self
            .library
            .track(report.track_id())
            .await
            .map_err(ActivityError::Catalogue)?
        else {
            return Err(ActivityError::UnknownTrack);
        };

        let threshold = qualification_threshold(track.duration_ms());
        let key = (report.principal(), report.client().clone());

        let (snapshot, submission) = {
            let mut sessions = self.sessions.lock();
            sessions.expire(report.listen_at());

            let session = sessions.session_for(report)?;
            let qualified = !report.ignore_scrobble() && session.listened_ms() >= threshold;
            let submitting = qualified && !session.submitted();

            if submitting {
                session.mark_submitted();
            }

            let submission = submitting.then(|| {
                ListenSubmission::new(
                    report.principal(),
                    ListenOrigin::ReportPlayback,
                    report.track_id().clone(),
                    report.listen_at(),
                    Some(session.session_id().clone()),
                    Some(ListenExtent::new(
                        session.start_position_ms(),
                        session.listened_ms(),
                        qualified,
                    )),
                )
            });

            let snapshot = session.clone();

            if report.state() == PlaybackState::Stopped {
                sessions.close(&key);
            }

            (snapshot, submission)
        };

        let Some(submission) = submission else {
            return Ok(snapshot);
        };

        if let Err(error) = self.listens.submit_listen(&submission).await {
            self.sessions.lock().release(&key, &submission);

            return Err(ActivityError::Submission(error));
        }

        Ok(snapshot)
    }

    pub fn now_playing(&self, principal: Uuid, at: DateTime<Utc>) -> Vec<ActivitySession> {
        let mut sessions = self.sessions.lock();
        sessions.expire(at);

        let mut live: Vec<ActivitySession> = sessions
            .live_for(principal)
            .map(|session| session.positioned_at(at))
            .collect();

        live.sort_by(|left, right| left.client().as_str().cmp(right.client().as_str()));

        live
    }

    pub fn expire_idle(&self, at: DateTime<Utc>) -> usize {
        self.sessions.lock().expire(at)
    }

    pub async fn submit_scrobble(
        &self,
        principal: Uuid,
        track_id: &TrackId,
        listen_at: DateTime<Utc>,
    ) -> Result<ListenOutcome, ActivityError> {
        let submission = ListenSubmission::new(
            principal,
            ListenOrigin::SubsonicScrobble,
            track_id.clone(),
            listen_at,
            None,
            None,
        );

        match self.listens.submit_listen(&submission).await {
            Ok(outcome) => Ok(outcome),
            Err(ListenError::UnknownTrack) => Err(ActivityError::UnknownTrack),
            Err(error) => Err(ActivityError::Submission(error)),
        }
    }
}

#[derive(Default)]
struct LiveSessions {
    entries: HashMap<(Uuid, PlaybackClient), ActivitySession>,
}

impl LiveSessions {
    fn session_for(
        &mut self,
        report: &PlaybackReport,
    ) -> Result<&mut ActivitySession, ActivityError> {
        let key = (report.principal(), report.client().clone());
        let continuing = self.entries.get(&key).is_some_and(|session| {
            session.track_id() == report.track_id() && report.state() != PlaybackState::Starting
        });

        if !continuing && !self.entries.contains_key(&key) {
            self.evict_oldest_for(report.principal());
        }

        match self.entries.entry(key) {
            Entry::Occupied(mut occupied) => {
                if continuing {
                    occupied.get_mut().advance(
                        report.state(),
                        report.position_ms(),
                        report.playback_rate(),
                        report.listen_at(),
                    );
                } else {
                    occupied.insert(ActivitySession::start(report, session_identity()?));
                }

                Ok(occupied.into_mut())
            }
            Entry::Vacant(vacant) => {
                Ok(vacant.insert(ActivitySession::start(report, session_identity()?)))
            }
        }
    }

    fn evict_oldest_for(&mut self, principal: Uuid) {
        let live = self.entries.keys().filter(|key| key.0 == principal).count();

        if live < MAX_LIVE_SESSIONS_PER_PRINCIPAL {
            return;
        }

        let oldest = self
            .entries
            .iter()
            .filter(|(key, _)| key.0 == principal)
            .min_by_key(|(_, session)| session.last_event_at())
            .map(|(key, _)| key.clone());

        if let Some(oldest) = oldest {
            self.entries.remove(&oldest);
        }
    }

    fn expire(&mut self, at: DateTime<Utc>) -> usize {
        let before = self.entries.len();

        self.entries
            .retain(|_, session| session.idle_ms(at) < SESSION_IDLE_TTL_MS);

        before - self.entries.len()
    }

    fn close(&mut self, key: &(Uuid, PlaybackClient)) {
        self.entries.remove(key);
    }

    fn release(&mut self, key: &(Uuid, PlaybackClient), submission: &ListenSubmission) {
        let Some(session) = self.entries.get_mut(key) else {
            return;
        };

        if Some(session.session_id()) == submission.session_id() {
            session.clear_submitted();
        }
    }

    fn live_for(&self, principal: Uuid) -> impl Iterator<Item = &ActivitySession> {
        self.entries
            .values()
            .filter(move |session| session.principal() == principal)
    }
}

fn qualification_threshold(duration_ms: Option<u64>) -> u64 {
    duration_ms.map_or(QUALIFICATION_LIMIT_MS, |duration| {
        (duration / 2).min(QUALIFICATION_LIMIT_MS)
    })
}

fn session_identity() -> Result<ListenSessionId, ActivityError> {
    ListenSessionId::new(Uuid::now_v7().to_string()).map_err(|_| ActivityError::SessionIdentity)
}
