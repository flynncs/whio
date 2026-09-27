use std::{collections::HashMap, sync::Arc};

use async_trait::async_trait;
use chrono::{DateTime, Duration, Utc};
use parking_lot::Mutex;
use uuid::Uuid;

use crate::features::music_library::{
    catalogue::LibraryService,
    domain::{
        ListenCount, ListenCoverage, ListenOrigin, ListenOutcome, ListenPage, ListenQuery,
        ListenSubmission, ListenWindow, SourceIdentity, Track, TrackId, TrackListFilter,
        TrackMetadata, TrackOrder, TrackPage, TrackView,
    },
    history::ListenService,
    test_support::{LibraryRead, ListenStore, SourceLookup, StateStore, TrackRepositoryError},
};

use super::{
    ActivityError, MAX_LIVE_SESSIONS_PER_PRINCIPAL, PlaybackActivityService, SESSION_IDLE_TTL_MS,
};
use crate::features::playback::domain::{
    PlaybackClient, PlaybackRate, PlaybackReport, PlaybackState,
};

const PRINCIPAL: Uuid = Uuid::from_u128(7);
const OTHER_PRINCIPAL: Uuid = Uuid::from_u128(8);

#[derive(Default)]
struct FakeLibrary {
    tracks: Mutex<HashMap<Uuid, Track>>,
}

impl FakeLibrary {
    fn new(tracks: &[Track]) -> Self {
        let tracks = tracks
            .iter()
            .map(|track| (*track.id().as_uuid(), track.clone()))
            .collect();

        Self {
            tracks: Mutex::new(tracks),
        }
    }
}

#[async_trait]
impl LibraryRead for FakeLibrary {
    async fn find_track(&self, track_id: &TrackId) -> Result<Option<Track>, TrackRepositoryError> {
        Ok(self.tracks.lock().get(track_id.as_uuid()).cloned())
    }

    async fn page_tracks(
        &self,
        _offset: u32,
        _limit: u32,
        _order: TrackOrder,
        _filter: &TrackListFilter,
        _user: Option<Uuid>,
    ) -> Result<TrackPage, TrackRepositoryError> {
        Ok(TrackPage::new(Vec::new(), 0))
    }

    async fn track_views(
        &self,
        _track_ids: &[TrackId],
        _user: Option<Uuid>,
    ) -> Result<Vec<TrackView>, TrackRepositoryError> {
        Ok(Vec::new())
    }
}

struct NoSources;

#[async_trait]
impl SourceLookup for NoSources {
    async fn find_sources(
        &self,
        _track_id: &TrackId,
    ) -> Result<Vec<SourceIdentity>, TrackRepositoryError> {
        Ok(Vec::new())
    }
}

struct NoState;

#[async_trait]
impl StateStore for NoState {}

#[derive(Default)]
struct Ledger {
    accepted: Mutex<Vec<ListenSubmission>>,
    failures: Mutex<usize>,
}

impl Ledger {
    fn rows(&self) -> Vec<ListenSubmission> {
        self.accepted.lock().clone()
    }

    fn plays(&self) -> usize {
        self.accepted
            .lock()
            .iter()
            .filter(|submission| submission.counts_as_play())
            .count()
    }

    fn same_listen(left: &ListenSubmission, right: &ListenSubmission) -> bool {
        left.principal() == right.principal()
            && left.origin() == right.origin()
            && left.track_id() == right.track_id()
            && left.listen_at() == right.listen_at()
            && left.session_id() == right.session_id()
    }
}

#[async_trait]
impl ListenStore for Ledger {
    async fn submit_listen(
        &self,
        submission: &ListenSubmission,
    ) -> Result<ListenOutcome, TrackRepositoryError> {
        let mut failures = self.failures.lock();

        if *failures > 0 {
            *failures -= 1;

            return Err(TrackRepositoryError::Unavailable);
        }

        drop(failures);

        let mut accepted = self.accepted.lock();

        if accepted
            .iter()
            .any(|existing| Ledger::same_listen(existing, submission))
        {
            return Ok(ListenOutcome::Duplicate);
        }

        accepted.push(submission.clone());

        Ok(ListenOutcome::Recorded)
    }

