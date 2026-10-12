use std::{
    collections::HashSet,
    path::{Component, Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use uuid::Uuid;

use crate::features::music_library::catalogue::{CompletionService, LibraryService};
use crate::features::music_library::domain::{AlbumId, TrackId};
use crate::platform::clock::Clock;

use super::config::AcquisitionConfig;
use super::domain::{
    AcquisitionOrigin, Attempt, AttemptFile, AttemptState, AttemptUpdate, FinishIntent, Request,
    RequestState, Round,
};
use super::ports::{
    AcquisitionTarget, CandidateLimit, CandidateSearch, ExpectedTrack, ImportError, ImportJob,
    ImportedItem, Importer, LibraryOwnership, LookupError, MbId, MbRecording, MbRelease,
    MusicBrainzLookup, OwnershipError, RemoteTransfer, Slot, TransferError, TransferHandle,
    TransferPhase, TransferProgress, TransferRequest,
};
use super::selection;
use super::storage::{PostgresAcquisitionStore, StorageError};

const SEARCH_LIMIT: u16 = 500;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    Wait(Duration),
    Done,
}

#[derive(Debug, Clone)]
pub struct RequestOutcome {
    pub request: Request,
    pub created: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum AcquisitionError {
    #[error("the acquisition request does not exist")]
    NotFound,
    #[error("an acquisition provider is unavailable")]
    Unavailable,
    #[error("an acquisition provider rejected the request")]
    Failed,
    #[error(transparent)]
    Repository(#[from] StorageError),
}

#[derive(Clone)]
pub struct AcquisitionPorts {
    pub search: Arc<dyn CandidateSearch>,
    pub transfer: Arc<dyn RemoteTransfer>,
    pub lookup: Arc<dyn MusicBrainzLookup>,
    pub importer: Arc<dyn Importer>,
    pub ownership: Arc<dyn LibraryOwnership>,
    pub completion: Arc<CompletionService>,
}

impl AcquisitionPorts {
    pub fn new(
        search: Arc<dyn CandidateSearch>,
        transfer: Arc<dyn RemoteTransfer>,
        lookup: Arc<dyn MusicBrainzLookup>,
        importer: Arc<dyn Importer>,
        ownership: Arc<dyn LibraryOwnership>,
        completion: Arc<CompletionService>,
    ) -> Self {
        Self {
            search,
            transfer,
            lookup,
            importer,
            ownership,
            completion,
        }
    }
}

enum Phase {
    Again,
    Wait(Duration),
    Done,
}

#[derive(Default)]
struct PinSummary {
    origin_entries: usize,
    candidate_track_counts: Vec<usize>,
}

impl PinSummary {
    fn reason(&self, code: &str) -> String {
        let counts = self
            .candidate_track_counts
            .iter()
            .map(usize::to_string)
            .collect::<Vec<_>>()
            .join(",");
        format!(
            "{code}: origin {} tracks; candidates {counts}",
            self.origin_entries
        )
    }
}

enum Pinned {
    NotFound(Option<PinSummary>),
    Ambiguous(Option<PinSummary>),
    Unique {
        target: AcquisitionTarget,
        artist: String,
        title: String,
        expected: Vec<ExpectedTrack>,
    },
}

pub struct AcquisitionService {
    store: Arc<PostgresAcquisitionStore>,
    ports: AcquisitionPorts,
    library: Arc<LibraryService>,
    config: AcquisitionConfig,
    clock: Clock,
    #[cfg(all(test, feature = "postgres-integration"))]
    conflict_hook: Option<ConflictHook>,
    #[cfg(all(test, feature = "postgres-integration"))]
    before_insert_hook: Option<ConflictHook>,
    #[cfg(all(test, feature = "postgres-integration"))]
    finish_hook: Option<FinishHook>,
}

#[cfg(all(test, feature = "postgres-integration"))]
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum FinishBoundary {
    BeforeIntent,
    IntentDurable,
    BeforeCommit,
}

#[cfg(all(test, feature = "postgres-integration"))]
pub(crate) type FinishHook = Arc<
    dyn Fn(FinishBoundary) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>
        + Send
        + Sync,
>;

#[cfg(all(test, feature = "postgres-integration"))]
pub(crate) type ConflictHook =
    Arc<dyn Fn() -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>> + Send + Sync>;

impl AcquisitionService {
    pub fn new(
        store: Arc<PostgresAcquisitionStore>,
        ports: AcquisitionPorts,
        library: Arc<LibraryService>,
        config: AcquisitionConfig,
        clock: Clock,
    ) -> Self {
        Self {
            store,
            ports,
            library,
            config,
            clock,
            #[cfg(all(test, feature = "postgres-integration"))]
            conflict_hook: None,
            #[cfg(all(test, feature = "postgres-integration"))]
            before_insert_hook: None,
            #[cfg(all(test, feature = "postgres-integration"))]
            finish_hook: None,
        }
    }

    #[cfg(all(test, feature = "postgres-integration"))]
    pub(crate) fn with_conflict_hook(mut self, hook: ConflictHook) -> Self {
        self.conflict_hook = Some(hook);
        self
    }

    #[cfg(all(test, feature = "postgres-integration"))]
    pub(crate) fn with_before_insert_hook(mut self, hook: ConflictHook) -> Self {
        self.before_insert_hook = Some(hook);
        self
    }

    #[cfg(all(test, feature = "postgres-integration"))]
    pub(crate) fn set_finish_hook(&mut self, hook: FinishHook) {
        self.finish_hook = Some(hook);
    }

    pub async fn request(
        &self,
        origin: AcquisitionOrigin,
    ) -> Result<RequestOutcome, AcquisitionError> {
        self.ensure_origin(&origin).await?;

        let (kind, id) = origin.key();
        if let Some(existing) = self.store.find_live_by_origin(kind, &id).await? {
            return Ok(RequestOutcome {
                request: existing,
                created: false,
            });
        }

        loop {
            #[cfg(all(test, feature = "postgres-integration"))]
            if let Some(hook) = &self.before_insert_hook {
                hook().await;
            }

            let request_id = Uuid::now_v7();

            if let Some(request) = self.store.insert_request(request_id, &origin).await? {
                return Ok(RequestOutcome {
                    request,
                    created: true,
                });
            }

            #[cfg(all(test, feature = "postgres-integration"))]
            if let Some(hook) = &self.conflict_hook {
                hook().await;
            }

            if let Some(existing) = self.store.find_live_by_origin(kind, &id).await? {
                return Ok(RequestOutcome {
                    request: existing,
                    created: false,
                });
            }
        }
    }

    pub async fn get(&self, id: Uuid) -> Result<Option<Request>, AcquisitionError> {
        Ok(self.store.find_request(id).await?)
    }

    #[tracing::instrument(skip_all, fields(operation = "live_track_origins"))]
    pub async fn live_track_origins(&self) -> Result<Vec<TrackId>, AcquisitionError> {
        Ok(self.store.live_track_origins().await?)
    }

    pub async fn list_by_origin(
        &self,
        origin_ids: &[String],
    ) -> Result<Vec<Request>, AcquisitionError> {
        Ok(self.store.list_latest_by_origins(origin_ids).await?)
    }

    #[tracing::instrument(skip_all, fields(request_id = %id, state = tracing::field::Empty, attempt = tracing::field::Empty, round = tracing::field::Empty, winner = tracing::field::Empty))]
    pub async fn advance(&self, id: Uuid) -> Result<Step, AcquisitionError> {
        loop {
            let request = self
                .store
                .find_request(id)
                .await?
                .ok_or(AcquisitionError::NotFound)?;
            tracing::Span::current().record("state", request.state.as_str());
            tracing::Span::current().record("attempt", request.attempts);
            if let Some(round) = &request.round {
                tracing::Span::current().record("round", round.attempts.first().copied());
                tracing::Span::current().record("winner", round.winner);
            }

            let phase = match request.state {
                RequestState::Pending => self.pin(&request).await?,
                RequestState::Searching => self.search(&request).await?,
                RequestState::Transferring | RequestState::Importing => {
                    self.run_round(&request).await?
                }
                _ => {
                    self.resume_cleanup(&request).await?;

                    return Ok(Step::Done);
                }
            };

            match phase {
                Phase::Again => continue,
                Phase::Wait(duration) => return Ok(Step::Wait(duration)),
                Phase::Done => return Ok(Step::Done),
            }
        }
    }

    async fn ensure_origin(&self, origin: &AcquisitionOrigin) -> Result<(), AcquisitionError> {
        match origin {
            AcquisitionOrigin::Album(id) => {
                let album = self
                    .library
                    .album_detail(id, None)
                    .await
                    .map_err(|_| AcquisitionError::Failed)?;

                if album.is_none() {
                    return Err(AcquisitionError::NotFound);
                }
            }
            AcquisitionOrigin::Track(id) => {
                let track = self
                    .library
                    .track(id)
                    .await
                    .map_err(|_| AcquisitionError::Failed)?;

                if track.is_none() {
                    return Err(AcquisitionError::NotFound);
                }
            }
            AcquisitionOrigin::Release(_) | AcquisitionOrigin::Recording(_) => {}
        }

        Ok(())
    }

    async fn pin(&self, request: &Request) -> Result<Phase, AcquisitionError> {
        let pinned = match &request.origin {
            AcquisitionOrigin::Album(id) => self.pin_album(id).await?,
            AcquisitionOrigin::Track(id) => self.pin_track(id).await?,
            AcquisitionOrigin::Release(id) => self.pin_release(*id).await?,
            AcquisitionOrigin::Recording(id) => self.pin_recording(*id).await?,
        };

        match pinned {
            Pinned::NotFound(summary) => {
                self.pin_failed(
                    request.id,
                    RequestState::ReleaseNotFound,
                    "release_not_found",
                    summary,
                )
                .await
            }
            Pinned::Ambiguous(summary) => {
                self.pin_failed(
                    request.id,
                    RequestState::ReleaseAmbiguous,
                    "release_ambiguous",
                    summary,
                )
                .await
            }
            Pinned::Unique {
                target,
                artist,
                title,
                expected,
            } => {
                self.store
                    .set_target(request.id, &target, &artist, &title, &expected)
                    .await?;

                if self
                    .ports
                    .ownership
                    .owns(&target)
                    .await
                    .map_err(map_ownership)?
                {
                    self.store
                        .set_terminal(request.id, RequestState::AlreadyOwned, None)
                        .await?;

                    return Ok(Phase::Done);
                }

                self.store
                    .set_state(request.id, RequestState::Searching)
                    .await?;

                Ok(Phase::Again)
            }
        }
    }

    async fn pin_failed(
        &self,
        id: Uuid,
        state: RequestState,
        code: &'static str,
        summary: Option<PinSummary>,
    ) -> Result<Phase, AcquisitionError> {
        let reason = summary
            .as_ref()
            .map_or_else(|| code.to_owned(), |summary| summary.reason(code));
        let summary = summary.unwrap_or_default();
        tracing::info!(
            reason = code,
            origin_entries = summary.origin_entries,
            candidate_releases = summary.candidate_track_counts.len(),
            candidate_track_counts = ?summary.candidate_track_counts,
            "acquisition pin failed"
        );
        self.store.set_terminal(id, state, Some(&reason)).await?;
        Ok(Phase::Done)
    }

    async fn pin_album(&self, id: &AlbumId) -> Result<Pinned, AcquisitionError> {
        let Some(detail) = self
            .ports
            .completion
            .album_detail(id, None)
            .await
            .map_err(|_| AcquisitionError::Failed)?
        else {
            return Ok(Pinned::NotFound(None));
        };
        let artist = detail
            .album
            .album_artist()
            .map(|artist| artist.name().to_owned())
            .unwrap_or_default();
        let title = detail.album.album().title().to_owned();
        let entries = super::pin::collapse_album_entries(&detail.entries);
        let expected = entries
            .iter()
            .map(|entry| ExpectedTrack {
                disc: entry.membership().disc_number().unwrap_or(1),
                position: entry
                    .membership()
                    .track_number()
                    .unwrap_or_else(|| u16::try_from(entry.membership().position()).unwrap_or(1)),
                title: entry.track().title().to_owned(),
                length_ms: entry
                    .track()
                    .duration_ms()
                    .and_then(|duration| u32::try_from(duration).ok()),
            })
            .collect::<Vec<_>>();
        let candidates = self.lookup_releases(&artist, &title).await?;
        let summary = PinSummary {
            origin_entries: expected.len(),
            candidate_track_counts: candidates
                .iter()
                .map(|release| release.tracks.len())
                .collect(),
        };

        Ok(match super::pin::pin_release(candidates, &expected) {
            super::pin::Pinned::Unique(release) => Pinned::Unique {
                target: AcquisitionTarget::Release(release.id),
                artist: release.artist,
                title: release.title,
                expected: release.tracks,
            },
            super::pin::Pinned::NotFound => Pinned::NotFound(Some(summary)),
            super::pin::Pinned::Ambiguous => Pinned::Ambiguous(Some(summary)),
        })
    }

    async fn pin_track(&self, id: &TrackId) -> Result<Pinned, AcquisitionError> {
        let Some(track) = self
            .library
            .track(id)
            .await
            .map_err(|_| AcquisitionError::Failed)?
        else {
            return Ok(Pinned::NotFound(None));
        };
        let artist = track
            .artists()
            .first()
            .map(|artist| artist.name().to_owned())
            .unwrap_or_default();
        let title = track.title().to_owned();
        let length_ms = track
            .duration_ms()
            .and_then(|duration| u32::try_from(duration).ok());
        let candidates = self.lookup_recordings(&artist, &title).await?;

        Ok(
            match super::pin::pin_recording(candidates, &title, length_ms) {
                super::pin::Pinned::Unique(recording) => recording_pinned(recording),
                super::pin::Pinned::NotFound => Pinned::NotFound(None),
                super::pin::Pinned::Ambiguous => Pinned::Ambiguous(None),
            },
        )
    }

    async fn pin_release(&self, id: MbId) -> Result<Pinned, AcquisitionError> {
        let Some(release) = self.ports.lookup.release(&id).await.map_err(map_lookup)? else {
            return Ok(Pinned::NotFound(None));
        };

        Ok(Pinned::Unique {
            target: AcquisitionTarget::Release(release.id),
            artist: release.artist,
            title: release.title,
            expected: release.tracks,
        })
    }

    async fn pin_recording(&self, id: MbId) -> Result<Pinned, AcquisitionError> {
        let Some(recording) = self.ports.lookup.recording(&id).await.map_err(map_lookup)? else {
            return Ok(Pinned::NotFound(None));
        };

        Ok(recording_pinned(recording))
    }

    async fn lookup_releases(
        &self,
        artist: &str,
        title: &str,
    ) -> Result<Vec<MbRelease>, AcquisitionError> {
        self.ports
            .lookup
            .release_candidates(artist, title)
            .await
            .map_err(map_lookup)
    }

    async fn lookup_recordings(
        &self,
        artist: &str,
        title: &str,
    ) -> Result<Vec<MbRecording>, AcquisitionError> {
        self.ports
            .lookup
            .recording_candidates(artist, title)
            .await
            .map_err(map_lookup)
    }

    async fn search(&self, request: &Request) -> Result<Phase, AcquisitionError> {
        if !request.candidates.is_empty() {
            return self.begin_round(request).await;
        }

        let Some(target) = request.target else {
            return Ok(Phase::Done);
        };
        let artist = request.artist.as_deref().unwrap_or_default();
        let title = request.title.as_deref().unwrap_or_default();
        let texts = selection::query_texts(artist, title);
        let limit = CandidateLimit::new(SEARCH_LIMIT).expect("the search limit is valid");
        let budget = self.config.candidate_budget;

        for text in texts {
            let candidates = self
                .ports
                .search
                .search(&text, limit)
                .await
                .map_err(map_transfer)?;
            let accepted = match target {
                AcquisitionTarget::Release(_) => selection::accepted_for_release(
                    candidates,
                    request.expected_tracks.len(),
                    &request
                        .expected_tracks
                        .iter()
                        .map(|track| track.length_ms)
                        .collect::<Vec<_>>(),
                    budget,
                ),
                AcquisitionTarget::Recording(_) => selection::accepted_for_recording(
                    candidates,
                    title,
                    request
                        .expected_tracks
                        .first()
                        .and_then(|track| track.length_ms),
                    budget,
                ),
            };

            if !accepted.is_empty() {
                self.store.set_candidates(request.id, &accepted).await?;
                let mut selected = request.clone();
                selected.candidates = accepted;
                return self.begin_round(&selected).await;
            }
        }

        self.store
            .set_terminal(request.id, RequestState::NoCandidate, Some("no_candidate"))
            .await?;

        Ok(Phase::Done)
    }

    async fn begin_round(&self, request: &Request) -> Result<Phase, AcquisitionError> {
        let previous = self.store.load_attempts(request.id).await?;
        let mut users = previous
            .iter()
            .flat_map(|attempt| &attempt.files)
            .map(|file| file.input().user().to_owned())
            .collect::<HashSet<_>>();
        let mut attempts = Vec::new();
        for (index, candidate) in request.candidates.iter().enumerate() {
            let number = u16::try_from(index + 1).map_err(|_| AcquisitionError::Failed)?;
            if previous.iter().any(|attempt| attempt.number == number) {
                continue;
            }
            if candidate.files.is_empty() {
                self.store
                    .set_terminal(request.id, RequestState::NoCandidate, Some("no_candidate"))
                    .await?;
                return Ok(Phase::Done);
            }
            let user = candidate.user().ok_or(AcquisitionError::Failed)?;
            if !users.insert(user.to_owned()) {
                continue;
            }
            attempts.push((number, attempt_files(candidate)));
            if attempts.len() == self.config.race_width {
                break;
            }
        }
        if attempts.is_empty() {
            self.store
                .set_terminal(request.id, RequestState::Exhausted, Some("exhausted"))
                .await?;
            return Ok(Phase::Done);
        }
        let round = Round {
            attempts: attempts.iter().map(|(number, _)| *number).collect(),
            started_at: self.clock.now(),
            winner: None,
            won_at: None,
        };
        self.store
            .dispatch_round(request.id, &round, &attempts)
            .await?;
        Ok(Phase::Again)
    }

    async fn run_round(&self, request: &Request) -> Result<Phase, AcquisitionError> {
        let Some(round) = &request.round else {
            return self.resume_single_attempt(request).await;
        };
        let mut attempts = Vec::new();
        for number in &round.attempts {
            let attempt = self
                .store
                .load_attempt(request.id, *number)
                .await?
                .ok_or(AcquisitionError::Failed)?;
            if let Some(intent) = &attempt.finish_intent {
                self.resume_finish(request, *number, intent).await?;
                return Ok(Phase::Again);
            }
            attempts.push(attempt);
        }

        if let Some(number) = round.winner {
            let winner = attempts
                .iter()
                .find(|attempt| attempt.number == number)
                .ok_or(AcquisitionError::Failed)?;
            return match winner.state {
                AttemptState::Failed => self.begin_round(request).await,
                AttemptState::Imported => {
                    self.finish_attempt(
                        request,
                        winner,
                        Finish::Imported(winner.imported.clone()),
                        &winner.handles,
                        winner.delivered(),
                    )
                    .await
                }
                AttemptState::Importing => self.import_phase(request, winner).await,
                AttemptState::Transferring => self.transfer_phase(request, winner).await,
            };
        }

        let live = attempts
            .iter()
            .filter(|attempt| attempt.state == AttemptState::Transferring)
            .collect::<Vec<_>>();
        if live.is_empty() {
            return self.begin_round(request).await;
        }
        if self.start_expired(round, self.clock.now()) {
            return self.timeout_round(request, round, &live).await;
        }
        let mut started = false;
        for attempt in &live {
            if attempt.handles.is_empty() || attempt.handles.iter().any(Option::is_none) {
                self.start_attempt(request.id, attempt.number).await?;
                started = true;
            }
        }
        if started {
            return Ok(Phase::Again);
        }
        self.poll_round(request, round, &live).await
    }

    async fn resume_single_attempt(&self, request: &Request) -> Result<Phase, AcquisitionError> {
        let Some(attempt) = self.store.load_current_attempt(request.id).await? else {
            return self.begin_round(request).await;
        };
        let round = Round {
            attempts: vec![attempt.number],
            started_at: attempt.created_at,
            winner: Some(attempt.number),
            won_at: Some(attempt.created_at),
        };
        let finishes = if attempt.state == AttemptState::Failed && attempt.finish_intent.is_none() {
            vec![(
                attempt.number,
                finish_intent(
                    &attempt,
                    Finish::Failed(attempt.reason.clone().ok_or(AcquisitionError::Failed)?),
                    &attempt.handles,
                    attempt.delivered(),
                ),
            )]
        } else {
            Vec::new()
        };
        self.store
            .record_round_outcome(request.id, &round, &finishes, &[])
            .await?;
        Ok(Phase::Again)
    }

    async fn poll_round(
        &self,
        request: &Request,
        round: &Round,
        attempts: &[&Attempt],
    ) -> Result<Phase, AcquisitionError> {
        let mut observed = Vec::new();
        for attempt in attempts {
            observed.push(self.observe_transfer(attempt).await?);
        }
        let now = self.clock.now();
        let expired = self.start_expired(round, now);
        let winner = if expired {
            None
        } else {
            observed.iter().position(|slots| {
                !transfer_failed(slots) && slots.iter().any(|slot| matches!(
                    slot,
                    Slot::Accepted(progress) if progress.bytes_done.unwrap_or_default() > 0
                        || matches!(progress.phase, TransferPhase::Active | TransferPhase::ReportedComplete)
                ))
            })
        };
        let mut decided = round.clone();
        if let Some(index) = winner {
            decided.winner = Some(attempts[index].number);
            decided.won_at = Some(now);
        }
        let mut finishes = attempts
            .iter()
            .zip(&observed)
            .enumerate()
            .filter_map(|(index, (attempt, slots))| {
                let reason = if winner.is_some() && winner != Some(index) {
                    "lost_race"
                } else if winner.is_none() && expired {
                    "start_timeout"
                } else if transfer_failed(slots) {
                    "transfer_failed"
                } else {
                    return None;
                };
                Some((
                    attempt.number,
                    finish_intent(
                        attempt,
                        Finish::Failed(reason.to_owned()),
                        &attempt.handles,
                        &delivered_of(slots),
                    ),
                ))
            })
            .collect::<Vec<_>>();
        if winner.is_some() || !finishes.is_empty() {
            let winner_paths = winner
                .map(|index| delivered_of(&observed[index]))
                .unwrap_or_default()
                .into_iter()
                .filter(|path| matches!(self.staged_input(path), StagedInput::Ready(_)))
                .collect::<Vec<_>>();
            let protected = self
                .store
                .record_round_outcome(
                    request.id,
                    &decided,
                    &finishes,
                    &delivered_strings(&winner_paths),
                )
                .await?;
            if !protected {
                let index = winner.ok_or(AcquisitionError::Failed)?;
                let attempt = attempts[index];
                finishes.push((
                    attempt.number,
                    finish_intent(
                        attempt,
                        Finish::Failed("staging_conflict".to_owned()),
                        &attempt.handles,
                        &delivered_of(&observed[index]),
                    ),
                ));
                self.store
                    .record_round_outcome(request.id, &decided, &finishes, &[])
                    .await?;
            }
            for (number, intent) in &finishes {
                self.resume_finish(request, *number, intent).await?;
            }
            if let Some(index) = winner.filter(|_| protected) {
                let mut request = request.clone();
                request.round = Some(decided);
                return self
                    .transfer_observed(&request, attempts[index], &observed[index])
                    .await;
            }
            return Ok(Phase::Again);
        }
        Ok(Phase::Wait(self.config.poll))
    }

    fn start_expired(&self, round: &Round, now: chrono::DateTime<chrono::Utc>) -> bool {
        now >= round.started_at
            + chrono::Duration::from_std(self.config.start_timeout).unwrap_or_default()
    }

    async fn timeout_round(
        &self,
        request: &Request,
        round: &Round,
        attempts: &[&Attempt],
    ) -> Result<Phase, AcquisitionError> {
        let mut finishes = Vec::new();
        for attempt in attempts {
            let recover_start =
                attempt.handles.is_empty() || attempt.handles.iter().any(Option::is_none);
            let delivered = if recover_start {
                attempt.delivered.clone()
            } else {
                delivered_of(&self.observe_transfer(attempt).await?)
            };
            let mut intent = finish_intent(
                attempt,
                Finish::Failed("start_timeout".to_owned()),
                &attempt.handles,
                &delivered,
            );
            intent.recover_start = recover_start;
            finishes.push((attempt.number, intent));
        }
        self.store
            .record_round_outcome(request.id, round, &finishes, &[])
            .await?;
        for (number, intent) in &finishes {
            self.resume_finish(request, *number, intent).await?;
        }
        Ok(Phase::Again)
    }

    async fn observe_transfer(
        &self,
        attempt: &Attempt,
    ) -> Result<Vec<Slot<TransferProgress>>, AcquisitionError> {
        let handles = attempt
            .handles
            .iter()
            .flatten()
            .map(|handle| TransferHandle::new(handle.clone()))
            .collect::<Vec<_>>();
        let slots = self
            .ports
            .transfer
            .status(&handles)
            .await
            .map_err(map_transfer)?;
        if slots.len() != handles.len() {
            return Err(AcquisitionError::Failed);
        }
        Ok(slots)
    }

    async fn start_transfer(
        &self,
        attempt: &Attempt,
    ) -> Result<Vec<Slot<TransferHandle>>, AcquisitionError> {
        let requests = attempt
            .files
            .iter()
            .map(|file| TransferRequest {
                key: file.key(),
                input: file.input().clone(),
            })
            .collect::<Vec<_>>();
        self.ports
            .transfer
            .start(&requests)
            .await
            .map_err(map_transfer)
    }

    async fn start_attempt(&self, id: Uuid, number: u16) -> Result<Phase, AcquisitionError> {
        let attempt = self
            .store
            .load_attempt(id, number)
            .await?
            .ok_or(AcquisitionError::Failed)?;
        if attempt.files.is_empty() {
            self.store
                .record_attempt(
                    id,
                    number,
                    &failed_update("no_candidate", &[], Vec::new()),
                    RequestState::NoCandidate,
                    Some("no_candidate"),
                )
                .await?;

            return Ok(Phase::Done);
        }
        let slots = self.start_transfer(&attempt).await?;
        let accepted = slots.len() == attempt.files.len()
            && slots.iter().all(|slot| matches!(slot, Slot::Accepted(_)));

        if !accepted {
            let handles = transfer_handles(&slots);

            let request = self
                .store
                .find_request(id)
                .await?
                .ok_or(AcquisitionError::NotFound)?;

            return self
                .finish_attempt(
                    &request,
                    &attempt,
                    Finish::Failed("rejected".to_owned()),
                    &handles,
                    &[],
                )
                .await;
        }

        let handles = transfer_handles(&slots);
        let update = AttemptUpdate {
            state: AttemptState::Transferring,
            reason: None,
            handles,
            delivered: Vec::new(),
            imported: Vec::new(),
        };
        self.store
            .record_attempt(id, number, &update, RequestState::Transferring, None)
            .await?;

        Ok(Phase::Again)
    }

    async fn transfer_phase(
        &self,
        request: &Request,
        attempt: &Attempt,
    ) -> Result<Phase, AcquisitionError> {
        if attempt.handles.is_empty() || attempt.handles.iter().any(Option::is_none) {
            return self.start_attempt(request.id, attempt.number).await;
        }

        let slots = self.observe_transfer(attempt).await?;
        self.transfer_observed(request, attempt, &slots).await
    }

    async fn transfer_observed(
        &self,
        request: &Request,
        attempt: &Attempt,
        slots: &[Slot<TransferProgress>],
    ) -> Result<Phase, AcquisitionError> {
        let delivered = delivered_of(slots);
        let complete = !slots.is_empty()
            && slots.iter().all(|slot| {
                matches!(
                    slot,
                    Slot::Accepted(progress)
                        if progress.phase == TransferPhase::ReportedComplete
                            && progress.delivered.is_some()
                )
            });

        let expired = self.clock.now()
            >= request
                .round
                .as_ref()
                .and_then(|round| round.won_at)
                .ok_or(AcquisitionError::Failed)?
                + chrono::Duration::from_std(self.config.transfer_timeout).unwrap_or_default();

        if complete {
            let mut ready = true;
            let mut invalid = false;

            for path in &delivered {
                match self.staged_input(path) {
                    StagedInput::Ready(_) => {}
                    StagedInput::Missing => ready = false,
                    StagedInput::Invalid => invalid = true,
                }
            }

            if invalid {
                return self
                    .finish_attempt(
                        request,
                        attempt,
                        Finish::Failed("staging_conflict".to_owned()),
                        &attempt.handles,
                        &delivered,
                    )
                    .await;
            }

            if ready {
                let paths = delivered_strings(&delivered);
                let update = AttemptUpdate {
                    state: AttemptState::Importing,
                    reason: None,
                    handles: attempt.handles.clone(),
                    delivered: delivered.clone(),
                    imported: Vec::new(),
                };

                if !self
                    .store
                    .claim_importing(request.id, attempt.number, &update, &paths)
                    .await?
                {
                    return self
                        .finish_attempt(
                            request,
                            attempt,
                            Finish::Failed("staging_conflict".to_owned()),
                            &attempt.handles,
                            &delivered,
                        )
                        .await;
                }

                return Ok(Phase::Again);
            }

            if !expired {
                return Ok(Phase::Wait(self.config.poll));
            }
        }

        let failed = transfer_failed(slots);

        if failed || expired {
            let reason = if failed {
                "transfer_failed"
            } else {
                "transfer_timeout"
            };

            return self
                .finish_attempt(
                    request,
                    attempt,
                    Finish::Failed(reason.to_owned()),
                    &attempt.handles,
                    &delivered,
                )
                .await;
        }

        Ok(Phase::Wait(self.config.poll))
    }

    async fn import_phase(
        &self,
        request: &Request,
        attempt: &Attempt,
    ) -> Result<Phase, AcquisitionError> {
        let Some(target) = request.target else {
            return Ok(Phase::Done);
        };

        if self
            .ports
            .ownership
            .owns(&target)
            .await
            .map_err(map_ownership)?
        {
            return self
                .finish_attempt(
                    request,
                    attempt,
                    Finish::Completed,
                    &attempt.handles,
                    attempt.delivered(),
                )
                .await;
        }

        let delivered = attempt.delivered().to_vec();
        let paths = delivered
            .iter()
            .filter_map(|path| self.staged(path))
            .collect::<Vec<_>>();

        if paths.len() != delivered.len() {
            return self
                .finish_attempt(
                    request,
                    attempt,
                    Finish::Failed("staging_conflict".to_owned()),
                    &attempt.handles,
                    &delivered,
                )
                .await;
        }

        let import_path = self
            .materialise_import_input(request.id, attempt.number, target, &paths)
            .await?;

        let outcome = self
            .ports
            .importer
            .import(&ImportJob {
                target,
                path: import_path,
            })
            .await;

        let finish = match outcome {
            Err(ImportError::Unavailable) => return Err(AcquisitionError::Unavailable),
            Err(ImportError::Failed { status }) => {
                Finish::Failed(format!("import_failed:{status:?}"))
            }
            Err(ImportError::TimedOut) => Finish::Failed("import_timed_out".to_owned()),
            Err(ImportError::Stalled) => Finish::Failed("import_stalled".to_owned()),
            Ok(outcome) if outcome.items.is_empty() => Finish::Failed("import_skipped".to_owned()),
            Ok(outcome) if import_matches(&outcome, &target, request.expected_tracks.len()) => {
                Finish::Imported(outcome.items)
            }
            Ok(outcome) => {
                let reason = outcome
                    .items
                    .iter()
                    .map(|item| item.path.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ");

                Finish::Mismatch(reason, outcome.items)
            }
        };

        self.finish_attempt(request, attempt, finish, &attempt.handles, &delivered)
            .await
    }

    async fn finish_attempt(
        &self,
        request: &Request,
        attempt: &Attempt,
        finish: Finish,
        handles: &[Option<String>],
        delivered: &[PathBuf],
    ) -> Result<Phase, AcquisitionError> {
        let intent = finish_intent(attempt, finish, handles, delivered);
        #[cfg(all(test, feature = "postgres-integration"))]
        if let Some(hook) = &self.finish_hook {
            hook(FinishBoundary::BeforeIntent).await;
        }
        let intent = self
            .store
            .persist_finish(request.id, attempt.number, &intent)
            .await?;

        self.resume_finish(request, attempt.number, &intent).await
    }

    async fn resume_finish(
        &self,
        request: &Request,
        number: u16,
        intent: &FinishIntent,
    ) -> Result<Phase, AcquisitionError> {
        #[cfg(all(test, feature = "postgres-integration"))]
        if let Some(hook) = &self.finish_hook {
            hook(FinishBoundary::IntentDurable).await;
        }
        let intent = self
            .recover_finish_start(request.id, number, intent)
            .await?;
        if intent.cancel {
            self.claim_subset(request.id, number, &intent.update.delivered)
                .await?;
            self.cancel_all(&intent.update.handles).await?;
        }
        self.cleanup_claimed_files(request.id, number).await?;
        #[cfg(all(test, feature = "postgres-integration"))]
        if let Some(hook) = &self.finish_hook {
            hook(FinishBoundary::BeforeCommit).await;
        }
        self.store
            .complete_finish(request.id, number, &intent)
            .await?;

        if intent.request_state.is_terminal() {
            Ok(Phase::Done)
        } else {
            Ok(Phase::Again)
        }
    }

    async fn recover_finish_start(
        &self,
        id: Uuid,
        number: u16,
        intent: &FinishIntent,
    ) -> Result<FinishIntent, AcquisitionError> {
        if !intent.recover_start {
            return Ok(intent.clone());
        }
        let mut attempt = self
            .store
            .load_attempt(id, number)
            .await?
            .ok_or(AcquisitionError::Failed)?;
        if attempt.files.is_empty() {
            return Err(AcquisitionError::Failed);
        }
        let slots = self.start_transfer(&attempt).await?;
        attempt.handles = transfer_handles(&slots);
        let mut recovered = intent.clone();
        recovered.update.handles = attempt.handles.clone();
        if attempt.handles.iter().any(Option::is_some) {
            let delivered = delivered_of(&self.observe_transfer(&attempt).await?);
            for path in delivered {
                if !recovered.update.delivered.contains(&path) {
                    recovered.update.delivered.push(path);
                }
            }
        }
        recovered.recover_start = false;
        Ok(self
            .store
            .persist_recovered_finish(id, number, intent, &recovered)
            .await?)
    }

    async fn claim_subset(
        &self,
        id: Uuid,
        number: u16,
        delivered: &[PathBuf],
    ) -> Result<(), AcquisitionError> {
        let ready = delivered
            .iter()
            .filter(|path| matches!(self.staged_input(path), StagedInput::Ready(_)))
            .map(|path| path.display().to_string())
            .collect::<Vec<_>>();

        if !ready.is_empty() {
            self.store
                .claim_paths_ignore_conflicts(id, number, &ready)
                .await?;
        }

        Ok(())
    }

    async fn materialise_import_input(
        &self,
        id: Uuid,
        number: u16,
        target: AcquisitionTarget,
        paths: &[PathBuf],
    ) -> Result<PathBuf, AcquisitionError> {
        let Some(staging) = self.config.staging_dir() else {
            return Err(AcquisitionError::Failed);
        };
        let staging = staging.to_owned();
        let paths = paths.to_vec();
        tokio::task::spawn_blocking(move || {
            build_import_input(&staging, id, number, target, &paths)
                .map_err(|_| AcquisitionError::Failed)
        })
        .await
        .map_err(|_| AcquisitionError::Failed)?
    }

    async fn resume_cleanup(&self, request: &Request) -> Result<(), AcquisitionError> {
        if let Some(attempt) = self.store.load_current_attempt(request.id).await? {
            let intent = match &attempt.finish_intent {
                Some(intent) => intent.clone(),
                None => {
                    let intent = FinishIntent {
                        update: AttemptUpdate {
                            state: attempt.state,
                            reason: attempt.reason.clone(),
                            handles: attempt.handles.clone(),
                            delivered: attempt.delivered().to_vec(),
                            imported: attempt.imported.clone(),
                        },
                        request_state: request.state,
                        request_reason: request.reason.clone(),
                        cancel: false,
                        recover_start: false,
                    };
                    self.store
                        .persist_finish(request.id, attempt.number, &intent)
                        .await?
                }
            };
            self.resume_finish(request, attempt.number, &intent).await?;
        }

        Ok(())
    }

    async fn cleanup_claimed_files(&self, id: Uuid, number: u16) -> Result<(), AcquisitionError> {
        let claimed = self.store.claimed_paths(id, number).await?;
        let paths = claimed
            .iter()
            .filter_map(|path| self.staged(Path::new(path)))
            .collect::<Vec<_>>();
        self.cleanup(&paths).await;

        if let Some(staging) = self.config.staging_dir() {
            match checked_import_dir(staging, id, number) {
                Ok(Some(dir)) => {
                    if let Err(error) = tokio::fs::remove_dir_all(dir).await
                        && error.kind() != std::io::ErrorKind::NotFound
                    {
                        tracing::debug!(%error, "a private import directory could not be removed");
                    }
                }
                Ok(None) => {}
                Err(error) => {
                    tracing::debug!(%error, "a private import directory could not be inspected");
                }
            }
        }

        Ok(())
    }

    async fn cancel_all(&self, handles: &[Option<String>]) -> Result<(), AcquisitionError> {
        for handle in handles.iter().flatten() {
            match self
                .ports
                .transfer
                .cancel(&TransferHandle::new(handle.clone()), true)
                .await
            {
                Ok(()) => {}
                Err(TransferError::Unavailable) => {
                    return Err(AcquisitionError::Unavailable);
                }
                Err(error) => return Err(map_transfer(error)),
            }
        }

        Ok(())
    }

    fn staged(&self, relative: &Path) -> Option<PathBuf> {
        match self.staged_input(relative) {
            StagedInput::Ready(path) => Some(path),
            StagedInput::Missing | StagedInput::Invalid => None,
        }
    }

    fn staged_input(&self, relative: &Path) -> StagedInput {
        match self.config.staging_dir() {
            Some(staging) => staged_input(staging, relative),
            None => StagedInput::Invalid,
        }
    }

    async fn cleanup(&self, paths: &[PathBuf]) {
        let Some(staging) = self.config.staging_dir() else {
            return;
        };
        let mut parents = HashSet::new();

        for path in paths {
            let Ok(relative) = path.strip_prefix(staging) else {
                continue;
            };

            if contained_file(staging, relative).as_deref() != Some(path.as_path()) {
                continue;
            }

            match tokio::fs::remove_file(path).await {
                Ok(()) => {
                    if let Some(parent) = path.parent() {
                        parents.insert(parent.to_path_buf());
                    }
                }
                Err(error) => {
                    tracing::debug!(%error, "a staged file could not be removed");
                }
            }
        }

        for parent in parents {
            if parent == staging || !parent.starts_with(staging) {
                continue;
            }

            if let Err(error) = tokio::fs::remove_dir(&parent).await
                && error.kind() != std::io::ErrorKind::DirectoryNotEmpty
                && error.kind() != std::io::ErrorKind::NotFound
            {
                tracing::debug!(%error, "a staged directory could not be removed");
            }
        }
    }
}

fn transfer_handles(slots: &[Slot<TransferHandle>]) -> Vec<Option<String>> {
    slots
        .iter()
        .map(|slot| match slot {
            Slot::Accepted(handle) => Some(handle.as_str().to_owned()),
            Slot::Rejected(_) | Slot::Missing => None,
        })
        .collect()
}

fn transfer_failed(slots: &[Slot<TransferProgress>]) -> bool {
    slots.iter().any(|slot| match slot {
        Slot::Missing | Slot::Rejected(_) => true,
        Slot::Accepted(progress) => matches!(
            progress.phase,
            TransferPhase::Failed | TransferPhase::Cancelled
        ),
    })
}

fn finish_intent(
    attempt: &Attempt,
    finish: Finish,
    handles: &[Option<String>],
    delivered: &[PathBuf],
) -> FinishIntent {
    let (update, request_state, request_reason) = match finish {
        Finish::Completed => (
            AttemptUpdate {
                state: AttemptState::Imported,
                reason: None,
                handles: handles.to_vec(),
                delivered: delivered.to_vec(),
                imported: attempt.imported.clone(),
            },
            RequestState::Completed,
            None,
        ),
        Finish::Failed(reason) => (
            failed_update(&reason, handles, delivered.to_vec()),
            RequestState::Transferring,
            None,
        ),
        Finish::Imported(items) => (
            AttemptUpdate {
                state: AttemptState::Imported,
                reason: None,
                handles: handles.to_vec(),
                delivered: delivered.to_vec(),
                imported: items,
            },
            RequestState::Completed,
            None,
        ),
        Finish::Mismatch(reason, items) => (
            AttemptUpdate {
                state: AttemptState::Failed,
                reason: Some("import_mismatch".to_owned()),
                handles: handles.to_vec(),
                delivered: delivered.to_vec(),
                imported: items,
            },
            RequestState::ImportMismatch,
            Some(reason),
        ),
    };

    FinishIntent {
        update,
        request_state,
        request_reason,
        cancel: attempt.state == AttemptState::Transferring,
        recover_start: false,
    }
}

fn delivered_strings(delivered: &[PathBuf]) -> Vec<String> {
    delivered
        .iter()
        .map(|path| path.display().to_string())
        .collect()
}

fn attempt_files(candidate: &super::ports::ReleaseCandidate) -> Vec<AttemptFile> {
    candidate
        .files
        .iter()
        .map(|file| AttemptFile::new(Uuid::now_v7(), file.clone()))
        .collect()
}

fn failed_update(
    reason: &str,
    handles: &[Option<String>],
    delivered: Vec<PathBuf>,
) -> AttemptUpdate {
    AttemptUpdate {
        state: AttemptState::Failed,
        reason: Some(reason.to_owned()),
        handles: handles.to_vec(),
        delivered,
        imported: Vec::new(),
    }
}

enum StagedInput {
    Ready(PathBuf),
    Missing,
    Invalid,
}

enum Finish {
    Completed,
    Failed(String),
    Imported(Vec<ImportedItem>),
    Mismatch(String, Vec<ImportedItem>),
}

fn import_scratch(staging: &Path, id: Uuid, number: u16) -> PathBuf {
    staging.join(".whio-import").join(format!("{id}-{number}"))
}

fn invalid_staging_node() -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidInput, "invalid staging node")
}

fn directory_exists(path: &Path) -> std::io::Result<bool> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => Ok(true),
        Ok(_) => Err(invalid_staging_node()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

fn checked_import_dir(staging: &Path, id: Uuid, number: u16) -> std::io::Result<Option<PathBuf>> {
    if !super::config::root_is_trusted(staging) {
        return Err(invalid_staging_node());
    }
    if !directory_exists(staging)? || !directory_exists(&staging.join(".whio-import"))? {
        return Ok(None);
    }
    let dir = import_scratch(staging, id, number);
    if !directory_exists(&dir)? {
        return Ok(None);
    }
    for entry in std::fs::read_dir(&dir)? {
        let metadata = std::fs::symlink_metadata(entry?.path())?;
        if !metadata.is_file() || metadata.file_type().is_symlink() {
            return Err(invalid_staging_node());
        }
    }

    Ok(Some(dir))
}

fn ensure_import_marker(
    marker: &Path,
    create: impl FnOnce(&Path) -> std::io::Result<()>,
) -> std::io::Result<()> {
    if directory_exists(marker)? {
        return Ok(());
    }

    match create(marker) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            if directory_exists(marker)? {
                Ok(())
            } else {
                Err(error)
            }
        }
        Err(error) => Err(error),
    }
}

fn build_import_input(
    staging: &Path,
    id: Uuid,
    number: u16,
    target: AcquisitionTarget,
    paths: &[PathBuf],
) -> std::io::Result<PathBuf> {
    build_import_input_with_marker(staging, id, number, target, paths, |marker| {
        std::fs::create_dir(marker)
    })
}

fn build_import_input_with_marker(
    staging: &Path,
    id: Uuid,
    number: u16,
    target: AcquisitionTarget,
    paths: &[PathBuf],
    create_marker: impl FnOnce(&Path) -> std::io::Result<()>,
) -> std::io::Result<PathBuf> {
    if let Some(existing) = checked_import_dir(staging, id, number)? {
        std::fs::remove_dir_all(existing)?;
    }
    if !directory_exists(staging)? {
        return Err(invalid_staging_node());
    }
    let marker = staging.join(".whio-import");
    ensure_import_marker(&marker, create_marker)?;
    let dir = import_scratch(staging, id, number);
    std::fs::create_dir(&dir)?;
    let mut linked = Vec::new();
    for path in paths {
        let relative = path
            .strip_prefix(staging)
            .map_err(|_| invalid_staging_node())?;
        if contained_file(staging, relative).as_deref() != Some(path.as_path()) {
            return Err(invalid_staging_node());
        }
        let name = path.file_name().ok_or_else(invalid_staging_node)?;
        let destination = dir.join(name);
        link_import_file(path, &destination, |source, destination| {
            std::fs::hard_link(source, destination)
        })?;
        linked.push(destination);
    }

    match target {
        AcquisitionTarget::Release(_) => Ok(dir),
        AcquisitionTarget::Recording(_) => {
            linked.into_iter().next().ok_or_else(invalid_staging_node)
        }
    }
}

fn link_import_file(
    source: &Path,
    destination: &Path,
    link: impl FnOnce(&Path, &Path) -> std::io::Result<()>,
) -> std::io::Result<()> {
    match std::fs::symlink_metadata(destination) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
        Ok(_) => return Err(std::io::Error::from(std::io::ErrorKind::AlreadyExists)),
    }
    match link(source, destination) {
        Ok(()) => Ok(()),
        Err(error) if error.raw_os_error() == Some(libc::EXDEV) => {
            let mut input = std::fs::File::open(source)?;
            let mut output = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(destination)?;
            std::io::copy(&mut input, &mut output)?;

            Ok(())
        }
        Err(error) => Err(error),
    }
}

#[cfg(test)]
#[path = "staging_tests.rs"]
mod staging_tests;

fn contained_file(staging: &Path, relative: &Path) -> Option<PathBuf> {
    match staged_input(staging, relative) {
        StagedInput::Ready(path) => Some(path),
        StagedInput::Missing | StagedInput::Invalid => None,
    }
}

fn staged_input(staging: &Path, relative: &Path) -> StagedInput {
    if relative.as_os_str().is_empty() || !super::config::root_is_trusted(staging) {
        return StagedInput::Invalid;
    }
    let mut path = staging.to_path_buf();
    let mut components = relative.components().peekable();

    while let Some(component) = components.next() {
        let Component::Normal(segment) = component else {
            return StagedInput::Invalid;
        };
        path.push(segment);
        let metadata = match std::fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return StagedInput::Missing;
            }
            Err(_) => return StagedInput::Invalid,
        };

        if metadata.file_type().is_symlink() {
            return StagedInput::Invalid;
        }
        if components.peek().is_some() {
            if !metadata.is_dir() {
                return StagedInput::Invalid;
            }
        } else if !metadata.is_file() {
            return StagedInput::Invalid;
        }
    }

    StagedInput::Ready(path)
}

fn recording_pinned(recording: MbRecording) -> Pinned {
    Pinned::Unique {
        target: AcquisitionTarget::Recording(recording.id),
        expected: vec![ExpectedTrack {
            disc: 1,
            position: 1,
            title: recording.title.clone(),
            length_ms: recording.length_ms,
        }],
        artist: recording.artist,
        title: recording.title,
    }
}