    async fn listen_history(
        &self,
        _principal: Uuid,
        _query: &ListenQuery,
    ) -> Result<ListenPage, TrackRepositoryError> {
        Ok(ListenPage::new(Vec::new(), ListenCoverage::default()))
    }

    async fn listen_count(
        &self,
        _principal: Uuid,
        _window: ListenWindow,
        _track: Option<&TrackId>,
    ) -> Result<ListenCount, TrackRepositoryError> {
        Ok(ListenCount::new(
            self.plays() as u64,
            ListenCoverage::default(),
        ))
    }
}

struct Harness {
    service: PlaybackActivityService,
    ledger: Arc<Ledger>,
}

fn harness(tracks: &[Track]) -> Harness {
    harness_failing(tracks, 0)
}

fn harness_failing(tracks: &[Track], failures: usize) -> Harness {
    let library = Arc::new(FakeLibrary::new(tracks));
    let ledger = Arc::new(Ledger {
        failures: Mutex::new(failures),
        ..Ledger::default()
    });
    let listens = Arc::new(ListenService::new(
        Arc::clone(&ledger) as Arc<dyn ListenStore>,
        Arc::clone(&library) as Arc<dyn LibraryRead>,
    ));
    let catalogue = Arc::new(LibraryService::new(
        Arc::clone(&library) as Arc<dyn LibraryRead>,
        Arc::new(NoSources),
        Arc::new(NoState),
    ));

    Harness {
        service: PlaybackActivityService::new(listens, catalogue),
        ledger,
    }
}

fn at(ms: i64) -> DateTime<Utc> {
    "2026-09-27T00:00:00Z".parse::<DateTime<Utc>>().unwrap() + Duration::milliseconds(ms)
}

fn track_id(seed: u128) -> TrackId {
    TrackId::from_uuid(Uuid::from_u128(seed))
}

fn track(seed: u128, duration_ms: Option<u64>) -> Track {
    Track::new(
        track_id(seed),
        TrackMetadata::from_names("Song".to_owned(), vec!["Artist".to_owned()], duration_ms)
            .unwrap(),
    )
}

fn client(name: &str) -> PlaybackClient {
    PlaybackClient::new(name.to_owned()).unwrap()
}

fn report(
    client: &PlaybackClient,
    state: PlaybackState,
    position_ms: u64,
    ms: i64,
) -> PlaybackReport {
    PlaybackReport::new(
        PRINCIPAL,
        client.clone(),
        track_id(1),
        state,
        position_ms,
        PlaybackRate::default(),
        at(ms),
    )
}

fn ramp(
    client: &PlaybackClient,
    steps: u64,
    from_ms: i64,
    from_position_ms: u64,
) -> Vec<PlaybackReport> {
    (1..=steps)
        .map(|step| {
            report(
                client,
                PlaybackState::Playing,
                from_position_ms + step * 30_000,
                from_ms + step as i64 * 30_000,
            )
        })
        .collect()
}