pub(crate) fn map_lookup(error: LookupError) -> AcquisitionError {
    match error {
        LookupError::Unavailable => AcquisitionError::Unavailable,
        LookupError::Malformed => AcquisitionError::Failed,
    }
}

pub(crate) fn map_ownership(error: OwnershipError) -> AcquisitionError {
    match error {
        OwnershipError::Unavailable => AcquisitionError::Unavailable,
        OwnershipError::Failed => AcquisitionError::Failed,
    }
}

pub(crate) fn map_transfer(error: TransferError) -> AcquisitionError {
    match error {
        TransferError::Unavailable => AcquisitionError::Unavailable,
        TransferError::InvalidRequest
        | TransferError::Unsupported
        | TransferError::Malformed
        | TransferError::CredentialsRejected
        | TransferError::Internal => AcquisitionError::Failed,
    }
}

fn delivered_of(slots: &[Slot<super::ports::TransferProgress>]) -> Vec<PathBuf> {
    slots
        .iter()
        .filter_map(|slot| match slot {
            Slot::Accepted(progress) => progress.delivered.clone(),
            _ => None,
        })
        .collect()
}

fn import_matches(
    outcome: &super::ports::ImportOutcome,
    target: &AcquisitionTarget,
    expected_count: usize,
) -> bool {
    match target {
        AcquisitionTarget::Release(id) => {
            let wanted = id.to_string();
            outcome.items.len() == expected_count
                && outcome
                    .items
                    .iter()
                    .all(|item| item.release_id.as_deref() == Some(wanted.as_str()))
        }
        AcquisitionTarget::Recording(id) => {
            let wanted = id.to_string();
            outcome.items.len() == 1
                && outcome.items[0].recording_id.as_deref() == Some(wanted.as_str())
        }
    }
}