#[tokio::test]
async fn a_report_starts_and_updates_a_session() {
    let harness = harness(&[track(1, Some(240_000))]);
    let phone = client("phone");
    let tablet = client("tablet");

    let session = harness
        .service
        .report(&report(&phone, PlaybackState::Playing, 0, 0))
        .await
        .unwrap();

    assert_eq!(session.state(), PlaybackState::Playing);
    assert_eq!(session.position_ms(), 0);
    assert_eq!(session.start_position_ms(), 0);
    assert_eq!(session.playback_rate().get(), 1.0);
    assert_eq!(session.listened_ms(), 0);
    assert_eq!(session.started_at(), at(0));
    assert_eq!(session.last_event_at(), at(0));
    assert!(!session.submitted());
    assert!(!session.session_id().as_str().is_empty());

    let session = harness
        .service
        .report(&report(&phone, PlaybackState::Playing, 30_000, 30_000))
        .await
        .unwrap();

    assert_eq!(session.position_ms(), 30_000);
    assert_eq!(session.listened_ms(), 30_000);

    let session = harness
        .service
        .report(&report(&phone, PlaybackState::Playing, 120_000, 31_000))
        .await
        .unwrap();

    assert_eq!(session.position_ms(), 120_000);
    assert_eq!(session.listened_ms(), 31_000);

    let session = harness
        .service
        .report(&PlaybackReport::new(
            PRINCIPAL,
            phone.clone(),
            track_id(1),
            PlaybackState::Playing,
            130_000,
            PlaybackRate::new(1.5).unwrap(),
            at(41_000),
        ))
        .await
        .unwrap();

    assert_eq!(session.playback_rate().get(), 1.5);
    assert_eq!(session.position_ms(), 130_000);
    assert_eq!(session.listened_ms(), 41_000);

    let session = harness
        .service
        .report(&PlaybackReport::new(
            PRINCIPAL,
            phone.clone(),
            track_id(1),
            PlaybackState::Playing,
            145_000,
            PlaybackRate::new(1.5).unwrap(),
            at(51_000),
        ))
        .await
        .unwrap();

    assert_eq!(session.listened_ms(), 56_000);

    harness
        .service
        .report(&report(&tablet, PlaybackState::Playing, 5_000, 51_000))
        .await
        .unwrap();

    let live = harness.service.now_playing(PRINCIPAL, at(51_000));

    assert_eq!(live.len(), 2);
    assert_eq!(live[0].client().as_str(), "phone");
    assert_eq!(live[0].position_ms(), 145_000);
    assert_eq!(live[0].listened_ms(), 56_000);
    assert_eq!(live[1].client().as_str(), "tablet");
    assert_eq!(live[1].position_ms(), 5_000);
    assert_eq!(live[1].listened_ms(), 0);
    assert!(
        harness
            .service
            .now_playing(OTHER_PRINCIPAL, at(41_000))
            .is_empty()
    );
    assert!(harness.ledger.rows().is_empty());
}

#[tokio::test]
async fn seeking_forward_does_not_qualify() {
    let harness = harness(&[track(1, Some(240_000))]);
    let phone = client("phone");

    harness
        .service
        .report(&report(&phone, PlaybackState::Playing, 0, 0))
        .await
        .unwrap();

    for (position_ms, ms) in [(238_000_u64, 1_000_i64), (240_000, 2_000), (240_000, 3_000)] {
        let session = harness
            .service
            .report(&report(&phone, PlaybackState::Playing, position_ms, ms))
            .await
            .unwrap();

        assert_eq!(session.position_ms(), position_ms);
        assert_eq!(session.listened_ms(), ms as u64);
    }

    assert!(harness.ledger.rows().is_empty());
    assert_eq!(harness.ledger.plays(), 0);
    assert_eq!(
        harness.service.now_playing(PRINCIPAL, at(3_000))[0].position_ms(),
        240_000
    );
}

#[tokio::test]
async fn a_qualified_session_submits_once() {
    let harness = harness(&[track(1, Some(300_000)), track(2, Some(300_000))]);
    let phone = client("phone");

    harness
        .service
        .report(&report(&phone, PlaybackState::Playing, 0, 0))
        .await
        .unwrap();

    for report in ramp(&phone, 5, 0, 0) {
        harness.service.report(&report).await.unwrap();
    }

    let first = harness.ledger.rows();

    assert_eq!(first.len(), 1);
    assert_eq!(first[0].origin(), ListenOrigin::ReportPlayback);
    assert_eq!(first[0].track_id(), &track_id(1));
    assert_eq!(first[0].listen_at(), at(150_000));
    assert_eq!(
        first[0].extent().map(|extent| extent.start_position_ms()),
        Some(0)
    );
    assert_eq!(
        first[0].extent().map(|extent| extent.listened_ms()),
        Some(150_000)
    );
    assert_eq!(
        first[0].extent().map(|extent| extent.qualified()),
        Some(true)
    );
    assert!(first[0].session_id().is_some());

    harness
        .service
        .report(&report(&phone, PlaybackState::Playing, 180_000, 180_000))
        .await
        .unwrap();
    harness
        .service
        .report(&report(&phone, PlaybackState::Paused, 200_000, 200_000))
        .await
        .unwrap();

    assert_eq!(harness.ledger.rows().len(), 1);

    let restarted = harness
        .service
        .report(&report(&phone, PlaybackState::Starting, 10_000, 211_000))
        .await
        .unwrap();

    assert_eq!(restarted.listened_ms(), 0);
    assert_eq!(restarted.start_position_ms(), 10_000);

    harness
        .service
        .report(&report(&phone, PlaybackState::Playing, 10_000, 211_000))
        .await
        .unwrap();

    for report in ramp(&phone, 5, 211_000, 10_000) {
        harness.service.report(&report).await.unwrap();
    }

    let rows = harness.ledger.rows();

    assert_eq!(rows.len(), 2);
    assert_eq!(
        rows[1].extent().map(|extent| extent.start_position_ms()),
        Some(10_000)
    );
    assert_eq!(
        rows[1].extent().map(|extent| extent.listened_ms()),
        Some(150_000)
    );
    assert_eq!(rows[1].listen_at(), at(361_000));
    assert_ne!(rows[1].session_id(), rows[0].session_id());
    assert_eq!(harness.ledger.plays(), 2);

    let replaced = harness
        .service
        .report(&PlaybackReport::new(
            PRINCIPAL,
            phone.clone(),
            track_id(2),
            PlaybackState::Playing,
            0,
            PlaybackRate::default(),
            at(400_000),
        ))
        .await
        .unwrap();

    assert_eq!(replaced.track_id(), &track_id(2));
    assert_eq!(replaced.listened_ms(), 0);

    let live = harness.service.now_playing(PRINCIPAL, at(400_000));

    assert_eq!(live.len(), 1);
    assert_eq!(live[0].track_id(), &track_id(2));
}

#[tokio::test]
async fn a_short_session_is_not_submitted() {
    let harness = harness(&[track(1, Some(300_000))]);
    let phone = client("phone");

    harness
        .service
        .report(&report(&phone, PlaybackState::Playing, 0, 0))
        .await
        .unwrap();
    harness
        .service
        .report(&report(&phone, PlaybackState::Stopped, 20_000, 20_000))
        .await
        .unwrap();

    assert!(harness.ledger.rows().is_empty());
    assert_eq!(harness.ledger.plays(), 0);
    assert!(
        harness
            .service
            .now_playing(PRINCIPAL, at(20_000))
            .is_empty()
    );
}

#[tokio::test]
async fn an_idle_session_expires_without_submitting() {
    let idle = harness(&[track(1, Some(300_000))]);
    let phone = client("phone");

    idle.service
        .report(&report(&phone, PlaybackState::Playing, 0, 0))
        .await
        .unwrap();

    assert_eq!(
        idle.service.expire_idle(at(SESSION_IDLE_TTL_MS as i64 - 1)),
        0
    );
    assert_eq!(idle.service.now_playing(PRINCIPAL, at(0)).len(), 1);
    assert_eq!(idle.service.expire_idle(at(SESSION_IDLE_TTL_MS as i64)), 1);
    assert!(idle.service.now_playing(PRINCIPAL, at(0)).is_empty());
    assert!(idle.ledger.rows().is_empty());

    let qualified = harness(&[track(1, Some(300_000))]);

    qualified
        .service
        .report(&report(&phone, PlaybackState::Playing, 0, 0))
        .await
        .unwrap();

    for report in ramp(&phone, 5, 0, 0) {
        qualified.service.report(&report).await.unwrap();
    }

    assert_eq!(qualified.ledger.rows().len(), 1);
    assert_eq!(
        qualified
            .service
            .expire_idle(at(SESSION_IDLE_TTL_MS as i64 + 150_000)),
        1
    );
    assert_eq!(qualified.ledger.rows().len(), 1);
    assert!(qualified.service.now_playing(PRINCIPAL, at(0)).is_empty());

    let suppressed = harness(&[track(1, Some(300_000))]);

    suppressed
        .service
        .report(&report(&phone, PlaybackState::Playing, 0, 0))
        .await
        .unwrap();

    for report in ramp(&phone, 5, 0, 0) {
        suppressed
            .service
            .report(&report.with_ignored_scrobble(true))
            .await
            .unwrap();
    }

    assert!(suppressed.ledger.rows().is_empty());
    assert_eq!(
        suppressed
            .service
            .expire_idle(at(SESSION_IDLE_TTL_MS as i64 + 150_000)),
        1
    );
    assert!(suppressed.ledger.rows().is_empty());
}

#[tokio::test]
async fn ignore_scrobble_has_no_effect() {
    let harness = harness(&[track(1, Some(300_000))]);
    let phone = client("phone");

    let mut session = harness
        .service
        .report(&report(&phone, PlaybackState::Playing, 0, 0))
        .await
        .unwrap();

    for report in ramp(&phone, 6, 0, 0) {
        session = harness
            .service
            .report(&report.with_ignored_scrobble(true))
            .await
            .unwrap();
    }

    assert!(harness.ledger.rows().is_empty());
    assert_eq!(harness.ledger.plays(), 0);
    assert_eq!(session.state(), PlaybackState::Playing);
    assert_eq!(session.position_ms(), 180_000);
    assert_eq!(session.listened_ms(), 180_000);
    assert_eq!(session.playback_rate().get(), 1.0);

    let live = harness.service.now_playing(PRINCIPAL, at(180_000));

    assert_eq!(live.len(), 1);
    assert_eq!(live[0].position_ms(), 180_000);
    assert_eq!(live[0].listened_ms(), 180_000);
}

#[tokio::test]
async fn an_ignored_report_does_not_suppress_a_later_reported_play() {
    let harness = harness(&[track(1, Some(300_000))]);
    let phone = client("phone");

    harness
        .service
        .report(&report(&phone, PlaybackState::Playing, 0, 0))
        .await
        .unwrap();
    harness
        .service
        .report(
            &report(&phone, PlaybackState::Playing, 150_000, 150_000).with_ignored_scrobble(true),
        )
        .await
        .unwrap();

    assert!(harness.ledger.rows().is_empty());

    harness
        .service
        .report(&report(&phone, PlaybackState::Playing, 151_000, 151_000))
        .await
        .unwrap();

    let rows = harness.ledger.rows();

    assert_eq!(rows.len(), 1);
    assert_eq!(
        rows[0].extent().map(|extent| extent.listened_ms()),
        Some(151_000)
    );
    assert_eq!(
        rows[0].extent().map(|extent| extent.qualified()),
        Some(true)
    );
}

#[tokio::test]
async fn a_legacy_scrobble_submits_without_an_extent() {
    let harness = harness(&[track(1, Some(300_000))]);

    let outcome = harness
        .service
        .submit_scrobble(PRINCIPAL, &track_id(1), at(0))
        .await
        .unwrap();

    assert_eq!(outcome, ListenOutcome::Recorded);

    let rows = harness.ledger.rows();

    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].origin(), ListenOrigin::SubsonicScrobble);
    assert_eq!(rows[0].track_id(), &track_id(1));
    assert_eq!(rows[0].listen_at(), at(0));
    assert_eq!(rows[0].session_id(), None);
    assert_eq!(rows[0].extent(), None);
    assert_eq!(harness.ledger.plays(), 1);

    let repeat = harness
        .service
        .submit_scrobble(PRINCIPAL, &track_id(1), at(0))
        .await
        .unwrap();

    assert_eq!(repeat, ListenOutcome::Duplicate);
    assert_eq!(harness.ledger.rows().len(), 1);

    let distinct = harness
        .service
        .submit_scrobble(PRINCIPAL, &track_id(1), at(1_000))
        .await
        .unwrap();

    assert_eq!(distinct, ListenOutcome::Recorded);
    assert_eq!(harness.ledger.rows().len(), 2);

    let unknown = harness
        .service
        .submit_scrobble(PRINCIPAL, &track_id(9), at(2_000))
        .await
        .unwrap_err();

    assert!(matches!(unknown, ActivityError::UnknownTrack));
    assert_eq!(harness.ledger.rows().len(), 2);
}

#[tokio::test]
async fn a_report_for_an_unknown_track_is_refused() {
    let harness = harness(&[track(1, Some(300_000))]);
    let phone = client("phone");

    let refused = harness
        .service
        .report(&PlaybackReport::new(
            PRINCIPAL,
            phone.clone(),
            track_id(9),
            PlaybackState::Playing,
            0,
            PlaybackRate::default(),
            at(0),
        ))
        .await
        .unwrap_err();

    assert!(matches!(refused, ActivityError::UnknownTrack));
    assert!(harness.service.now_playing(PRINCIPAL, at(0)).is_empty());
    assert!(harness.ledger.rows().is_empty());
}

#[tokio::test]
async fn a_failed_stopped_submission_retries_and_closes() {
    let harness = harness_failing(&[track(1, Some(240_000))], 1);
    let phone = client("phone");

    harness
        .service
        .report(&report(&phone, PlaybackState::Playing, 0, 0))
        .await
        .unwrap();

    let failed = harness
        .service
        .report(&report(&phone, PlaybackState::Stopped, 240_000, 240_000))
        .await
        .unwrap_err();

    assert!(matches!(failed, ActivityError::Submission(_)));
    assert!(harness.ledger.rows().is_empty());
    assert!(
        harness
            .service
            .now_playing(PRINCIPAL, at(240_000))
            .is_empty()
    );

    let retried = harness
        .service
        .report(&report(&phone, PlaybackState::Stopped, 240_000, 241_000))
        .await
        .unwrap();

    let rows = harness.ledger.rows();

    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].listen_at(), at(241_000));
    assert_eq!(
        rows[0].extent().map(|extent| extent.listened_ms()),
        Some(240_000)
    );
    assert_eq!(
        rows[0].extent().map(|extent| extent.qualified()),
        Some(true)
    );
    assert_eq!(retried.state(), PlaybackState::Stopped);
    assert!(
        harness
            .service
            .now_playing(PRINCIPAL, at(241_000))
            .is_empty()
    );
    assert_eq!(
        harness
            .service
            .expire_idle(at(241_000 + SESSION_IDLE_TTL_MS as i64)),
        0
    );
}

#[tokio::test]
async fn eviction_keeps_a_play_that_is_close_to_qualifying() {
    let harness = harness(&[track(1, Some(300_000))]);
    let keeping = client("keeping");

    harness
        .service
        .report(&report(&keeping, PlaybackState::Playing, 0, 0))
        .await
        .unwrap();
    harness
        .service
        .report(&report(&keeping, PlaybackState::Playing, 149_000, 149_000))
        .await
        .unwrap();

    for index in 0..MAX_LIVE_SESSIONS_PER_PRINCIPAL {
        let other = client(&format!("client-{index:02}"));

        harness
            .service
            .report(&report(
                &other,
                PlaybackState::Playing,
                0,
                150_000 + index as i64 * 1_000,
            ))
            .await
            .unwrap();
    }

    let live = harness.service.now_playing(PRINCIPAL, at(165_000));

    assert_eq!(live.len(), MAX_LIVE_SESSIONS_PER_PRINCIPAL);
    assert!(
        live.iter()
            .any(|session| session.client().as_str() == "keeping")
    );
    assert!(
        !live
            .iter()
            .any(|session| session.client().as_str() == "client-00")
    );

    let session = harness
        .service
        .report(&report(&keeping, PlaybackState::Playing, 151_000, 151_000))
        .await
        .unwrap();

    assert_eq!(session.listened_ms(), 151_000);

    let rows = harness.ledger.rows();

    assert_eq!(rows.len(), 1);
    assert_eq!(
        rows[0].extent().map(|extent| extent.listened_ms()),
        Some(151_000)
    );
}

#[tokio::test]
async fn a_principal_holds_one_live_session_per_client_up_to_the_bound() {
    let harness = harness(&[track(1, Some(300_000))]);

    for index in 0..(MAX_LIVE_SESSIONS_PER_PRINCIPAL + 1) {
        let client = client(&format!("client-{index:02}"));

        harness
            .service
            .report(&report(
                &client,
                PlaybackState::Playing,
                0,
                index as i64 * 1_000,
            ))
            .await
            .unwrap();
    }

    let live = harness.service.now_playing(PRINCIPAL, at(16_000));

    assert_eq!(live.len(), MAX_LIVE_SESSIONS_PER_PRINCIPAL);
    assert!(
        live.iter()
            .all(|session| session.client().as_str() != "client-00")
    );
    assert!(
        live.iter()
            .any(|session| session.client().as_str() == "client-16")
    );
}
