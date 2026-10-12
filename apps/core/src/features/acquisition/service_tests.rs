use std::{
    collections::{HashSet, VecDeque},
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};

use async_trait::async_trait;
use chrono::Utc;
use sqlx::PgPool;
use uuid::Uuid;

use crate::features::acquisition::config::AcquisitionConfig;
use crate::features::acquisition::domain::{
    AcquisitionOrigin, AttemptFile, AttemptState, AttemptUpdate, RequestState,
};
use crate::features::acquisition::ports::{
    AcquisitionTarget, BackendHealth, CandidateFile, CandidateLimit, CandidateSearch,
    ExpectedTrack, FileFacts, ImportError, ImportJob, ImportOutcome, ImportedItem, Importer,
    LibraryOwnership, LookupError, MbId, MbRecording, MbRelease, MusicBrainzLookup, OwnershipError,
    PeerFacts, Rejection, ReleaseCandidate, RemoteTransfer, Slot, TransferError, TransferHandle,
    TransferInput, TransferPhase, TransferProgress, TransferRequest,
};
use crate::features::acquisition::service::{
    AcquisitionError, AcquisitionPorts, AcquisitionService, Step,
};
use crate::features::acquisition::storage::PostgresAcquisitionStore;
use crate::features::music_library::catalogue::{CompletionService, LibraryService, SearchService};
use crate::features::music_library::test_support::PostgresTrackRepository;
use crate::platform::clock::Clock;

struct Hold {
    entered: tokio::sync::mpsc::UnboundedSender<()>,
    release: tokio::sync::Semaphore,
}

impl Hold {
    fn new() -> (Arc<Self>, tokio::sync::mpsc::UnboundedReceiver<()>) {
        let (entered, receiver) = tokio::sync::mpsc::unbounded_channel();

        (
            Arc::new(Self {
                entered,
                release: tokio::sync::Semaphore::new(0),
            }),
            receiver,
        )
    }

    async fn wait(&self) {
        self.entered.send(()).unwrap();
        self.release.acquire().await.unwrap().forget();
    }

    fn release(&self, count: usize) {
        self.release.add_permits(count);
    }

    fn hook(self: &Arc<Self>) -> crate::features::acquisition::service::ConflictHook {
        let hold = Arc::clone(self);
        Arc::new(move || {
            let hold = Arc::clone(&hold);
            Box::pin(async move { hold.wait().await })
        })
    }
}

async fn entered(receiver: &mut tokio::sync::mpsc::UnboundedReceiver<()>) {
    tokio::time::timeout(Duration::from_secs(10), receiver.recv())
        .await
        .expect("the held operation is entered")
        .expect("the entry acknowledgement arrives");
}

async fn joined<T>(handle: tokio::task::JoinHandle<T>) -> T {
    tokio::time::timeout(Duration::from_secs(10), handle)
        .await
        .expect("the released operation finishes")
        .expect("the operation does not panic")
}

fn advance(harness: &Harness, id: Uuid) -> tokio::task::JoinHandle<Result<Step, AcquisitionError>> {
    let service = Arc::clone(&harness.service);
    tokio::spawn(async move { service.advance(id).await })
}

struct FakeSearch {
    responses: Mutex<VecDeque<Result<Vec<ReleaseCandidate>, TransferError>>>,
}

#[async_trait]
impl CandidateSearch for FakeSearch {
    async fn search(
        &self,
        _query: &crate::features::acquisition::ports::SearchText,
        _limit: CandidateLimit,
    ) -> Result<Vec<ReleaseCandidate>, TransferError> {
        self.responses
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or(Ok(Vec::new()))
    }
}

#[derive(Default)]
struct FakeTransfer {
    starts: Mutex<VecDeque<Result<Vec<Slot<TransferHandle>>, TransferError>>>,
    statuses: Mutex<VecDeque<Result<Vec<Slot<TransferProgress>>, TransferError>>>,
    started_keys: Mutex<Vec<Uuid>>,
    cancelled: Mutex<Vec<(String, bool)>>,
    cancels: Mutex<VecDeque<Result<(), TransferError>>>,
    cancel_hold: Mutex<Option<(String, Arc<Hold>)>>,
    removed: Mutex<HashSet<String>>,
    status_calls: std::sync::atomic::AtomicUsize,
    observe_removed: std::sync::atomic::AtomicBool,
    on_status: Mutex<Option<(PathBuf, Vec<u8>)>>,
    status_hold: Mutex<Option<Arc<Hold>>>,
    start_hold: Mutex<Option<Arc<Hold>>>,
}

impl FakeTransfer {
    fn accepted_handles(&self, count: usize) -> Vec<Slot<TransferHandle>> {
        (0..count)
            .map(|index| Slot::Accepted(TransferHandle::new(format!("handle-{index}"))))
            .collect()
    }
}

#[async_trait]
impl RemoteTransfer for FakeTransfer {
    async fn start(
        &self,
        requests: &[TransferRequest],
    ) -> Result<Vec<Slot<TransferHandle>>, TransferError> {
        let hold = self.start_hold.lock().unwrap().take();
        if let Some(hold) = hold {
            hold.wait().await;
        }
        self.started_keys
            .lock()
            .unwrap()
            .extend(requests.iter().map(|request| request.key));

        match self.starts.lock().unwrap().pop_front() {
            Some(response) => response,
            None => Ok(self.accepted_handles(requests.len())),
        }
    }

    async fn status(
        &self,
        handles: &[TransferHandle],
    ) -> Result<Vec<Slot<TransferProgress>>, TransferError> {
        let hold = self.status_hold.lock().unwrap().take();
        if let Some(hold) = hold {
            hold.wait().await;
        }
        if let Some((path, bytes)) = self.on_status.lock().unwrap().take() {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, bytes).unwrap();
        }
        self.status_calls
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        match self.statuses.lock().unwrap().pop_front() {
            Some(response) => response,
            None => Ok(handles
                .iter()
                .map(|handle| {
                    if self
                        .observe_removed
                        .load(std::sync::atomic::Ordering::SeqCst)
                        && self.removed.lock().unwrap().contains(handle.as_str())
                    {
                        Slot::Missing
                    } else {
                        Slot::Accepted(progress(TransferPhase::Active))
                    }
                })
                .collect()),
        }
    }

    async fn cancel(&self, handle: &TransferHandle, remove: bool) -> Result<(), TransferError> {
        self.cancelled
            .lock()
            .unwrap()
            .push((handle.as_str().to_owned(), remove));

        let hold = {
            let mut held = self.cancel_hold.lock().unwrap();
            if held
                .as_ref()
                .is_some_and(|(expected, _)| expected == handle.as_str())
            {
                held.take().map(|(_, hold)| hold)
            } else {
                None
            }
        };
        if let Some(hold) = hold {
            hold.wait().await;
        }
        let result = self.cancels.lock().unwrap().pop_front().unwrap_or(Ok(()));
        if result.is_ok() {
            self.removed
                .lock()
                .unwrap()
                .insert(handle.as_str().to_owned());
        }

        result
    }

    async fn health(&self) -> BackendHealth {
        BackendHealth::Ready
    }
}

struct FakeLookup {
    release: Mutex<Option<Result<Option<MbRelease>, LookupError>>>,
    candidates: Mutex<VecDeque<Result<Vec<MbRelease>, LookupError>>>,
    recordings: Mutex<VecDeque<Result<Vec<MbRecording>, LookupError>>>,
}

#[async_trait]
impl MusicBrainzLookup for FakeLookup {
    async fn release_candidates(
        &self,
        _artist: &str,
        _title: &str,
    ) -> Result<Vec<MbRelease>, LookupError> {
        self.candidates
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or(Ok(Vec::new()))
    }

    async fn release(&self, _id: &MbId) -> Result<Option<MbRelease>, LookupError> {
        self.release.lock().unwrap().clone().unwrap_or(Ok(None))
    }

    async fn recording_candidates(
        &self,
        _artist: &str,
        _title: &str,
    ) -> Result<Vec<MbRecording>, LookupError> {
        self.recordings
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or(Ok(Vec::new()))
    }

    async fn recording(&self, _id: &MbId) -> Result<Option<MbRecording>, LookupError> {
        Ok(None)
    }
}

#[derive(Default)]
struct FakeImporter {
    outcomes: Mutex<VecDeque<Result<ImportOutcome, ImportError>>>,
    seen: Mutex<Vec<(PathBuf, Vec<String>)>>,
    hold: Mutex<Option<Arc<Hold>>>,
    after_hold: Mutex<Vec<Vec<String>>>,
}

#[async_trait]
impl Importer for FakeImporter {
    async fn import(&self, job: &ImportJob) -> Result<ImportOutcome, ImportError> {
        let entries = std::fs::read_dir(&job.path)
            .map(|read| {
                read.filter_map(Result::ok)
                    .map(|entry| entry.file_name().to_string_lossy().into_owned())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        self.seen.lock().unwrap().push((job.path.clone(), entries));
        let hold = self.hold.lock().unwrap().take();
        if let Some(hold) = hold {
            hold.wait().await;
            let mut entries = std::fs::read_dir(&job.path)
                .unwrap()
                .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
                .collect::<Vec<_>>();
            entries.sort();
            self.after_hold.lock().unwrap().push(entries);
        }

        self.outcomes
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or(Ok(ImportOutcome { items: Vec::new() }))
    }
}

struct FakeOwnership {
    owned: Mutex<bool>,
}

#[async_trait]
impl LibraryOwnership for FakeOwnership {
    async fn owns(&self, _target: &AcquisitionTarget) -> Result<bool, OwnershipError> {
        Ok(*self.owned.lock().unwrap())
    }
}

fn progress(phase: TransferPhase) -> TransferProgress {
    progress_with(phase, None)
}

fn progress_with(phase: TransferPhase, delivered: Option<&str>) -> TransferProgress {
    TransferProgress {
        phase,
        bytes_done: None,
        bytes_total: None,
        observed_at: Utc::now(),
        delivered: delivered.map(PathBuf::from),
    }
}

struct Harness {
    service: Arc<AcquisitionService>,
    store: Arc<PostgresAcquisitionStore>,
    search: Arc<FakeSearch>,
    transfer: Arc<FakeTransfer>,
    lookup: Arc<FakeLookup>,
    importer: Arc<FakeImporter>,
    ownership: Arc<FakeOwnership>,
    staging: PathBuf,
    ports: AcquisitionPorts,
    library: Arc<LibraryService>,
    config: AcquisitionConfig,
    clock: Clock,
}

impl Harness {
    fn restart(&mut self) {
        self.service = Arc::new(AcquisitionService::new(
            Arc::clone(&self.store),
            self.ports.clone(),
            Arc::clone(&self.library),
            self.config.clone(),
            self.clock.clone(),
        ));
    }

    async fn new(pool: PgPool, staging: PathBuf, clock: Clock) -> Self {
        Self::build(pool, staging, clock, None).await
    }

    async fn build(
        pool: PgPool,
        staging: PathBuf,
        clock: Clock,
        hook: Option<crate::features::acquisition::service::ConflictHook>,
    ) -> Self {
        Self::build_with(pool, staging, clock, hook, None).await
    }

    async fn build_with(
        pool: PgPool,
        staging: PathBuf,
        clock: Clock,
        hook: Option<crate::features::acquisition::service::ConflictHook>,
        before: Option<crate::features::acquisition::service::ConflictHook>,
    ) -> Self {
        Self::build_with_options(pool, staging, clock, hook, before, 1, None).await
    }

    async fn build_with_options(
        pool: PgPool,
        staging: PathBuf,
        clock: Clock,
        hook: Option<crate::features::acquisition::service::ConflictHook>,
        before: Option<crate::features::acquisition::service::ConflictHook>,
        race_width: usize,
        provider: Option<Arc<dyn crate::features::music_library::catalogue::CatalogueProvider>>,
    ) -> Self {
        awa::migrations::run(&pool).await.expect("the awa schema");

        let repository = Arc::new(PostgresTrackRepository::new(pool.clone()));
        let library = Arc::new(LibraryService::new(
            Arc::clone(&repository)
                as Arc<dyn crate::features::music_library::test_support::LibraryRead>,
            Arc::clone(&repository)
                as Arc<dyn crate::features::music_library::test_support::SourceLookup>,
            Arc::clone(&repository)
                as Arc<dyn crate::features::music_library::test_support::StateStore>,
        ));
        let search = Arc::new(FakeSearch {
            responses: Mutex::new(VecDeque::new()),
        });
        let transfer = Arc::new(FakeTransfer::default());
        let lookup = Arc::new(FakeLookup {
            release: Mutex::new(None),
            candidates: Mutex::new(VecDeque::new()),
            recordings: Mutex::new(VecDeque::new()),
        });
        let importer = Arc::new(FakeImporter::default());
        let ownership = Arc::new(FakeOwnership {
            owned: Mutex::new(false),
        });
        let store = Arc::new(PostgresAcquisitionStore::new(pool));
        let search_service = Arc::new(SearchService::new(
            Arc::clone(&repository)
                as Arc<dyn crate::features::music_library::test_support::LibraryRead>,
            Arc::clone(&repository)
                as Arc<dyn crate::features::music_library::test_support::CatalogueWrite>,
            Arc::clone(&repository)
                as Arc<dyn crate::features::music_library::test_support::SourceLookup>,
            Arc::clone(&repository)
                as Arc<dyn crate::features::music_library::test_support::StateStore>,
        ));
        let completion = Arc::new(match provider {
            Some(provider) => CompletionService::new(
                Arc::clone(&library),
                Arc::clone(&repository)
                    as Arc<dyn crate::features::music_library::test_support::CatalogueWrite>,
                Arc::clone(&repository)
                    as Arc<dyn crate::features::music_library::test_support::SourceLookup>,
                vec![provider],
                search_service,
            )
            .with_registry(
                crate::features::music_library::availability::ProviderRegistry::with_enabled([
                    "youtube_music",
                ]),
            ),
            None => CompletionService::disabled(Arc::clone(&library), search_service),
        });
        let ports = AcquisitionPorts::new(
            Arc::clone(&search) as Arc<dyn CandidateSearch>,
            Arc::clone(&transfer) as Arc<dyn RemoteTransfer>,
            Arc::clone(&lookup) as Arc<dyn MusicBrainzLookup>,
            Arc::clone(&importer) as Arc<dyn Importer>,
            Arc::clone(&ownership) as Arc<dyn LibraryOwnership>,
            completion,
        );

        let config = AcquisitionConfig {
            enabled: true,
            staging_dir: Some(staging.clone()),
            library_dir: Some(staging.clone()),
            candidate_budget: 6,
            race_width,
            start_timeout: Duration::from_secs(600),
            transfer_timeout: Duration::from_secs(1800),
            poll: Duration::from_secs(10),
        };
        let service = AcquisitionService::new(
            Arc::clone(&store),
            ports.clone(),
            Arc::clone(&library),
            config.clone(),
            clock.clone(),
        );
        let service = match hook {
            Some(hook) => service.with_conflict_hook(hook),
            None => service,
        };
        let service = match before {
            Some(hook) => service.with_before_insert_hook(hook),
            None => service,
        };
        let service = Arc::new(service);

        Self {
            service,
            store,
            search,
            transfer,
            lookup,
            importer,
            ownership,
            staging,
            ports,
            library,
            config,
            clock,
        }
    }
}

fn release_id() -> MbId {
    MbId::parse("11111111-1111-1111-1111-111111111111").unwrap()
}

fn recording_id() -> MbId {
    MbId::parse("22222222-2222-2222-2222-222222222222").unwrap()
}

fn candidate_file(name: &str) -> CandidateFile {
    CandidateFile {
        input: TransferInput::SoulseekFile {
            peer: "peer-a".to_owned(),
            path: name.to_owned(),
            size_bytes: 1,
        },
        name: name.to_owned(),
        facts: FileFacts::default(),
    }
}

fn candidate(name: &str) -> ReleaseCandidate {
    candidate_from(name, &[name])
}

fn candidate_from(user: &str, names: &[&str]) -> ReleaseCandidate {
    ReleaseCandidate {
        source: format!("{user}:dir"),
        peer: PeerFacts::default(),
        files: names
            .iter()
            .map(|name| {
                let mut file = candidate_file(name);
                let TransferInput::SoulseekFile { peer, .. } = &mut file.input;
                *peer = user.to_owned();
                file
            })
            .collect(),
    }
}

fn candidate_with(names: &[&str]) -> ReleaseCandidate {
    candidate_from(names.first().copied().unwrap_or("empty"), names)
}

fn expected_track() -> Vec<ExpectedTrack> {
    vec![ExpectedTrack {
        disc: 1,
        position: 1,
        title: "One".to_owned(),
        length_ms: Some(200_000),
    }]
}

async fn seeded(
    harness: &Harness,
    target: AcquisitionTarget,
    state: RequestState,
    candidates: Vec<ReleaseCandidate>,
) -> Uuid {
    let id = Uuid::now_v7();
    let origin = AcquisitionOrigin::Release(release_id());

    harness.store.insert_request(id, &origin).await.unwrap();
    harness
        .store
        .set_target(id, &target, "An Artist", "An Album", &expected_track())
        .await
        .unwrap();
    harness.store.set_candidates(id, &candidates).await.unwrap();
    harness.store.set_state(id, state).await.unwrap();

    id
}

async fn seed_attempt(harness: &Harness, id: Uuid, files: Vec<CandidateFile>) {
    let files = files
        .into_iter()
        .map(|file| AttemptFile::new(Uuid::now_v7(), file))
        .collect::<Vec<_>>();

    harness.store.insert_attempt(id, 1, &files).await.unwrap();
    harness.store.set_attempts(id, 1).await.unwrap();
}

async fn state(harness: &Harness, id: Uuid) -> RequestState {
    harness.store.find_request(id).await.unwrap().unwrap().state
}

fn staging_file(harness: &Harness, relative: &str) -> String {
    let path = harness.staging.join(relative);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, b"audio").unwrap();

    relative.to_owned()
}

#[sqlx::test]
async fn a_request_and_its_job_commit_together(pool: PgPool) {
    let staging = std::env::temp_dir().join(format!("whio-aq-{}", Uuid::now_v7()));
    let harness = Harness::new(pool.clone(), staging, Clock::system()).await;

    let outcome = harness
        .service
        .request(AcquisitionOrigin::Release(release_id()))
        .await
        .unwrap();

    assert!(outcome.created);
    let jobs = crate::platform::jobs::phase(
        &pool,
        "whio.acquisition",
        "whio.acquisition.advance.v1",
        outcome.request.id,
    )
    .await
    .unwrap();
    assert!(jobs.is_some(), "the request and its job commit together");
}

#[sqlx::test]
async fn a_second_request_for_a_live_origin_returns_the_first(pool: PgPool) {
    let staging = std::env::temp_dir().join(format!("whio-aq-{}", Uuid::now_v7()));
    let harness = Harness::new(pool, staging, Clock::system()).await;

    let first = harness
        .service
        .request(AcquisitionOrigin::Release(release_id()))
        .await
        .unwrap();
    let second = harness
        .service
        .request(AcquisitionOrigin::Release(release_id()))
        .await
        .unwrap();

    assert!(first.created);
    assert!(!second.created);
    assert_eq!(first.request.id, second.request.id);
}

#[sqlx::test]
async fn an_owned_target_is_already_owned(pool: PgPool) {
    let staging = std::env::temp_dir().join(format!("whio-aq-{}", Uuid::now_v7()));
    let harness = Harness::new(pool, staging, Clock::system()).await;
    *harness.ownership.owned.lock().unwrap() = true;
    *harness.lookup.release.lock().unwrap() = Some(Ok(Some(MbRelease {
        id: release_id(),
        artist: "An Artist".to_owned(),
        title: "An Album".to_owned(),
        tracks: expected_track(),
    })));
    let id = seeded(
        &harness,
        AcquisitionTarget::Release(release_id()),
        RequestState::Pending,
        Vec::new(),
    )
    .await;

    assert_eq!(harness.service.advance(id).await.unwrap(), Step::Done);
    assert_eq!(state(&harness, id).await, RequestState::AlreadyOwned);
}

#[sqlx::test]
async fn zero_track_metadata_and_non_audio_results_are_no_candidate(pool: PgPool) {
    let staging = std::env::temp_dir().join(format!("whio-aq-{}", Uuid::now_v7()));
    let harness = Harness::new(pool, staging, Clock::system()).await;
    *harness.lookup.release.lock().unwrap() = Some(Ok(Some(MbRelease {
        id: release_id(),
        artist: "An Artist".to_owned(),
        title: "An Album".to_owned(),
        tracks: Vec::new(),
    })));
    harness
        .search
        .responses
        .lock()
        .unwrap()
        .push_back(Ok(vec![candidate("cover.jpg")]));
    let request = harness
        .service
        .request(AcquisitionOrigin::Release(release_id()))
        .await
        .unwrap();

    assert_eq!(
        harness.service.advance(request.request.id).await.unwrap(),
        Step::Done
    );
    let request = harness
        .service
        .get(request.request.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(request.state, RequestState::NoCandidate);
    assert_eq!(request.reason.as_deref(), Some("no_candidate"));
    assert!(request.expected_tracks.is_empty());
    assert_eq!(request.attempts, 0);
    assert!(
        harness
            .store
            .load_current_attempt(request.id)
            .await
            .unwrap()
            .is_none()
    );
    assert!(harness.transfer.started_keys.lock().unwrap().is_empty());
}

#[sqlx::test]
async fn an_empty_saved_bundle_never_dispatches_an_attempt(pool: PgPool) {
    let staging = std::env::temp_dir().join(format!("whio-aq-{}", Uuid::now_v7()));
    let harness = Harness::new(pool, staging, Clock::system()).await;
    let id = seeded(
        &harness,
        AcquisitionTarget::Release(release_id()),
        RequestState::Searching,
        vec![candidate_with(&[])],
    )
    .await;

    assert_eq!(harness.service.advance(id).await.unwrap(), Step::Done);
    assert_eq!(state(&harness, id).await, RequestState::NoCandidate);
    assert!(
        harness
            .store
            .load_current_attempt(id)
            .await
            .unwrap()
            .is_none()
    );
    assert!(harness.transfer.started_keys.lock().unwrap().is_empty());
}

#[sqlx::test]
async fn an_empty_persisted_attempt_never_starts_empty_transfers(pool: PgPool) {
    let staging = std::env::temp_dir().join(format!("whio-aq-{}", Uuid::now_v7()));
    let harness = Harness::new(pool, staging, Clock::system()).await;
    let id = seeded(
        &harness,
        AcquisitionTarget::Release(release_id()),
        RequestState::Transferring,
        vec![candidate_with(&[])],
    )
    .await;
    seed_attempt(&harness, id, Vec::new()).await;

    assert_eq!(harness.service.advance(id).await.unwrap(), Step::Done);
    assert_eq!(state(&harness, id).await, RequestState::NoCandidate);
    assert_eq!(
        harness
            .store
            .load_attempt(id, 1)
            .await
            .unwrap()
            .unwrap()
            .state,
        AttemptState::Failed
    );
    assert!(harness.transfer.started_keys.lock().unwrap().is_empty());
}

#[sqlx::test]
async fn no_accepted_candidate_is_no_candidate(pool: PgPool) {
    let staging = std::env::temp_dir().join(format!("whio-aq-{}", Uuid::now_v7()));
    let harness = Harness::new(pool, staging, Clock::system()).await;
    harness
        .search
        .responses
        .lock()
        .unwrap()
        .push_back(Ok(vec![candidate("Solo.jpg")]));
    let id = seeded(
        &harness,
        AcquisitionTarget::Release(release_id()),
        RequestState::Searching,
        Vec::new(),
    )
    .await;

    assert_eq!(harness.service.advance(id).await.unwrap(), Step::Done);
    assert_eq!(state(&harness, id).await, RequestState::NoCandidate);
}

#[sqlx::test]
async fn a_rejected_slot_cancels_the_attempt_and_tries_the_next(pool: PgPool) {
    let staging = std::env::temp_dir().join(format!("whio-aq-{}", Uuid::now_v7()));
    let harness = Harness::new(pool, staging, Clock::system()).await;
    harness.transfer.starts.lock().unwrap().push_back(Ok(vec![
        Slot::Accepted(TransferHandle::new("handle-0".to_owned())),
        Slot::Rejected(Rejection::Refused),
    ]));
    let id = seeded(
        &harness,
        AcquisitionTarget::Release(release_id()),
        RequestState::Transferring,
        vec![
            candidate_with(&["One.flac", "Two.flac"]),
            candidate("Three.flac"),
        ],
    )
    .await;
    seed_attempt(
        &harness,
        id,
        vec![candidate_file("One.flac"), candidate_file("Two.flac")],
    )
    .await;

    let _ = harness.service.advance(id).await.unwrap();

    assert_eq!(
        harness
            .store
            .load_attempt(id, 1)
            .await
            .unwrap()
            .unwrap()
            .state,
        AttemptState::Failed
    );
    assert!(harness.store.load_attempt(id, 2).await.unwrap().is_some());
    assert!(
        harness
            .transfer
            .cancelled
            .lock()
            .unwrap()
            .iter()
            .any(|(handle, remove)| handle == "handle-0" && *remove)
    );
}

#[sqlx::test]
async fn a_transfer_in_progress_waits_for_the_poll_interval(pool: PgPool) {
    let staging = std::env::temp_dir().join(format!("whio-aq-{}", Uuid::now_v7()));
    let harness = Harness::new(pool, staging, Clock::system()).await;
    let id = seeded(
        &harness,
        AcquisitionTarget::Release(release_id()),
        RequestState::Transferring,
        vec![candidate("One.flac")],
    )
    .await;
    seed_attempt(&harness, id, vec![candidate_file("One.flac")]).await;
    harness
        .store
        .update_attempt(
            id,
            1,
            &AttemptUpdate {
                state: AttemptState::Transferring,
                reason: None,
                handles: vec![Some("handle-0".to_owned())],
                delivered: Vec::new(),
                imported: Vec::new(),
            },
        )
        .await
        .unwrap();

    assert_eq!(
        harness.service.advance(id).await.unwrap(),
        Step::Wait(Duration::from_secs(10))
    );
}

#[sqlx::test]
async fn a_failed_transfer_tries_the_next_candidate(pool: PgPool) {
    let staging = std::env::temp_dir().join(format!("whio-aq-{}", Uuid::now_v7()));
    let harness = Harness::new(pool, staging, Clock::system()).await;
    let id = seeded(
        &harness,
        AcquisitionTarget::Release(release_id()),
        RequestState::Transferring,
        vec![candidate("One.flac"), candidate("Two.flac")],
    )
    .await;
    seed_attempt(&harness, id, vec![candidate_file("One.flac")]).await;
    harness
        .store
        .update_attempt(
            id,
            1,
            &AttemptUpdate {
                state: AttemptState::Transferring,
                reason: None,
                handles: vec![Some("handle-0".to_owned())],
                delivered: Vec::new(),
                imported: Vec::new(),
            },
        )
        .await
        .unwrap();
    harness
        .transfer
        .statuses
        .lock()
        .unwrap()
        .push_back(Ok(vec![Slot::Accepted(progress(TransferPhase::Failed))]));

    let _ = harness.service.advance(id).await.unwrap();

    assert_eq!(
        harness
            .store
            .load_attempt(id, 1)
            .await
            .unwrap()
            .unwrap()
            .state,
        AttemptState::Failed
    );
    assert!(harness.store.load_attempt(id, 2).await.unwrap().is_some());
    assert!(
        harness
            .transfer
            .cancelled
            .lock()
            .unwrap()
            .iter()
            .any(|(_, remove)| *remove)
    );
}

#[sqlx::test]
async fn a_transfer_past_its_timeout_is_cancelled(pool: PgPool) {
    let staging = std::env::temp_dir().join(format!("whio-aq-{}", Uuid::now_v7()));
    let future = Utc::now() + chrono::Duration::hours(2);
    let harness = Harness::new(pool, staging, Clock::from_fn(move || future)).await;
    let id = seeded(
        &harness,
        AcquisitionTarget::Release(release_id()),
        RequestState::Transferring,
        vec![candidate("One.flac")],
    )
    .await;
    seed_attempt(&harness, id, vec![candidate_file("One.flac")]).await;
    harness
        .store
        .update_attempt(
            id,
            1,
            &AttemptUpdate {
                state: AttemptState::Transferring,
                reason: None,
                handles: vec![Some("handle-0".to_owned())],
                delivered: Vec::new(),
                imported: Vec::new(),
            },
        )
        .await
        .unwrap();

    let _ = harness.service.advance(id).await.unwrap();

    let attempt = harness.store.load_attempt(id, 1).await.unwrap().unwrap();
    assert_eq!(attempt.state, AttemptState::Failed);
    assert_eq!(attempt.reason.as_deref(), Some("transfer_timeout"));
}

#[sqlx::test]
async fn the_last_failed_attempt_is_exhausted(pool: PgPool) {
    let staging = std::env::temp_dir().join(format!("whio-aq-{}", Uuid::now_v7()));
    let harness = Harness::new(pool, staging, Clock::system()).await;
    let id = seeded(
        &harness,
        AcquisitionTarget::Release(release_id()),
        RequestState::Transferring,
        vec![candidate("One.flac")],
    )
    .await;
    seed_attempt(&harness, id, vec![candidate_file("One.flac")]).await;
    harness
        .transfer
        .statuses
        .lock()
        .unwrap()
        .push_back(Ok(vec![Slot::Accepted(progress(TransferPhase::Failed))]));

    assert_eq!(harness.service.advance(id).await.unwrap(), Step::Done);
    assert_eq!(state(&harness, id).await, RequestState::Exhausted);
}

#[sqlx::test]
async fn a_restarted_start_reuses_its_keys(pool: PgPool) {
    let staging = std::env::temp_dir().join(format!("whio-aq-{}", Uuid::now_v7()));
    let harness = Harness::new(pool, staging, Clock::system()).await;
    let id = seeded(
        &harness,
        AcquisitionTarget::Release(release_id()),
        RequestState::Transferring,
        vec![candidate("One.flac")],
    )
    .await;
    seed_attempt(&harness, id, vec![candidate_file("One.flac")]).await;
    let attempt = harness.store.load_attempt(id, 1).await.unwrap().unwrap();
    let stored = attempt.files[0].key();

    let _ = harness.service.advance(id).await.unwrap();

    assert_eq!(
        harness.transfer.started_keys.lock().unwrap().as_slice(),
        [stored]
    );
}

#[sqlx::test]
async fn a_staging_folder_with_extra_audio_is_not_imported(pool: PgPool) {
    let staging = std::env::temp_dir().join(format!("whio-aq-{}", Uuid::now_v7()));
    let harness = Harness::new(pool, staging.clone(), Clock::system()).await;
    let id = seeded(
        &harness,
        AcquisitionTarget::Release(release_id()),
        RequestState::Importing,
        vec![candidate("Album/One.flac")],
    )
    .await;
    let one = staging_file(&harness, "Album/One.flac");
    staging_file(&harness, "Album/Extra.flac");
    staging_file(&harness, "Other/Three.flac");
    seed_attempt_with_delivered(&harness, id, &[one]).await;
    harness
        .importer
        .outcomes
        .lock()
        .unwrap()
        .push_back(Ok(ImportOutcome {
            items: vec![ImportedItem {
                release_id: Some(release_id().to_string()),
                recording_id: None,
                path: PathBuf::from("/library/Album/One.flac"),
            }],
        }));

    assert_eq!(harness.service.advance(id).await.unwrap(), Step::Done);
    assert_eq!(state(&harness, id).await, RequestState::Completed);

    let seen = harness.importer.seen.lock().unwrap().clone();
    assert_eq!(seen.len(), 1);
    let (input, mut names) = seen[0].clone();
    assert!(
        input.starts_with(staging.join(".whio-import")),
        "the importer reads an attempt-private directory"
    );
    names.sort();
    assert_eq!(names, vec!["One.flac".to_owned()]);
    assert!(
        staging.join("Album/Extra.flac").exists(),
        "another permitted download is not consumed by this import"
    );
    assert!(staging.join("Other/Three.flac").exists());
}

async fn seed_attempt_with_delivered(harness: &Harness, id: Uuid, delivered: &[String]) {
    let files = delivered
        .iter()
        .map(|relative| {
            AttemptFile::new(
                Uuid::now_v7(),
                candidate_file(relative.rsplit('/').next().unwrap()),
            )
        })
        .collect::<Vec<_>>();
    harness.store.insert_attempt(id, 1, &files).await.unwrap();
    let update = AttemptUpdate {
        state: AttemptState::Importing,
        reason: None,
        handles: vec![Some("handle-0".to_owned())],
        delivered: delivered.iter().map(PathBuf::from).collect(),
        imported: Vec::new(),
    };
    assert!(
        harness
            .store
            .claim_importing(id, 1, &update, delivered)
            .await
            .unwrap(),
        "the seeded attempt owns its delivered paths"
    );
}

#[sqlx::test]
async fn a_skipped_import_tries_the_next_candidate(pool: PgPool) {
    let staging = std::env::temp_dir().join(format!("whio-aq-{}", Uuid::now_v7()));
    let harness = Harness::new(pool, staging, Clock::system()).await;
    let id = seeded(
        &harness,
        AcquisitionTarget::Release(release_id()),
        RequestState::Importing,
        vec![candidate("Album/One.flac"), candidate("Album/Two.flac")],
    )
    .await;
    let one = staging_file(&harness, "Album/One.flac");
    seed_attempt_with_delivered(&harness, id, &[one]).await;
    harness
        .importer
        .outcomes
        .lock()
        .unwrap()
        .push_back(Ok(ImportOutcome { items: Vec::new() }));

    let _ = harness.service.advance(id).await.unwrap();

    let attempt = harness.store.load_attempt(id, 1).await.unwrap().unwrap();
    assert_eq!(attempt.reason.as_deref(), Some("import_skipped"));
    assert!(harness.store.load_attempt(id, 2).await.unwrap().is_some());
}

#[sqlx::test]
async fn a_complete_import_completes_and_cleans_staging(pool: PgPool) {
    let staging = std::env::temp_dir().join(format!("whio-aq-{}", Uuid::now_v7()));
    let harness = Harness::new(pool, staging.clone(), Clock::system()).await;
    let id = seeded(
        &harness,
        AcquisitionTarget::Release(release_id()),
        RequestState::Importing,
        vec![candidate("Album/One.flac")],
    )
    .await;
    let one = staging_file(&harness, "Album/One.flac");
    seed_attempt_with_delivered(&harness, id, &[one]).await;
    harness
        .importer
        .outcomes
        .lock()
        .unwrap()
        .push_back(Ok(ImportOutcome {
            items: vec![ImportedItem {
                release_id: Some(release_id().to_string()),
                recording_id: None,
                path: PathBuf::from("/library/Album/One.flac"),
            }],
        }));

    assert_eq!(harness.service.advance(id).await.unwrap(), Step::Done);
    assert_eq!(state(&harness, id).await, RequestState::Completed);
    assert!(!staging.join("Album/One.flac").exists());
    assert!(!staging.join("Album").exists());
}

#[sqlx::test]
async fn a_wrong_item_count_is_import_mismatch(pool: PgPool) {
    let staging = std::env::temp_dir().join(format!("whio-aq-{}", Uuid::now_v7()));
    let harness = Harness::new(pool, staging, Clock::system()).await;
    let id = seeded(
        &harness,
        AcquisitionTarget::Release(release_id()),
        RequestState::Importing,
        vec![candidate("Album/One.flac")],
    )
    .await;
    let one = staging_file(&harness, "Album/One.flac");
    seed_attempt_with_delivered(&harness, id, &[one]).await;
    harness
        .importer
        .outcomes
        .lock()
        .unwrap()
        .push_back(Ok(ImportOutcome {
            items: vec![
                ImportedItem {
                    release_id: Some(release_id().to_string()),
                    recording_id: None,
                    path: PathBuf::from("/library/Album/One.flac"),
                },
                ImportedItem {
                    release_id: Some(release_id().to_string()),
                    recording_id: None,
                    path: PathBuf::from("/library/Album/Two.flac"),
                },
            ],
        }));

    assert_eq!(harness.service.advance(id).await.unwrap(), Step::Done);
    assert_eq!(state(&harness, id).await, RequestState::ImportMismatch);
}

#[sqlx::test]
async fn an_import_rerun_after_ownership_completes(pool: PgPool) {
    let staging = std::env::temp_dir().join(format!("whio-aq-{}", Uuid::now_v7()));
    let harness = Harness::new(pool, staging, Clock::system()).await;
    *harness.ownership.owned.lock().unwrap() = true;
    let id = seeded(
        &harness,
        AcquisitionTarget::Release(release_id()),
        RequestState::Importing,
        vec![candidate("Album/One.flac")],
    )
    .await;
    seed_attempt_with_delivered(&harness, id, &["Album/One.flac".to_owned()]).await;

    assert_eq!(harness.service.advance(id).await.unwrap(), Step::Done);
    assert_eq!(state(&harness, id).await, RequestState::Completed);
    assert!(harness.importer.outcomes.lock().unwrap().is_empty());
}

#[sqlx::test]
async fn a_backend_outage_is_retryable(pool: PgPool) {
    let staging = std::env::temp_dir().join(format!("whio-aq-{}", Uuid::now_v7()));
    let harness = Harness::new(pool, staging, Clock::system()).await;
    harness
        .search
        .responses
        .lock()
        .unwrap()
        .push_back(Err(TransferError::Unavailable));
    let id = seeded(
        &harness,
        AcquisitionTarget::Release(release_id()),
        RequestState::Searching,
        Vec::new(),
    )
    .await;

    assert!(matches!(
        harness.service.advance(id).await.unwrap_err(),
        AcquisitionError::Unavailable
    ));
}

#[sqlx::test]
async fn an_mb_outage_is_retryable(pool: PgPool) {
    let staging = std::env::temp_dir().join(format!("whio-aq-{}", Uuid::now_v7()));
    let harness = Harness::new(pool, staging, Clock::system()).await;
    *harness.lookup.release.lock().unwrap() = Some(Err(LookupError::Unavailable));
    let id = seeded(
        &harness,
        AcquisitionTarget::Release(release_id()),
        RequestState::Pending,
        Vec::new(),
    )
    .await;

    assert!(matches!(
        harness.service.advance(id).await.unwrap_err(),
        AcquisitionError::Unavailable
    ));
}

#[sqlx::test]
async fn a_recording_import_uses_the_file_path(pool: PgPool) {
    let staging = std::env::temp_dir().join(format!("whio-aq-{}", Uuid::now_v7()));
    let harness = Harness::new(pool, staging, Clock::system()).await;
    let id = seeded(
        &harness,
        AcquisitionTarget::Recording(recording_id()),
        RequestState::Importing,
        vec![candidate("Solo.flac")],
    )
    .await;
    let solo = staging_file(&harness, "Solo.flac");
    seed_attempt_with_delivered(&harness, id, &[solo]).await;
    harness
        .importer
        .outcomes
        .lock()
        .unwrap()
        .push_back(Ok(ImportOutcome {
            items: vec![ImportedItem {
                release_id: None,
                recording_id: Some(recording_id().to_string()),
                path: PathBuf::from("/library/Solo.flac"),
            }],
        }));

    assert_eq!(harness.service.advance(id).await.unwrap(), Step::Done);
    assert_eq!(state(&harness, id).await, RequestState::Completed);
}

#[sqlx::test]
async fn simultaneous_first_requests_create_one_row(pool: PgPool) {
    let staging = std::env::temp_dir().join(format!("whio-aq-{}", Uuid::now_v7()));
    let (before, mut arrivals) = Hold::new();
    let harness = Harness::build_with(
        pool.clone(),
        staging,
        Clock::system(),
        None,
        Some(before.hook()),
    )
    .await;
    let first = tokio::spawn({
        let service = Arc::clone(&harness.service);
        async move {
            service
                .request(AcquisitionOrigin::Release(release_id()))
                .await
        }
    });
    let second = tokio::spawn({
        let service = Arc::clone(&harness.service);
        async move {
            service
                .request(AcquisitionOrigin::Release(release_id()))
                .await
        }
    });
    entered(&mut arrivals).await;
    entered(&mut arrivals).await;
    assert!(
        !first.is_finished() && !second.is_finished(),
        "both initial reads finished before either insert"
    );
    before.release(2);
    let first = joined(first).await.unwrap();
    let second = joined(second).await.unwrap();
    let (winner, loser) = if first.created {
        (&first, &second)
    } else {
        (&second, &first)
    };
    assert!(winner.created);
    assert!(!loser.created);
    assert_eq!(winner.request.id, loser.request.id);

    assert_eq!(first.request.id, second.request.id);
    assert_ne!(first.created, second.created);

    let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM acquisition_requests")
        .fetch_one(&pool)
        .await
        .unwrap();
    let jobs: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM awa.jobs WHERE kind = 'whio.acquisition.advance.v1'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(rows, 1, "one live request per origin");
    assert_eq!(jobs, 1, "only the winning insert enqueues a job");
}

#[sqlx::test]
async fn a_missing_release_is_released_for_a_new_request(pool: PgPool) {
    let staging = std::env::temp_dir().join(format!("whio-aq-{}", Uuid::now_v7()));
    let harness = Harness::new(pool, staging, Clock::system()).await;
    let id = seeded(
        &harness,
        AcquisitionTarget::Release(release_id()),
        RequestState::Pending,
        Vec::new(),
    )
    .await;

    assert_eq!(harness.service.advance(id).await.unwrap(), Step::Done);
    assert_eq!(state(&harness, id).await, RequestState::ReleaseNotFound);
    let request = harness.store.find_request(id).await.unwrap().unwrap();
    assert_eq!(request.reason.as_deref(), Some("release_not_found"));

    let again = harness
        .service
        .request(AcquisitionOrigin::Release(release_id()))
        .await
        .unwrap();
    assert!(again.created, "a terminal request does not block a new one");
    assert_ne!(again.request.id, id);
}

#[sqlx::test]
async fn a_completed_transfer_after_its_timeout_is_still_accepted(pool: PgPool) {
    let staging = std::env::temp_dir().join(format!("whio-aq-{}", Uuid::now_v7()));
    let future = Utc::now() + chrono::Duration::hours(2);
    let harness = Harness::new(pool, staging, Clock::from_fn(move || future)).await;
    let id = seeded(
        &harness,
        AcquisitionTarget::Release(release_id()),
        RequestState::Transferring,
        vec![candidate("Album/One.flac")],
    )
    .await;
    seed_attempt(&harness, id, vec![candidate_file("One.flac")]).await;
    harness
        .store
        .update_attempt(
            id,
            1,
            &AttemptUpdate {
                state: AttemptState::Transferring,
                reason: None,
                handles: vec![Some("handle-0".to_owned())],
                delivered: Vec::new(),
                imported: Vec::new(),
            },
        )
        .await
        .unwrap();
    staging_file(&harness, "Album/One.flac");
    harness
        .transfer
        .statuses
        .lock()
        .unwrap()
        .push_back(Ok(vec![Slot::Accepted(progress_with(
            TransferPhase::ReportedComplete,
            Some("Album/One.flac"),
        ))]));

    let _ = harness.service.advance(id).await.unwrap();

    assert!(
        harness.transfer.cancelled.lock().unwrap().is_empty(),
        "a completed transfer is not cancelled by the timeout"
    );
    let attempt = harness.store.load_attempt(id, 1).await.unwrap().unwrap();
    assert_ne!(attempt.reason.as_deref(), Some("transfer_timeout"));
}

#[sqlx::test]
async fn an_unavailable_import_retains_its_staged_input(pool: PgPool) {
    let staging = std::env::temp_dir().join(format!("whio-aq-{}", Uuid::now_v7()));
    let harness = Harness::new(pool, staging, Clock::system()).await;
    let id = seeded(
        &harness,
        AcquisitionTarget::Release(release_id()),
        RequestState::Importing,
        vec![candidate("Album/One.flac")],
    )
    .await;
    let one = staging_file(&harness, "Album/One.flac");
    seed_attempt_with_delivered(&harness, id, std::slice::from_ref(&one)).await;
    harness
        .importer
        .outcomes
        .lock()
        .unwrap()
        .push_back(Err(ImportError::Unavailable));

    assert!(matches!(
        harness.service.advance(id).await.unwrap_err(),
        AcquisitionError::Unavailable
    ));
    assert!(
        harness.staging.join(&one).exists(),
        "an unavailable importer keeps the staged input"
    );
    assert_eq!(
        harness.store.claimed_paths(id, 1).await.unwrap(),
        vec!["Album/One.flac".to_owned()],
        "an interruption before the outcome keeps the attempt's claims"
    );
    let private = harness.importer.seen.lock().unwrap()[0].0.join("One.flac");
    assert_eq!(std::fs::read(harness.staging.join(&one)).unwrap(), b"audio");
    assert_eq!(std::fs::read(&private).unwrap(), b"audio");
    harness
        .importer
        .outcomes
        .lock()
        .unwrap()
        .push_back(Err(ImportError::Unavailable));
    assert!(matches!(
        harness.service.advance(id).await.unwrap_err(),
        AcquisitionError::Unavailable
    ));
    assert_eq!(std::fs::read(harness.staging.join(&one)).unwrap(), b"audio");
    assert_eq!(std::fs::read(&private).unwrap(), b"audio");
    assert_eq!(
        harness.importer.seen.lock().unwrap()[1].0,
        private.parent().unwrap()
    );

    harness
        .importer
        .outcomes
        .lock()
        .unwrap()
        .push_back(Ok(ImportOutcome {
            items: vec![ImportedItem {
                release_id: Some(release_id().to_string()),
                recording_id: None,
                path: PathBuf::from("/library/Album/One.flac"),
            }],
        }));
    assert_eq!(harness.service.advance(id).await.unwrap(), Step::Done);
    assert_eq!(state(&harness, id).await, RequestState::Completed);
}

#[sqlx::test]
async fn a_symlinked_staging_directory_is_a_conflict(pool: PgPool) {
    let staging = std::env::temp_dir().join(format!("whio-aq-{}", Uuid::now_v7()));
    let harness = Harness::new(pool, staging.clone(), Clock::system()).await;
    let outside = std::env::temp_dir().join(format!("whio-aq-out-{}", Uuid::now_v7()));
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(outside.join("secret.flac"), b"secret").unwrap();
    std::fs::create_dir_all(&staging).unwrap();
    std::os::unix::fs::symlink(&outside, staging.join("link")).unwrap();
    let id = seeded(
        &harness,
        AcquisitionTarget::Release(release_id()),
        RequestState::Importing,
        vec![candidate("link/secret.flac")],
    )
    .await;
    seed_attempt_with_delivered(&harness, id, &["link/secret.flac".to_owned()]).await;

    let _ = harness.service.advance(id).await.unwrap();

    assert_eq!(
        harness
            .store
            .load_attempt(id, 1)
            .await
            .unwrap()
            .unwrap()
            .reason
            .as_deref(),
        Some("staging_conflict")
    );
    assert!(
        outside.join("secret.flac").exists(),
        "outside files survive"
    );
    let _ = std::fs::remove_dir_all(&outside);
}

#[sqlx::test]
async fn a_symlinked_staging_file_is_a_conflict(pool: PgPool) {
    let staging = std::env::temp_dir().join(format!("whio-aq-{}", Uuid::now_v7()));
    let harness = Harness::new(pool, staging.clone(), Clock::system()).await;
    let outside = std::env::temp_dir().join(format!("whio-aq-out-{}", Uuid::now_v7()));
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(outside.join("Solo.flac"), b"secret").unwrap();
    std::fs::create_dir_all(&staging).unwrap();
    std::os::unix::fs::symlink(outside.join("Solo.flac"), staging.join("Solo.flac")).unwrap();
    let id = seeded(
        &harness,
        AcquisitionTarget::Recording(recording_id()),
        RequestState::Importing,
        vec![candidate("Solo.flac")],
    )
    .await;
    seed_attempt_with_delivered(&harness, id, &["Solo.flac".to_owned()]).await;

    let _ = harness.service.advance(id).await.unwrap();

    assert_eq!(
        harness
            .store
            .load_attempt(id, 1)
            .await
            .unwrap()
            .unwrap()
            .reason
            .as_deref(),
        Some("staging_conflict")
    );
    assert!(outside.join("Solo.flac").exists());
    let _ = std::fs::remove_dir_all(&outside);
}

#[sqlx::test]
async fn a_traversal_delivered_path_is_a_conflict(pool: PgPool) {
    let staging = std::env::temp_dir().join(format!("whio-aq-{}", Uuid::now_v7()));
    let harness = Harness::new(pool, staging.clone(), Clock::system()).await;
    std::fs::create_dir_all(staging.parent().unwrap()).unwrap();
    let escape = staging.parent().unwrap().join("escape.flac");
    std::fs::write(&escape, b"secret").unwrap();
    let id = seeded(
        &harness,
        AcquisitionTarget::Recording(recording_id()),
        RequestState::Importing,
        vec![candidate("../escape.flac")],
    )
    .await;
    seed_attempt_with_delivered(&harness, id, &["../escape.flac".to_owned()]).await;

    let _ = harness.service.advance(id).await.unwrap();

    assert_eq!(
        harness
            .store
            .load_attempt(id, 1)
            .await
            .unwrap()
            .unwrap()
            .reason
            .as_deref(),
        Some("staging_conflict")
    );
    assert!(escape.exists(), "a traversal target is never removed");
    let _ = std::fs::remove_file(&escape);
}

#[sqlx::test]
async fn a_root_level_recording_cleanup_keeps_the_staging_root(pool: PgPool) {
    let staging = std::env::temp_dir().join(format!("whio-aq-{}", Uuid::now_v7()));
    let harness = Harness::new(pool, staging.clone(), Clock::system()).await;
    std::fs::create_dir_all(&staging).unwrap();
    let id = seeded(
        &harness,
        AcquisitionTarget::Recording(recording_id()),
        RequestState::Importing,
        vec![candidate("Solo.flac")],
    )
    .await;
    std::fs::write(staging.join("Solo.flac"), b"audio").unwrap();
    seed_attempt_with_delivered(&harness, id, &["Solo.flac".to_owned()]).await;
    harness
        .importer
        .outcomes
        .lock()
        .unwrap()
        .push_back(Ok(ImportOutcome {
            items: vec![ImportedItem {
                release_id: None,
                recording_id: Some(recording_id().to_string()),
                path: PathBuf::from("/library/Solo.flac"),
            }],
        }));

    assert_eq!(harness.service.advance(id).await.unwrap(), Step::Done);
    assert_eq!(state(&harness, id).await, RequestState::Completed);
    assert!(!staging.join("Solo.flac").exists());
    assert!(staging.exists(), "the staging root is never removed");
}

#[sqlx::test]
async fn a_path_claimed_by_another_attempt_conflicts(pool: PgPool) {
    let staging = std::env::temp_dir().join(format!("whio-aq-{}", Uuid::now_v7()));
    let harness = Harness::new(pool, staging.clone(), Clock::system()).await;
    let first = seeded(
        &harness,
        AcquisitionTarget::Release(release_id()),
        RequestState::Importing,
        vec![candidate("Album/One.flac")],
    )
    .await;
    std::fs::create_dir_all(staging.join("Album")).unwrap();
    std::fs::write(staging.join("Album/One.flac"), b"audio").unwrap();
    seed_attempt_with_delivered(&harness, first, &["Album/One.flac".to_owned()]).await;

    let second = Uuid::now_v7();
    let other_origin =
        AcquisitionOrigin::Release(MbId::parse("33333333-3333-3333-3333-333333333333").unwrap());
    harness
        .store
        .insert_request(second, &other_origin)
        .await
        .unwrap();
    harness
        .store
        .set_target(
            second,
            &AcquisitionTarget::Release(release_id()),
            "a",
            "t",
            &expected_track(),
        )
        .await
        .unwrap();
    harness
        .store
        .set_candidates(second, &[candidate("Album/One.flac")])
        .await
        .unwrap();
    harness
        .store
        .set_state(second, RequestState::Transferring)
        .await
        .unwrap();
    seed_attempt(&harness, second, vec![candidate_file("One.flac")]).await;
    harness
        .store
        .update_attempt(
            second,
            1,
            &AttemptUpdate {
                state: AttemptState::Transferring,
                reason: None,
                handles: vec![Some("handle-0".to_owned())],
                delivered: Vec::new(),
                imported: Vec::new(),
            },
        )
        .await
        .unwrap();
    harness
        .transfer
        .statuses
        .lock()
        .unwrap()
        .push_back(Ok(vec![Slot::Accepted(progress_with(
            TransferPhase::ReportedComplete,
            Some("Album/One.flac"),
        ))]));

    let _ = harness.service.advance(second).await.unwrap();

    assert_eq!(
        harness
            .store
            .load_attempt(second, 1)
            .await
            .unwrap()
            .unwrap()
            .reason
            .as_deref(),
        Some("staging_conflict")
    );
    assert!(
        staging.join("Album/One.flac").exists(),
        "a path owned by another attempt is not deleted"
    );
    assert_eq!(
        harness.store.claimed_paths(first, 1).await.unwrap(),
        vec!["Album/One.flac".to_owned()]
    );
}

#[sqlx::test]
async fn a_reported_complete_file_that_is_not_visible_yet_waits(pool: PgPool) {
    let staging = std::env::temp_dir().join(format!("whio-aq-{}", Uuid::now_v7()));
    let harness = Harness::new(pool, staging, Clock::system()).await;
    let id = seeded(
        &harness,
        AcquisitionTarget::Release(release_id()),
        RequestState::Transferring,
        vec![candidate("Album/One.flac")],
    )
    .await;
    seed_attempt(&harness, id, vec![candidate_file("One.flac")]).await;
    harness
        .store
        .update_attempt(
            id,
            1,
            &AttemptUpdate {
                state: AttemptState::Transferring,
                reason: None,
                handles: vec![Some("handle-0".to_owned())],
                delivered: Vec::new(),
                imported: Vec::new(),
            },
        )
        .await
        .unwrap();
    harness
        .transfer
        .statuses
        .lock()
        .unwrap()
        .push_back(Ok(vec![Slot::Accepted(progress_with(
            TransferPhase::ReportedComplete,
            Some("Album/One.flac"),
        ))]));

    assert_eq!(
        harness.service.advance(id).await.unwrap(),
        Step::Wait(Duration::from_secs(10))
    );
    assert_eq!(
        harness
            .store
            .load_attempt(id, 1)
            .await
            .unwrap()
            .unwrap()
            .state,
        AttemptState::Transferring
    );
}

#[sqlx::test]
async fn a_cancel_outage_preserves_the_attempt(pool: PgPool) {
    let staging = std::env::temp_dir().join(format!("whio-aq-{}", Uuid::now_v7()));
    let harness = Harness::new(pool, staging, Clock::system()).await;
    let id = seeded(
        &harness,
        AcquisitionTarget::Release(release_id()),
        RequestState::Transferring,
        vec![candidate("Album/One.flac"), candidate("Album/Two.flac")],
    )
    .await;
    seed_attempt(&harness, id, vec![candidate_file("One.flac")]).await;
    harness
        .store
        .update_attempt(
            id,
            1,
            &AttemptUpdate {
                state: AttemptState::Transferring,
                reason: None,
                handles: vec![Some("handle-0".to_owned())],
                delivered: Vec::new(),
                imported: Vec::new(),
            },
        )
        .await
        .unwrap();
    harness
        .transfer
        .statuses
        .lock()
        .unwrap()
        .push_back(Ok(vec![Slot::Accepted(progress(TransferPhase::Failed))]));
    harness
        .transfer
        .cancels
        .lock()
        .unwrap()
        .push_back(Err(TransferError::Unavailable));

    assert!(matches!(
        harness.service.advance(id).await.unwrap_err(),
        AcquisitionError::Unavailable
    ));
    assert_eq!(
        harness
            .store
            .load_attempt(id, 1)
            .await
            .unwrap()
            .unwrap()
            .state,
        AttemptState::Transferring,
        "a cancel outage leaves the attempt resumable"
    );
    assert!(harness.store.load_attempt(id, 2).await.unwrap().is_none());
}

#[sqlx::test]
async fn an_initial_start_outage_resumes_the_same_attempt(pool: PgPool) {
    let staging = std::env::temp_dir().join(format!("whio-aq-{}", Uuid::now_v7()));
    let harness = Harness::new(pool, staging, Clock::system()).await;
    harness
        .search
        .responses
        .lock()
        .unwrap()
        .push_back(Ok(vec![candidate("One.flac")]));
    harness
        .transfer
        .starts
        .lock()
        .unwrap()
        .push_back(Err(TransferError::Unavailable));
    let id = seeded(
        &harness,
        AcquisitionTarget::Release(release_id()),
        RequestState::Searching,
        Vec::new(),
    )
    .await;

    assert!(matches!(
        harness.service.advance(id).await.unwrap_err(),
        AcquisitionError::Unavailable
    ));
    assert_eq!(state(&harness, id).await, RequestState::Transferring);
    let first = harness.transfer.started_keys.lock().unwrap().clone();
    assert_eq!(first.len(), 1);

    let _ = harness.service.advance(id).await.unwrap();

    let second = harness.transfer.started_keys.lock().unwrap().clone();
    assert_eq!(second.len(), 2);
    assert_eq!(second[0], second[1], "the stored keys are reused");
    assert_eq!(state(&harness, id).await, RequestState::Transferring);
    assert_eq!(
        harness
            .store
            .load_attempt(id, 1)
            .await
            .unwrap()
            .unwrap()
            .reason,
        None
    );
}

#[sqlx::test]
async fn an_unavailable_next_start_resumes_before_importing(pool: PgPool) {
    let staging = std::env::temp_dir().join(format!("whio-aq-{}", Uuid::now_v7()));
    let harness = Harness::new(pool, staging.clone(), Clock::system()).await;
    let id = seeded(
        &harness,
        AcquisitionTarget::Release(release_id()),
        RequestState::Importing,
        vec![candidate("Album/One.flac"), candidate("Album/Two.flac")],
    )
    .await;
    std::fs::create_dir_all(staging.join("Album")).unwrap();
    std::fs::write(staging.join("Album/One.flac"), b"audio").unwrap();
    seed_attempt_with_delivered(&harness, id, &["Album/One.flac".to_owned()]).await;
    harness
        .importer
        .outcomes
        .lock()
        .unwrap()
        .push_back(Ok(ImportOutcome { items: Vec::new() }));
    harness
        .transfer
        .starts
        .lock()
        .unwrap()
        .push_back(Err(TransferError::Unavailable));

    assert!(matches!(
        harness.service.advance(id).await.unwrap_err(),
        AcquisitionError::Unavailable
    ));
    assert!(harness.store.load_attempt(id, 2).await.unwrap().is_some());
    assert_eq!(state(&harness, id).await, RequestState::Transferring);

    let _ = harness.service.advance(id).await.unwrap();

    assert_eq!(
        harness
            .store
            .load_attempt(id, 2)
            .await
            .unwrap()
            .unwrap()
            .reason,
        None,
        "attempt 2 starts rather than reporting a staging conflict"
    );
}

#[sqlx::test]
async fn a_long_mismatch_reason_is_persisted(pool: PgPool) {
    let staging = std::env::temp_dir().join(format!("whio-aq-{}", Uuid::now_v7()));
    let harness = Harness::new(pool, staging.clone(), Clock::system()).await;
    let id = seeded(
        &harness,
        AcquisitionTarget::Release(release_id()),
        RequestState::Importing,
        vec![candidate("Album/One.flac")],
    )
    .await;
    std::fs::create_dir_all(staging.join("Album")).unwrap();
    std::fs::write(staging.join("Album/One.flac"), b"audio").unwrap();
    seed_attempt_with_delivered(&harness, id, &["Album/One.flac".to_owned()]).await;
    let items = (0..80)
        .map(|index| ImportedItem {
            release_id: Some("99999999-9999-9999-9999-999999999999".to_owned()),
            recording_id: None,
            path: PathBuf::from(format!("/library/{index}-{}.flac", "x".repeat(40))),
        })
        .collect::<Vec<_>>();
    harness
        .importer
        .outcomes
        .lock()
        .unwrap()
        .push_back(Ok(ImportOutcome { items }));

    assert_eq!(harness.service.advance(id).await.unwrap(), Step::Done);

    let request = harness.store.find_request(id).await.unwrap().unwrap();
    assert_eq!(request.state, RequestState::ImportMismatch);
    assert!(
        request.reason.as_deref().unwrap_or_default().len() > 2000,
        "the complete mismatch reason is persisted"
    );
}

#[test]
fn only_unavailable_provider_errors_are_retryable() {
    use crate::features::acquisition::service::{map_lookup, map_ownership, map_transfer};

    for error in [
        TransferError::InvalidRequest,
        TransferError::Unsupported,
        TransferError::Malformed,
        TransferError::CredentialsRejected,
        TransferError::Internal,
    ] {
        assert!(
            matches!(map_transfer(error), AcquisitionError::Failed),
            "{error:?}"
        );
    }
    assert!(matches!(
        map_transfer(TransferError::Unavailable),
        AcquisitionError::Unavailable
    ));

    assert!(matches!(
        map_lookup(LookupError::Malformed),
        AcquisitionError::Failed
    ));
    assert!(matches!(
        map_lookup(LookupError::Unavailable),
        AcquisitionError::Unavailable
    ));

    assert!(matches!(
        map_ownership(OwnershipError::Failed),
        AcquisitionError::Failed
    ));
    assert!(matches!(
        map_ownership(OwnershipError::Unavailable),
        AcquisitionError::Unavailable
    ));
}

struct AlbumFixtureProvider {
    fetched: crate::features::music_library::catalogue::FetchedAlbum,
    calls: Arc<std::sync::atomic::AtomicUsize>,
}

#[async_trait]
impl crate::features::music_library::catalogue::CatalogueProvider for AlbumFixtureProvider {
    async fn search_tracks(
        &self,
        _search: &crate::features::music_library::catalogue::CatalogueSearch,
        _context: &crate::platform::request::RequestContext,
    ) -> Result<
        Vec<crate::features::music_library::catalogue::CatalogueCandidate>,
        crate::features::music_library::catalogue::ProviderError,
    > {
        Err(crate::features::music_library::catalogue::ProviderError::UnsupportedProvider)
    }

    async fn fetch_album(
        &self,
        _source: &crate::features::music_library::domain::SourceIdentity,
        _context: &crate::platform::request::RequestContext,
    ) -> Result<
        crate::features::music_library::catalogue::FetchedAlbum,
        crate::features::music_library::catalogue::ProviderError,
    > {
        self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(self.fetched.clone())
    }
}

fn album_fixture_source(video: &str) -> crate::features::music_library::domain::SourceIdentity {
    use crate::features::music_library::domain::{ProviderId, SourceIdentity, SourceScope};
    SourceIdentity::new(
        ProviderId::new("youtube_music".to_owned()).unwrap(),
        SourceScope::Global,
        video.to_owned(),
    )
    .unwrap()
}

fn pylon_candidates() -> Vec<crate::features::music_library::catalogue::CatalogueCandidate> {
    use crate::features::music_library::catalogue::CatalogueCandidate;
    use crate::features::music_library::domain::{AlbumMetadata, ArtistCredit, TrackMetadata};
    let album = AlbumMetadata::new("Pylon".to_owned())
        .unwrap()
        .with_album_artist(ArtistCredit::new("An Artist".to_owned(), None).unwrap())
        .with_provider(Some(album_fixture_source("MPREb_pylon")));
    (0..14)
        .map(|index| {
            let title = match index {
                0 => "Spark".to_owned(),
                1 => "Satellite".to_owned(),
                6 => "Switchblade".to_owned(),
                _ => format!("Song {index}"),
            };
            CatalogueCandidate::new(
                album_fixture_source(&format!("video-{index}")),
                TrackMetadata::from_names(
                    title,
                    vec!["An Artist".to_owned()],
                    Some(200_000 + index * 10_000),
                )
                .unwrap(),
            )
            .with_album(Some(album.clone()))
            .with_track_numbers(None, Some(u16::try_from(index + 1).unwrap()))
            .unwrap()
        })
        .collect()
}

async fn seed_legacy_pylon(pool: &PgPool) -> crate::features::music_library::domain::AlbumId {
    use crate::features::music_library::domain::AlbumTrack;
    use crate::features::music_library::test_support::CatalogueWrite;
    let repository = PostgresTrackRepository::new(pool.clone());
    let songs = pylon_candidates();
    let album = repository
        .resolve_or_create_album(songs[0].album().unwrap())
        .await
        .unwrap();
    repository
        .set_album_source(album.id(), &album_fixture_source("MPREb_pylon"))
        .await
        .unwrap();
    for (index, song) in songs.iter().enumerate().filter(|(index, _)| *index != 6) {
        let track = repository
            .get_or_create(song.source().clone(), song.metadata().clone())
            .await
            .unwrap();
        repository
            .add_album_track(album.id(), track.id(), &AlbumTrack::unplaced())
            .await
            .unwrap();
        if index < 2 {
            let duplicate = repository
                .get_or_create(
                    album_fixture_source(&format!("search-{index}")),
                    song.metadata().clone(),
                )
                .await
                .unwrap();
            repository
                .add_album_track(album.id(), duplicate.id(), &AlbumTrack::unplaced())
                .await
                .unwrap();
        }
    }
    repository
        .set_provider_entity_state(
            &album_fixture_source("MPREb_pylon"),
            &crate::features::music_library::domain::ProviderEntityState::new(
                Utc::now(),
                Some(14),
                None,
            )
            .unwrap(),
        )
        .await
        .unwrap();
    album.id().clone()
}

fn pylon_release() -> MbRelease {
    MbRelease {
        id: release_id(),
        artist: "An Artist".to_owned(),
        title: "Pylon".to_owned(),
        tracks: pylon_candidates()
            .iter()
            .enumerate()
            .map(|(index, song)| ExpectedTrack {
                disc: 1,
                position: u16::try_from(index + 1).unwrap(),
                title: song.title().to_owned(),
                length_ms: song
                    .duration_ms()
                    .and_then(|length| u32::try_from(length).ok()),
            })
            .collect(),
    }
}

#[sqlx::test]
async fn a_pending_track_origin_is_protected_during_repair_then_pins_normally(pool: PgPool) {
    use crate::features::music_library::domain::{
        AlbumMetadata, AlbumTrack, SkipReason, TrackMetadata,
    };
    use crate::features::music_library::test_support::{CatalogueWrite, LibraryRead, SourceLookup};
    let repository = PostgresTrackRepository::new(pool.clone());
    let metadata = TrackMetadata::from_names(
        "One".to_owned(),
        vec!["An Artist".to_owned()],
        Some(200_000),
    )
    .unwrap();
    let kept = repository
        .get_or_create(album_fixture_source("kept"), metadata.clone())
        .await
        .unwrap();
    let removed = repository
        .get_or_create(album_fixture_source("removed"), metadata)
        .await
        .unwrap();
    let album = repository
        .resolve_or_create_album(&AlbumMetadata::new("Album".to_owned()).unwrap())
        .await
        .unwrap();
    for track in [&kept, &removed] {
        repository
            .add_album_track(album.id(), track.id(), &AlbumTrack::unplaced())
            .await
            .unwrap();
    }
    let harness = Harness::new(
        pool.clone(),
        std::env::temp_dir().join(Uuid::now_v7().to_string()),
        Clock::system(),
    )
    .await;
    let request = harness
        .service
        .request(AcquisitionOrigin::Track(removed.id().clone()))
        .await
        .unwrap();
    let protected = harness.service.live_track_origins().await.unwrap();
    assert_eq!(protected, [removed.id().clone()]);
    let maintenance = crate::features::music_library::maintenance(pool.clone());
    let request_before = harness
        .store
        .find_request(request.request.id)
        .await
        .unwrap()
        .unwrap();
    let preview = maintenance.provider_duplicates(&protected).await.unwrap();
    assert!(preview.track_merges.is_empty());
    assert_eq!(preview.skipped.len(), 1);
    assert_eq!(preview.skipped[0].id, *removed.id().as_uuid());
    assert_eq!(preview.skipped[0].reason, SkipReason::AcquisitionOrigin);
    assert_eq!(
        repository
            .find_album_entries(album.id(), None)
            .await
            .unwrap()
            .len(),
        2
    );
    assert_eq!(
        maintenance
            .delete_provider_duplicates(&protected)
            .await
            .unwrap(),
        preview
    );
    assert_eq!(
        repository
            .find_album_entries(album.id(), None)
            .await
            .unwrap()
            .len(),
        2
    );
    assert_eq!(
        repository.find_sources(removed.id()).await.unwrap().len(),
        1
    );
    let still_pending = harness
        .store
        .find_request(request.request.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(still_pending.id, request_before.id);
    assert_eq!(still_pending.origin, request_before.origin);
    assert_eq!(still_pending.state, RequestState::Pending);
    assert!(still_pending.target.is_none());
    harness
        .lookup
        .recordings
        .lock()
        .unwrap()
        .push_back(Ok(vec![MbRecording {
            id: recording_id(),
            artist: "An Artist".to_owned(),
            title: "One".to_owned(),
            length_ms: Some(200_000),
        }]));
    *harness.ownership.owned.lock().unwrap() = true;
    assert_eq!(
        harness.service.advance(request.request.id).await.unwrap(),
        Step::Done
    );
    let pinned = harness
        .store
        .find_request(request.request.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(pinned.id, request_before.id);
    assert_eq!(pinned.origin, request_before.origin);
    assert_eq!(pinned.state, RequestState::AlreadyOwned);
    assert_eq!(
        pinned.target,
        Some(AcquisitionTarget::Recording(recording_id()))
    );
    let unprotected = harness.service.live_track_origins().await.unwrap();
    assert!(unprotected.is_empty());
    assert_eq!(
        maintenance
            .delete_provider_duplicates(&unprotected)
            .await
            .unwrap()
            .track_merges
            .len(),
        1
    );
}

#[sqlx::test]
async fn pylon_feature_credits_and_versions_agree_for_library_album_repair_and_pin(pool: PgPool) {
    use crate::features::music_library::domain::{
        AlbumMetadata, AlbumTrack, ProviderId, SourceIdentity, SourceScope, TrackMetadata,
    };
    use crate::features::music_library::test_support::{
        CatalogueWrite, LibraryRead, Retention, SourceLookup,
    };
    let repository = PostgresTrackRepository::new(pool.clone());
    let metadata = |title: &str, artists: &[&str], duration| {
        TrackMetadata::from_names(
            title.to_owned(),
            artists.iter().map(|artist| (*artist).to_owned()).collect(),
            Some(duration),
        )
        .unwrap()
    };
    let cases = [
        (
            "Nothing to Prove",
            "Nothing To Prove",
            "Hayley Williams",
            144_316,
            145_000,
        ),
        (
            "Powerlines",
            "Powerlines",
            "Brendan Yates",
            178_391,
            179_000,
        ),
    ];
    let mut albums = Vec::new();
    for (title, provider_title, guest, local_ms, provider_ms) in cases {
        let tagged = format!("{title} feat. {guest}");
        let album = repository
            .resolve_or_create_album(&AlbumMetadata::new(format!("{title} Album")).unwrap())
            .await
            .unwrap();
        let kept = repository
            .get_or_create(
                album_fixture_source(&format!("{title}-kept")),
                metadata(&tagged, &["beabadoobee"], local_ms),
            )
            .await
            .unwrap();
        repository
            .add_album_track(album.id(), kept.id(), &AlbumTrack::unplaced())
            .await
            .unwrap();
        let attached = repository
            .get_or_create_provider(
                album_fixture_source(&format!("{title}-attached")),
                metadata(provider_title, &["beabadoobee", guest], provider_ms),
                Some(album.id()),
            )
            .await
            .unwrap();
        assert_eq!(attached.id(), kept.id());
        let mut versions = Vec::new();
        for version in ["Remix", "Live"] {
            let track = repository
                .get_or_create_provider(
                    album_fixture_source(&format!("{title}-{version}")),
                    metadata(
                        &format!("{provider_title} ({version})"),
                        &["beabadoobee", guest],
                        provider_ms,
                    ),
                    Some(album.id()),
                )
                .await
                .unwrap();
            assert_ne!(track.id(), kept.id());
            repository
                .add_album_track(album.id(), track.id(), &AlbumTrack::unplaced())
                .await
                .unwrap();
            versions.push(track);
        }
        let legacy = repository
            .get_or_create(
                album_fixture_source(&format!("{title}-legacy")),
                metadata(provider_title, &["beabadoobee", guest], provider_ms),
            )
            .await
            .unwrap();
        repository
            .add_album_track(album.id(), legacy.id(), &AlbumTrack::unplaced())
            .await
            .unwrap();
        let entries = repository
            .find_album_entries(album.id(), None)
            .await
            .unwrap();
        assert_eq!(entries.len(), 4);
        let collapsed = super::pin::collapse_album_entries(&entries);
        assert_eq!(
            collapsed
                .iter()
                .map(|entry| entry.track().id())
                .collect::<Vec<_>>(),
            [&kept, &versions[0], &versions[1]].map(|track| track.id())
        );
        albums.push((album, kept, legacy, versions));
    }
    let preview = repository.provider_duplicates(&[]).await.unwrap();
    assert_eq!(preview.track_merges.len(), 2);
    for (album, kept, legacy, _) in &albums {
        assert!(
            preview
                .track_merges
                .iter()
                .any(|merge| merge.id == *legacy.id().as_uuid()
                    && merge.into == *kept.id().as_uuid())
        );
        assert_eq!(
            repository
                .find_album_entries(album.id(), None)
                .await
                .unwrap()
                .len(),
            4
        );
        assert_eq!(repository.find_sources(kept.id()).await.unwrap().len(), 2);
    }
    assert_eq!(
        repository.delete_provider_duplicates(&[]).await.unwrap(),
        preview
    );
    for (album, kept, _, versions) in &albums {
        let entries = repository
            .find_album_entries(album.id(), None)
            .await
            .unwrap();
        assert_eq!(
            entries
                .iter()
                .map(|entry| entry.track().id())
                .collect::<Vec<_>>(),
            [kept, &versions[0], &versions[1]].map(|track| track.id())
        );
        assert_eq!(repository.find_sources(kept.id()).await.unwrap().len(), 3);
    }
    for (title, provider_title, guest, local_ms, provider_ms) in cases {
        let local_source = SourceIdentity::new(
            ProviderId::new("local".to_owned()).unwrap(),
            SourceScope::Named("/music".to_owned()),
            format!("{title}.flac"),
        )
        .unwrap();
        let local = repository
            .get_or_create(
                local_source,
                metadata(
                    &format!("{title} feat. {guest}"),
                    &["beabadoobee"],
                    local_ms,
                ),
            )
            .await
            .unwrap();
        let attached = repository
            .get_or_create_provider(
                album_fixture_source(&format!("{title}-library")),
                metadata(provider_title, &["beabadoobee", guest], provider_ms),
                None,
            )
            .await
            .unwrap();
        assert_eq!(attached.id(), local.id());
        for version in ["Remix", "Live"] {
            let distinct = repository
                .get_or_create_provider(
                    album_fixture_source(&format!("{title}-library-{version}")),
                    metadata(
                        &format!("{provider_title} ({version})"),
                        &["beabadoobee", guest],
                        provider_ms,
                    ),
                    None,
                )
                .await
                .unwrap();
            assert_ne!(distinct.id(), local.id());
        }
        assert_eq!(repository.find_sources(local.id()).await.unwrap().len(), 2);
    }
}

#[sqlx::test]
async fn feature_title_selection_agrees_for_library_album_repair_and_pin(pool: PgPool) {
    use crate::features::music_library::domain::{
        AlbumMetadata, AlbumTrack, ProviderId, SourceIdentity, SourceScope, TrackMetadata,
    };
    use crate::features::music_library::test_support::{
        CatalogueWrite, LibraryRead, Retention, SourceLookup,
    };
    let repository = PostgresTrackRepository::new(pool.clone());
    let metadata = |title: &str, artist: &str| {
        TrackMetadata::from_names(title.to_owned(), vec![artist.to_owned()], Some(200_000)).unwrap()
    };
    let local_source = |name: &str| {
        SourceIdentity::new(
            ProviderId::new("local".to_owned()).unwrap(),
            SourceScope::Named("/music".to_owned()),
            name.to_owned(),
        )
        .unwrap()
    };
    repository
        .get_or_create(local_source("song.flac"), metadata("Song", "Artist"))
        .await
        .unwrap();
    let library_feature = repository
        .get_or_create(
            local_source("feature.flac"),
            metadata("Song (feat. Guest)", "Artist"),
        )
        .await
        .unwrap();
    let library_matched = repository
        .get_or_create_provider(
            album_fixture_source("library-feature"),
            metadata("Song (feat. Guest)", "Artist"),
            None,
        )
        .await
        .unwrap();
    assert_ne!(library_matched.id(), library_feature.id());

    let album = repository
        .resolve_or_create_album(&AlbumMetadata::new("Provider Album".to_owned()).unwrap())
        .await
        .unwrap();
    let plain = repository
        .get_or_create(
            album_fixture_source("plain"),
            metadata("Song", "Provider Artist"),
        )
        .await
        .unwrap();
    let feature = repository
        .get_or_create(
            album_fixture_source("feature"),
            metadata("Song (feat. Guest)", "Provider Artist"),
        )
        .await
        .unwrap();
    for track in [&plain, &feature] {
        repository
            .add_album_track(album.id(), track.id(), &AlbumTrack::unplaced())
            .await
            .unwrap();
    }
    let album_matched = repository
        .get_or_create_provider(
            album_fixture_source("feature-match"),
            metadata("Song (feat. Guest)", "Provider Artist"),
            Some(album.id()),
        )
        .await
        .unwrap();
    assert_ne!(album_matched.id(), feature.id());
    let legacy = album_matched;
    repository
        .add_album_track(album.id(), legacy.id(), &AlbumTrack::unplaced())
        .await
        .unwrap();
    let entries = repository
        .find_album_entries(album.id(), None)
        .await
        .unwrap();
    assert_eq!(entries.len(), 3);
    let collapsed = super::pin::collapse_album_entries(&entries);
    assert_eq!(collapsed.len(), 3);
    assert_eq!(collapsed[0].track().id(), plain.id());
    assert_eq!(collapsed[1].track().id(), feature.id());
    assert_eq!(collapsed[2].track().id(), legacy.id());
    let preview = repository.provider_duplicates(&[]).await.unwrap();
    assert!(preview.track_merges.is_empty());
    assert!(preview.duplicates.is_empty());
    assert_eq!(
        repository.delete_provider_duplicates(&[]).await.unwrap(),
        preview
    );
    assert_eq!(
        repository
            .find_album_entries(album.id(), None)
            .await
            .unwrap()
            .len(),
        3
    );
    assert_eq!(
        repository.find_sources(feature.id()).await.unwrap().len(),
        1
    );
}

#[sqlx::test]
async fn pylon_repair_refetch_orders_fourteen_tracks_and_pins_the_release(pool: PgPool) {
    use crate::features::music_library::domain::{ProviderId, SourceIdentity, SourceScope};
    use crate::features::music_library::test_support::{
        CatalogueWrite, LibraryRead, Retention, SourceLookup,
    };
    let album_id = seed_legacy_pylon(&pool).await;
    let repository = PostgresTrackRepository::new(pool.clone());
    let library_source = SourceIdentity::new(
        ProviderId::new("local".to_owned()).unwrap(),
        SourceScope::Named("/music".to_owned()),
        "switchblade.flac".to_owned(),
    )
    .unwrap();
    let switchblade = repository
        .get_or_create(
            library_source,
            pylon_candidates()[6]
                .metadata()
                .clone()
                .with_recording_id(Some("library-switchblade".to_owned()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        repository
            .find_album_entries(&album_id, None)
            .await
            .unwrap()
            .len(),
        15
    );
    let preview = repository.provider_duplicates(&[]).await.unwrap();
    assert_eq!(preview.track_merges.len(), 2);
    assert_eq!(preview.refetch.len(), 1);
    assert_eq!(
        repository
            .find_album_entries(&album_id, None)
            .await
            .unwrap()
            .len(),
        15
    );
    assert_eq!(
        preview,
        repository.delete_provider_duplicates(&[]).await.unwrap()
    );
    assert_eq!(
        repository
            .find_album_entries(&album_id, None)
            .await
            .unwrap()
            .len(),
        13
    );
    assert!(
        repository
            .find_provider_entity_state(&album_fixture_source("MPREb_pylon"))
            .await
            .unwrap()
            .is_none()
    );
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let staging = std::env::temp_dir().join(Uuid::now_v7().to_string());
    let harness = Harness::build_with_options(
        pool.clone(),
        staging,
        Clock::system(),
        None,
        None,
        1,
        Some(Arc::new(AlbumFixtureProvider {
            fetched: crate::features::music_library::catalogue::FetchedAlbum::new(
                pylon_candidates(),
                Some(14),
            ),
            calls: Arc::clone(&calls),
        })),
    )
    .await;
    harness
        .lookup
        .candidates
        .lock()
        .unwrap()
        .push_back(Ok(vec![pylon_release()]));
    *harness.ownership.owned.lock().unwrap() = true;
    let request = harness
        .service
        .request(AcquisitionOrigin::Album(album_id.clone()))
        .await
        .unwrap();
    assert_eq!(
        harness.service.advance(request.request.id).await.unwrap(),
        Step::Done
    );
    let stored = harness
        .store
        .find_request(request.request.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        stored.target,
        Some(AcquisitionTarget::Release(release_id()))
    );
    assert_eq!(stored.state, RequestState::AlreadyOwned);
    assert_eq!(stored.expected_tracks.len(), 14);
    let entries = repository
        .find_album_entries(&album_id, None)
        .await
        .unwrap();
    assert_eq!(entries.len(), 14);
    assert_eq!(
        entries
            .iter()
            .map(|entry| entry.track().title())
            .collect::<Vec<_>>(),
        pylon_release()
            .tracks
            .iter()
            .map(|track| track.title.as_str())
            .collect::<Vec<_>>()
    );
    for (index, entry) in entries.iter().enumerate() {
        assert_eq!(
            entry.membership().track_number(),
            Some(u16::try_from(index + 1).unwrap())
        );
        assert_eq!(entry.membership().position(), u32::try_from(index).unwrap());
    }
    assert_eq!(entries[6].track().id(), switchblade.id());
    assert_eq!(
        repository
            .find_sources(switchblade.id())
            .await
            .unwrap()
            .len(),
        2
    );
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
}

#[derive(Clone)]
struct PinLogWriter(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for PinLogWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for PinLogWriter {
    type Writer = Self;

    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

async fn pin_failure_events(harness: &Harness, id: Uuid) -> Vec<serde_json::Value> {
    use tracing::instrument::WithSubscriber;
    let writer = PinLogWriter(Arc::new(Mutex::new(Vec::new())));
    let subscriber = tracing_subscriber::fmt()
        .json()
        .with_writer(writer.clone())
        .finish();
    harness
        .service
        .advance(id)
        .with_subscriber(subscriber)
        .await
        .unwrap();
    let bytes = writer.0.lock().unwrap().clone();
    std::str::from_utf8(&bytes)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .filter(|event: &serde_json::Value| event["fields"]["message"] == "acquisition pin failed")
        .collect()
}

#[sqlx::test]
async fn album_pin_failures_store_and_log_the_collapsed_count_once(pool: PgPool) {
    let album_id = seed_legacy_pylon(&pool).await;
    let staging = std::env::temp_dir().join(Uuid::now_v7().to_string());
    let harness = Harness::new(pool.clone(), staging, Clock::system()).await;
    let candidates = vec![pylon_release(), pylon_release(), pylon_release()];
    harness
        .lookup
        .candidates
        .lock()
        .unwrap()
        .push_back(Ok(candidates));
    let request = harness
        .service
        .request(AcquisitionOrigin::Album(album_id.clone()))
        .await
        .unwrap();
    let events = pin_failure_events(&harness, request.request.id).await;
    let stored = harness
        .store
        .find_request(request.request.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.state, RequestState::ReleaseNotFound);
    assert_eq!(
        stored.reason.as_deref(),
        Some("release_not_found: origin 13 tracks; candidates 14,14,14")
    );

    assert_eq!(events.len(), 1);
    assert_eq!(events[0]["level"], "INFO");
    assert_eq!(events[0]["fields"]["origin_entries"], 13);
    assert_eq!(events[0]["fields"]["candidate_releases"], 3);
    assert_eq!(
        events[0]["fields"]["candidate_track_counts"],
        "[14, 14, 14]"
    );
    assert_eq!(events[0]["span"]["name"], "advance");
}

#[sqlx::test]
async fn an_ambiguous_album_pin_stores_the_candidate_counts(pool: PgPool) {
    use crate::features::music_library::domain::AlbumTrack;
    use crate::features::music_library::test_support::CatalogueWrite;
    let repository = PostgresTrackRepository::new(pool.clone());
    let song = &pylon_candidates()[0];
    let album = repository
        .resolve_or_create_album(song.album().unwrap())
        .await
        .unwrap();
    let first = repository
        .get_or_create(song.source().clone(), song.metadata().clone())
        .await
        .unwrap();
    let second = repository
        .get_or_create(album_fixture_source("duplicate"), song.metadata().clone())
        .await
        .unwrap();
    for track in [&first, &second] {
        repository
            .add_album_track(album.id(), track.id(), &AlbumTrack::unplaced())
            .await
            .unwrap();
    }
    let harness = Harness::new(
        pool,
        std::env::temp_dir().join(Uuid::now_v7().to_string()),
        Clock::system(),
    )
    .await;
    let mut release = pylon_release();
    release.tracks.truncate(1);
    let mut another = release.clone();
    another.id = recording_id();
    another.tracks[0].title = "Different".to_owned();
    harness
        .lookup
        .candidates
        .lock()
        .unwrap()
        .push_back(Ok(vec![release, another]));
    let request = harness
        .service
        .request(AcquisitionOrigin::Album(album.id().clone()))
        .await
        .unwrap();
    let events = pin_failure_events(&harness, request.request.id).await;
    assert_eq!(events.len(), 1);
    assert_eq!(events[0]["level"], "INFO");
    assert_eq!(events[0]["fields"]["origin_entries"], 1);
    assert_eq!(events[0]["fields"]["candidate_releases"], 2);
    assert_eq!(events[0]["fields"]["candidate_track_counts"], "[1, 1]");
    assert_eq!(events[0]["span"]["name"], "advance");
    let stored = harness
        .store
        .find_request(request.request.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.state, RequestState::ReleaseAmbiguous);
    assert_eq!(
        stored.reason.as_deref(),
        Some("release_ambiguous: origin 1 tracks; candidates 1,1")
    );
}

struct HeldProvider {
    called: Arc<tokio::sync::Notify>,
    release: Arc<tokio::sync::Notify>,
    fetched: crate::features::music_library::catalogue::FetchedAlbum,
}

#[async_trait]
impl crate::features::music_library::catalogue::CatalogueProvider for HeldProvider {
    async fn search_tracks(
        &self,
        _search: &crate::features::music_library::catalogue::CatalogueSearch,
        _context: &crate::platform::request::RequestContext,
    ) -> Result<
        Vec<crate::features::music_library::catalogue::CatalogueCandidate>,
        crate::features::music_library::catalogue::ProviderError,
    > {
        Ok(Vec::new())
    }

    async fn fetch_album(
        &self,
        _source: &crate::features::music_library::domain::SourceIdentity,
        _context: &crate::platform::request::RequestContext,
    ) -> Result<
        crate::features::music_library::catalogue::FetchedAlbum,
        crate::features::music_library::catalogue::ProviderError,
    > {
        self.called.notify_one();
        self.release.notified().await;

        Ok(self.fetched.clone())
    }
}

#[sqlx::test]
async fn an_album_pin_waits_for_completion(pool: PgPool) {
    use crate::features::music_library::availability::ProviderRegistry;
    use crate::features::music_library::domain::SourceIdentity;
    use crate::features::music_library::domain::{AlbumMetadata, ProviderId, SourceScope};
    use crate::features::music_library::test_support::CatalogueWrite;

    awa::migrations::run(&pool).await.expect("the awa schema");

    let repository = Arc::new(PostgresTrackRepository::new(pool.clone()));
    let library = Arc::new(LibraryService::new(
        Arc::clone(&repository)
            as Arc<dyn crate::features::music_library::test_support::LibraryRead>,
        Arc::clone(&repository)
            as Arc<dyn crate::features::music_library::test_support::SourceLookup>,
        Arc::clone(&repository)
            as Arc<dyn crate::features::music_library::test_support::StateStore>,
    ));
    let album = repository
        .resolve_or_create_album(&AlbumMetadata::new("Provider Album".to_owned()).unwrap())
        .await
        .unwrap();
    let provider_source = SourceIdentity::new(
        ProviderId::new("theaudiodb".to_owned()).unwrap(),
        SourceScope::Global,
        "ext-1".to_owned(),
    )
    .unwrap();
    repository
        .set_album_source(album.id(), &provider_source)
        .await
        .unwrap();

    let search_service = Arc::new(SearchService::new(
        Arc::clone(&repository)
            as Arc<dyn crate::features::music_library::test_support::LibraryRead>,
        Arc::clone(&repository)
            as Arc<dyn crate::features::music_library::test_support::CatalogueWrite>,
        Arc::clone(&repository)
            as Arc<dyn crate::features::music_library::test_support::SourceLookup>,
        Arc::clone(&repository)
            as Arc<dyn crate::features::music_library::test_support::StateStore>,
    ));
    let called = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let candidate_metadata = crate::features::music_library::domain::TrackMetadata::from_names(
        "Completed Track".to_owned(),
        vec!["An Artist".to_owned()],
        Some(200_000),
    )
    .unwrap();
    let fetched = crate::features::music_library::catalogue::FetchedAlbum::new(
        vec![
            crate::features::music_library::catalogue::CatalogueCandidate::new(
                provider_source.clone(),
                candidate_metadata,
            )
            .with_album(Some(
                AlbumMetadata::new("Provider Album".to_owned()).unwrap(),
            )),
        ],
        Some(1),
    );
    let completion = Arc::new(
        CompletionService::new(
            Arc::clone(&library),
            Arc::clone(&repository)
                as Arc<dyn crate::features::music_library::test_support::CatalogueWrite>,
            Arc::clone(&repository)
                as Arc<dyn crate::features::music_library::test_support::SourceLookup>,
            vec![Arc::new(HeldProvider {
                called: Arc::clone(&called),
                release: Arc::clone(&release),
                fetched,
            })],
            search_service,
        )
        .with_registry(ProviderRegistry::with_enabled(["theaudiodb"]))
        .with_budget(Duration::from_secs(3600)),
    );

    let store = Arc::new(PostgresAcquisitionStore::new(pool));
    let ports = AcquisitionPorts::new(
        Arc::new(FakeSearch {
            responses: Mutex::new(VecDeque::new()),
        }),
        Arc::new(FakeTransfer::default()),
        {
            let lookup = FakeLookup {
                release: Mutex::new(None),
                candidates: Mutex::new(VecDeque::new()),
                recordings: Mutex::new(VecDeque::new()),
            };
            lookup
                .candidates
                .lock()
                .unwrap()
                .push_back(Ok(vec![MbRelease {
                    id: release_id(),
                    artist: "An Artist".to_owned(),
                    title: "Provider Album".to_owned(),
                    tracks: vec![ExpectedTrack {
                        disc: 1,
                        position: 1,
                        title: "Completed Track".to_owned(),
                        length_ms: Some(200_000),
                    }],
                }]));

            Arc::new(lookup)
        },
        Arc::new(FakeImporter::default()),
        Arc::new(FakeOwnership {
            owned: Mutex::new(false),
        }),
        completion,
    );
    let service = Arc::new(AcquisitionService::new(
        store,
        ports,
        Arc::clone(&library),
        AcquisitionConfig {
            enabled: true,
            staging_dir: Some(PathBuf::from("/staging")),
            library_dir: Some(PathBuf::from("/staging")),
            candidate_budget: 3,
            race_width: 1,
            start_timeout: Duration::from_secs(600),
            transfer_timeout: Duration::from_secs(1800),
            poll: Duration::from_secs(10),
        },
        Clock::system(),
    ));
    let request = service
        .request(AcquisitionOrigin::Album(album.id().clone()))
        .await
        .unwrap();

    let handle = tokio::spawn({
        let service = Arc::clone(&service);
        let id = request.request.id;
        async move { service.advance(id).await }
    });
    called.notified().await;
    assert!(
        !handle.is_finished(),
        "the pin waits for the completion provider"
    );
    release.notify_one();
    let _ = handle.await.unwrap();

    let stored = service.get(request.request.id).await.unwrap().unwrap();
    assert_eq!(
        stored.target,
        Some(AcquisitionTarget::Release(release_id()))
    );
    assert_eq!(
        stored.expected_tracks.len(),
        1,
        "the pin uses the provider's completed track list"
    );
    assert_eq!(stored.expected_tracks[0].length_ms, Some(200_000));
}

#[sqlx::test]
async fn a_start_error_does_not_advance_a_candidate(pool: PgPool) {
    for (index, error) in [
        TransferError::InvalidRequest,
        TransferError::Unsupported,
        TransferError::Malformed,
        TransferError::CredentialsRejected,
        TransferError::Internal,
    ]
    .into_iter()
    .enumerate()
    {
        let staging = std::env::temp_dir().join(format!("whio-aq-{}", Uuid::now_v7()));
        let harness = Harness::new(pool.clone(), staging, Clock::system()).await;
        let id = Uuid::now_v7();
        let origin = AcquisitionOrigin::Release(MbId::new(Uuid::from_u128(index as u128 + 1)));
        harness.store.insert_request(id, &origin).await.unwrap();
        harness
            .store
            .set_target(
                id,
                &AcquisitionTarget::Release(release_id()),
                "a",
                "t",
                &expected_track(),
            )
            .await
            .unwrap();
        harness
            .store
            .set_state(id, RequestState::Searching)
            .await
            .unwrap();
        harness
            .search
            .responses
            .lock()
            .unwrap()
            .push_back(Ok(vec![candidate("One.flac")]));
        harness
            .transfer
            .starts
            .lock()
            .unwrap()
            .push_back(Err(error));

        assert!(
            matches!(
                harness.service.advance(id).await.unwrap_err(),
                AcquisitionError::Failed
            ),
            "start {error:?} is not retryable and not a candidate rejection"
        );
        assert!(
            harness.store.load_attempt(id, 2).await.unwrap().is_none(),
            "no successor is started after {error:?}"
        );
    }
}

#[sqlx::test]
async fn a_cancel_error_does_not_advance_a_candidate(pool: PgPool) {
    for (index, error) in [
        TransferError::InvalidRequest,
        TransferError::Unsupported,
        TransferError::Malformed,
        TransferError::CredentialsRejected,
        TransferError::Internal,
    ]
    .into_iter()
    .enumerate()
    {
        let staging = std::env::temp_dir().join(format!("whio-aq-{}", Uuid::now_v7()));
        let harness = Harness::new(pool.clone(), staging, Clock::system()).await;
        let id = Uuid::now_v7();
        let origin = AcquisitionOrigin::Release(MbId::new(Uuid::from_u128(index as u128 + 100)));
        harness.store.insert_request(id, &origin).await.unwrap();
        harness
            .store
            .set_target(
                id,
                &AcquisitionTarget::Release(release_id()),
                "a",
                "t",
                &expected_track(),
            )
            .await
            .unwrap();
        harness
            .store
            .set_candidates(id, &[candidate("One.flac"), candidate("Two.flac")])
            .await
            .unwrap();
        harness
            .store
            .set_state(id, RequestState::Transferring)
            .await
            .unwrap();
        seed_attempt(&harness, id, vec![candidate_file("One.flac")]).await;
        harness
            .store
            .update_attempt(
                id,
                1,
                &AttemptUpdate {
                    state: AttemptState::Transferring,
                    reason: None,
                    handles: vec![Some("handle-0".to_owned())],
                    delivered: Vec::new(),
                    imported: Vec::new(),
                },
            )
            .await
            .unwrap();
        harness
            .transfer
            .statuses
            .lock()
            .unwrap()
            .push_back(Ok(vec![Slot::Accepted(progress(TransferPhase::Failed))]));
        harness
            .transfer
            .cancels
            .lock()
            .unwrap()
            .push_back(Err(error));

        assert!(
            matches!(
                harness.service.advance(id).await.unwrap_err(),
                AcquisitionError::Failed
            ),
            "cancel {error:?} is an explicit failure"
        );
        assert!(
            harness.store.load_attempt(id, 2).await.unwrap().is_none(),
            "a failed cancellation never starts a successor"
        );
    }
}

#[sqlx::test]
async fn a_successor_outage_does_not_suppress_predecessor_cleanup(pool: PgPool) {
    let staging = std::env::temp_dir().join(format!("whio-aq-{}", Uuid::now_v7()));
    let harness = Harness::new(pool.clone(), staging.clone(), Clock::system()).await;
    let id = seeded(
        &harness,
        AcquisitionTarget::Release(release_id()),
        RequestState::Importing,
        vec![candidate("Album/One.flac"), candidate("Album/Two.flac")],
    )
    .await;
    std::fs::create_dir_all(staging.join("Album")).unwrap();
    std::fs::write(staging.join("Album/One.flac"), b"audio").unwrap();
    seed_attempt_with_delivered(&harness, id, &["Album/One.flac".to_owned()]).await;
    harness
        .importer
        .outcomes
        .lock()
        .unwrap()
        .push_back(Ok(ImportOutcome { items: Vec::new() }));
    harness
        .transfer
        .starts
        .lock()
        .unwrap()
        .push_back(Err(TransferError::Unavailable));

    assert!(matches!(
        harness.service.advance(id).await.unwrap_err(),
        AcquisitionError::Unavailable
    ));
    assert!(
        !staging.join("Album/One.flac").exists(),
        "the skipped attempt is cleaned before its successor starts"
    );
    assert!(harness.store.load_attempt(id, 2).await.unwrap().is_some());
}

#[sqlx::test]
async fn a_committed_selection_resumes_without_searching(pool: PgPool) {
    let staging = std::env::temp_dir().join(format!("whio-aq-{}", Uuid::now_v7()));
    let harness = Harness::new(pool, staging, Clock::system()).await;
    let id = seeded(
        &harness,
        AcquisitionTarget::Release(release_id()),
        RequestState::Searching,
        vec![candidate("One.flac")],
    )
    .await;

    let _ = harness.service.advance(id).await.unwrap();

    assert!(
        harness.store.load_attempt(id, 1).await.unwrap().is_some(),
        "the stored selection starts attempt 1 instead of re-searching"
    );
    assert_ne!(state(&harness, id).await, RequestState::NoCandidate);
}

#[sqlx::test]
async fn a_missing_complete_file_times_out_after_the_budget(pool: PgPool) {
    let staging = std::env::temp_dir().join(format!("whio-aq-{}", Uuid::now_v7()));
    let future = Utc::now() + chrono::Duration::hours(2);
    let harness = Harness::new(pool, staging, Clock::from_fn(move || future)).await;
    let id = seeded(
        &harness,
        AcquisitionTarget::Release(release_id()),
        RequestState::Transferring,
        vec![candidate("Album/One.flac"), candidate("Album/Two.flac")],
    )
    .await;
    seed_attempt(&harness, id, vec![candidate_file("One.flac")]).await;
    harness
        .store
        .update_attempt(
            id,
            1,
            &AttemptUpdate {
                state: AttemptState::Transferring,
                reason: None,
                handles: vec![Some("handle-0".to_owned())],
                delivered: Vec::new(),
                imported: Vec::new(),
            },
        )
        .await
        .unwrap();
    harness
        .transfer
        .statuses
        .lock()
        .unwrap()
        .push_back(Ok(vec![Slot::Accepted(progress_with(
            TransferPhase::ReportedComplete,
            Some("Album/One.flac"),
        ))]));

    let _ = harness.service.advance(id).await.unwrap();

    let attempt = harness.store.load_attempt(id, 1).await.unwrap().unwrap();
    assert_eq!(attempt.reason.as_deref(), Some("transfer_timeout"));
    assert!(harness.store.load_attempt(id, 2).await.unwrap().is_some());
}

#[sqlx::test]
async fn an_invalid_complete_input_conflicts_without_waiting(pool: PgPool) {
    let staging = std::env::temp_dir().join(format!("whio-aq-{}", Uuid::now_v7()));
    let harness = Harness::new(pool, staging.clone(), Clock::system()).await;
    std::fs::create_dir_all(&staging).unwrap();
    std::os::unix::fs::symlink("/etc", staging.join("link")).unwrap();
    let id = seeded(
        &harness,
        AcquisitionTarget::Release(release_id()),
        RequestState::Transferring,
        vec![candidate("link/passwd"), candidate("Album/Two.flac")],
    )
    .await;
    seed_attempt(&harness, id, vec![candidate_file("passwd")]).await;
    harness
        .store
        .update_attempt(
            id,
            1,
            &AttemptUpdate {
                state: AttemptState::Transferring,
                reason: None,
                handles: vec![Some("handle-0".to_owned())],
                delivered: Vec::new(),
                imported: Vec::new(),
            },
        )
        .await
        .unwrap();
    harness
        .transfer
        .statuses
        .lock()
        .unwrap()
        .push_back(Ok(vec![Slot::Accepted(progress_with(
            TransferPhase::ReportedComplete,
            Some("link/passwd"),
        ))]));

    let _ = harness.service.advance(id).await.unwrap();

    let attempt = harness.store.load_attempt(id, 1).await.unwrap().unwrap();
    assert_eq!(attempt.reason.as_deref(), Some("staging_conflict"));
}

#[sqlx::test]
async fn a_symlinked_staging_root_is_a_conflict(pool: PgPool) {
    let real = std::env::temp_dir().join(format!("whio-aq-real-{}", Uuid::now_v7()));
    std::fs::create_dir_all(&real).unwrap();
    std::fs::write(real.join("Solo.flac"), b"secret").unwrap();
    let link = std::env::temp_dir().join(format!("whio-aq-link-{}", Uuid::now_v7()));
    std::os::unix::fs::symlink(&real, &link).unwrap();
    let harness = Harness::new(pool, link.clone(), Clock::system()).await;
    let id = seeded(
        &harness,
        AcquisitionTarget::Recording(recording_id()),
        RequestState::Importing,
        vec![candidate("Solo.flac")],
    )
    .await;
    seed_attempt_with_delivered(&harness, id, &["Solo.flac".to_owned()]).await;

    let _ = harness.service.advance(id).await.unwrap();

    assert_eq!(
        harness
            .store
            .load_attempt(id, 1)
            .await
            .unwrap()
            .unwrap()
            .reason
            .as_deref(),
        Some("staging_conflict")
    );
    assert!(
        real.join("Solo.flac").exists(),
        "a root symlink never grants import or delete authority"
    );
    let _ = std::fs::remove_dir_all(&real);
    let _ = std::fs::remove_file(&link);
}

fn importing_update(delivered: &str) -> AttemptUpdate {
    AttemptUpdate {
        state: AttemptState::Importing,
        reason: None,
        handles: vec![Some("handle-0".to_owned())],
        delivered: vec![PathBuf::from(delivered)],
        imported: Vec::new(),
    }
}

#[sqlx::test]
async fn two_first_claims_resolve_to_one_owner(pool: PgPool) {
    use crate::features::acquisition::storage::{ClaimBoundary, ClaimHook};

    let staging = std::env::temp_dir().join(format!("whio-aq-{}", Uuid::now_v7()));
    let harness = Harness::new(pool.clone(), staging, Clock::system()).await;
    let first = seeded(
        &harness,
        AcquisitionTarget::Release(release_id()),
        RequestState::Transferring,
        vec![candidate("Album/One.flac")],
    )
    .await;
    let second = Uuid::now_v7();
    harness
        .store
        .insert_request(
            second,
            &AcquisitionOrigin::Release(MbId::new(Uuid::from_u128(7))),
        )
        .await
        .unwrap();
    harness
        .store
        .set_state(second, RequestState::Transferring)
        .await
        .unwrap();
    seed_attempt(&harness, first, vec![candidate_file("One.flac")]).await;
    seed_attempt(&harness, second, vec![candidate_file("One.flac")]).await;
    let path = "Album/One.flac".to_owned();
    let (held, mut owner_entered) = Hold::new();
    let (competitor_entered, mut competitor) = tokio::sync::mpsc::unbounded_channel();
    let hook: ClaimHook = {
        let held = Arc::clone(&held);
        Arc::new(move |id, boundary| {
            let held = Arc::clone(&held);
            let competitor_entered = competitor_entered.clone();
            Box::pin(async move {
                if id == first && boundary == ClaimBoundary::AfterInsert {
                    held.wait().await;
                } else if id == second && boundary == ClaimBoundary::BeforeInsert {
                    competitor_entered.send(()).unwrap();
                }
            })
        })
    };
    let store = Arc::new(PostgresAcquisitionStore::new(pool.clone()).with_claim_hook(hook));
    let claim = |id| {
        let store = Arc::clone(&store);
        let path = path.clone();
        tokio::spawn(async move {
            store
                .claim_importing(id, 1, &importing_update(&path), std::slice::from_ref(&path))
                .await
        })
    };
    let a = claim(first);
    entered(&mut owner_entered).await;
    let b = claim(second);
    entered(&mut competitor).await;
    assert!(
        !a.is_finished(),
        "the owner's inserted claim remains uncommitted"
    );
    assert!(
        !b.is_finished(),
        "the competitor entered storage while the claim is in flight"
    );
    held.release(1);
    let a = joined(a).await.unwrap();
    let b = joined(b).await.unwrap();

    assert!(a ^ b, "exactly one attempt owns the path");
    assert!(a && !b, "the held insert identifies the actual owner");
    let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM acquisition_claims WHERE path = $1")
        .bind(&path)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(rows, 1);
    let owner: Uuid =
        sqlx::query_scalar("SELECT request_id FROM acquisition_claims WHERE path = $1")
            .bind(&path)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(owner, first);
    assert_eq!(state(&harness, first).await, RequestState::Importing);
    assert_eq!(state(&harness, second).await, RequestState::Transferring);
    assert!(
        harness
            .store
            .load_attempt(second, 1)
            .await
            .unwrap()
            .unwrap()
            .delivered
            .is_empty()
    );
}

#[sqlx::test]
async fn a_cleaned_path_is_reusable(pool: PgPool) {
    let staging = std::env::temp_dir().join(format!("whio-aq-{}", Uuid::now_v7()));
    let harness = Harness::new(pool, staging, Clock::system()).await;
    let first = seeded(
        &harness,
        AcquisitionTarget::Release(release_id()),
        RequestState::Transferring,
        vec![candidate("Album/One.flac")],
    )
    .await;
    seed_attempt(&harness, first, vec![candidate_file("One.flac")]).await;
    let path = staging_file(&harness, "Album/One.flac");
    assert!(
        harness
            .store
            .claim_importing(
                first,
                1,
                &importing_update(&path),
                std::slice::from_ref(&path)
            )
            .await
            .unwrap()
    );
    harness
        .importer
        .outcomes
        .lock()
        .unwrap()
        .push_back(Ok(ImportOutcome {
            items: vec![ImportedItem {
                release_id: Some(release_id().to_string()),
                recording_id: None,
                path: PathBuf::from("/library/One.flac"),
            }],
        }));
    assert_eq!(harness.service.advance(first).await.unwrap(), Step::Done);
    assert_eq!(state(&harness, first).await, RequestState::Completed);
    assert!(
        !harness.staging.join(&path).exists(),
        "service finish removed the claimed file"
    );
    assert!(
        harness
            .store
            .claimed_paths(first, 1)
            .await
            .unwrap()
            .is_empty()
    );

    let second = Uuid::now_v7();
    harness
        .store
        .insert_request(
            second,
            &AcquisitionOrigin::Release(MbId::new(Uuid::from_u128(8))),
        )
        .await
        .unwrap();
    harness
        .store
        .set_state(second, RequestState::Transferring)
        .await
        .unwrap();
    seed_attempt(&harness, second, vec![candidate_file("One.flac")]).await;
    staging_file(&harness, &path);
    std::fs::write(harness.staging.join(&path), b"replacement audio").unwrap();
    assert!(
        harness
            .store
            .claim_importing(
                second,
                1,
                &importing_update(&path),
                std::slice::from_ref(&path)
            )
            .await
            .unwrap(),
        "a service-cleaned path is reusable"
    );
    assert_eq!(
        harness.store.claimed_paths(second, 1).await.unwrap(),
        vec![path.clone()]
    );
    assert_eq!(
        std::fs::read(harness.staging.join(&path)).unwrap(),
        b"replacement audio"
    );
}

#[sqlx::test]
async fn a_colliding_recording_never_deletes_the_owners_file(pool: PgPool) {
    let staging = std::env::temp_dir().join(format!("whio-aq-{}", Uuid::now_v7()));
    let harness = Harness::new(pool, staging.clone(), Clock::system()).await;
    let owner = seeded(
        &harness,
        AcquisitionTarget::Release(release_id()),
        RequestState::Importing,
        vec![candidate("Album/One.flac")],
    )
    .await;
    std::fs::create_dir_all(staging.join("Album")).unwrap();
    std::fs::write(staging.join("Album/One.flac"), b"audio").unwrap();
    seed_attempt_with_delivered(&harness, owner, &["Album/One.flac".to_owned()]).await;

    let colliding = Uuid::now_v7();
    let other = AcquisitionOrigin::Release(MbId::new(Uuid::from_u128(9)));
    harness
        .store
        .insert_request(colliding, &other)
        .await
        .unwrap();
    harness
        .store
        .set_target(
            colliding,
            &AcquisitionTarget::Recording(recording_id()),
            "a",
            "t",
            &expected_track(),
        )
        .await
        .unwrap();
    harness
        .store
        .set_candidates(colliding, &[candidate("Album/One.flac")])
        .await
        .unwrap();
    harness
        .store
        .set_state(colliding, RequestState::Transferring)
        .await
        .unwrap();
    seed_attempt(&harness, colliding, vec![candidate_file("One.flac")]).await;
    harness
        .store
        .update_attempt(
            colliding,
            1,
            &AttemptUpdate {
                state: AttemptState::Transferring,
                reason: None,
                handles: vec![Some("handle-0".to_owned())],
                delivered: Vec::new(),
                imported: Vec::new(),
            },
        )
        .await
        .unwrap();
    harness
        .transfer
        .statuses
        .lock()
        .unwrap()
        .push_back(Ok(vec![Slot::Accepted(progress_with(
            TransferPhase::ReportedComplete,
            Some("Album/One.flac"),
        ))]));

    let _ = harness.service.advance(colliding).await.unwrap();

    assert_eq!(
        harness
            .store
            .load_attempt(colliding, 1)
            .await
            .unwrap()
            .unwrap()
            .reason
            .as_deref(),
        Some("staging_conflict")
    );
    assert!(
        staging.join("Album/One.flac").exists(),
        "the owner's file survives the colliding recording"
    );
    assert_eq!(
        harness.store.claimed_paths(owner, 1).await.unwrap(),
        vec!["Album/One.flac".to_owned()]
    );
}

#[sqlx::test]
async fn a_losing_request_resolves_after_the_winner_ends(pool: PgPool) {
    let staging = std::env::temp_dir().join(format!("whio-aq-{}", Uuid::now_v7()));
    let (before, mut reads) = Hold::new();
    let (conflict, mut conflicts) = Hold::new();
    let first_inserts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let before_hook: crate::features::acquisition::service::ConflictHook = {
        let before = Arc::clone(&before);
        Arc::new(move || {
            let before = Arc::clone(&before);
            let first_inserts = Arc::clone(&first_inserts);
            Box::pin(async move {
                if first_inserts.fetch_add(1, std::sync::atomic::Ordering::SeqCst) < 2 {
                    before.wait().await;
                }
            })
        })
    };
    let harness = Harness::build_with(
        pool.clone(),
        staging,
        Clock::system(),
        Some(conflict.hook()),
        Some(before_hook),
    )
    .await;
    let (completed, mut completions) = tokio::sync::mpsc::unbounded_channel();
    let caller = |label| {
        let service = Arc::clone(&harness.service);
        let completed = completed.clone();
        tokio::spawn(async move {
            let result = service
                .request(AcquisitionOrigin::Release(release_id()))
                .await;
            completed.send((label, result)).unwrap();
        })
    };
    let first = caller(0);
    let second = caller(1);
    entered(&mut reads).await;
    entered(&mut reads).await;
    before.release(2);

    let (winner_label, winner) = tokio::time::timeout(Duration::from_secs(10), completions.recv())
        .await
        .expect("the winning caller finishes")
        .unwrap();
    let winner = winner.unwrap();
    assert!(winner.created, "the completed caller is the actual winner");
    entered(&mut conflicts).await;
    assert_eq!(
        state(&harness, winner.request.id).await,
        RequestState::Pending
    );
    harness
        .store
        .set_terminal(
            winner.request.id,
            RequestState::Exhausted,
            Some("exhausted"),
        )
        .await
        .unwrap();
    conflict.release(1);

    let (loser_label, loser) = tokio::time::timeout(Duration::from_secs(10), completions.recv())
        .await
        .expect("the released loser resolves after the winner ends")
        .unwrap();
    let loser = loser.unwrap();
    assert_ne!(
        winner_label, loser_label,
        "caller identity is observed, not assigned by spawn order"
    );
    assert!(
        loser.created,
        "the loser repeats rather than reporting a failure"
    );
    assert_ne!(winner.request.id, loser.request.id);
    joined(first).await;
    joined(second).await;
    let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM acquisition_requests")
        .fetch_one(&pool)
        .await
        .unwrap();
    let jobs: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM awa.jobs WHERE kind = 'whio.acquisition.advance.v1'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(rows, 2);
    assert_eq!(jobs, 2, "each observed winning insertion enqueues once");
}

#[sqlx::test]
async fn a_conflict_resolution_is_not_capped(pool: PgPool) {
    use crate::features::acquisition::service::ConflictHook;
    use std::sync::atomic::Ordering;

    let staging = std::env::temp_dir().join(format!("whio-aq-{}", Uuid::now_v7()));
    let origin = release_id();
    let origin_id = origin.to_string();
    let remaining = Arc::new(std::sync::atomic::AtomicUsize::new(5));
    let before: ConflictHook = {
        let pool = pool.clone();
        let origin_id = origin_id.clone();
        let remaining = Arc::clone(&remaining);

        Arc::new(move || {
            let pool = pool.clone();
            let origin_id = origin_id.clone();
            let remaining = Arc::clone(&remaining);

            Box::pin(async move {
                if remaining.load(Ordering::SeqCst) == 0 {
                    return;
                }
                remaining.fetch_sub(1, Ordering::SeqCst);
                let _ = sqlx::query(
                    "INSERT INTO acquisition_requests (id, origin_kind, origin_id, state) \
                     VALUES ($1, 'release', $2, 'pending')",
                )
                .bind(Uuid::now_v7())
                .bind(&origin_id)
                .execute(&pool)
                .await;
            })
        })
    };
    let conflict: ConflictHook = {
        let pool = pool.clone();
        let origin_id = origin_id.clone();

        Arc::new(move || {
            let pool = pool.clone();
            let origin_id = origin_id.clone();

            Box::pin(async move {
                let _ = sqlx::query(
                    "UPDATE acquisition_requests SET state = 'exhausted' WHERE origin_id = $1 \
                     AND state NOT IN ('completed','already_owned','release_not_found',\
                     'release_ambiguous','no_candidate','exhausted','import_mismatch')",
                )
                .bind(&origin_id)
                .execute(&pool)
                .await;
            })
        })
    };

    let harness = Harness::build_with(
        pool.clone(),
        staging,
        Clock::system(),
        Some(conflict),
        Some(before),
    )
    .await;
    let outcome = harness
        .service
        .request(AcquisitionOrigin::Release(origin))
        .await
        .unwrap();

    assert!(
        outcome.created,
        "the create resolves after repeated conflicts"
    );
    let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM acquisition_requests")
        .fetch_one(&pool)
        .await
        .unwrap();
    let jobs: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM awa.jobs WHERE kind = 'whio.acquisition.advance.v1'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(rows, 6, "five conflicts then one insertion");
    assert_eq!(jobs, 1, "one job per actual insertion");
}

#[sqlx::test]
async fn an_owner_finishes_after_a_collision(pool: PgPool) {
    let staging = std::env::temp_dir().join(format!("whio-aq-{}", Uuid::now_v7()));
    let harness = Harness::new(pool, staging.clone(), Clock::system()).await;
    let owner = seeded(
        &harness,
        AcquisitionTarget::Release(release_id()),
        RequestState::Importing,
        vec![candidate("Album/One.flac")],
    )
    .await;
    std::fs::create_dir_all(staging.join("Album")).unwrap();
    std::fs::write(staging.join("Album/One.flac"), b"audio").unwrap();
    seed_attempt_with_delivered(&harness, owner, &["Album/One.flac".to_owned()]).await;

    let colliding = Uuid::now_v7();
    let other = AcquisitionOrigin::Release(MbId::new(Uuid::from_u128(11)));
    harness
        .store
        .insert_request(colliding, &other)
        .await
        .unwrap();
    harness
        .store
        .set_target(
            colliding,
            &AcquisitionTarget::Recording(recording_id()),
            "a",
            "t",
            &expected_track(),
        )
        .await
        .unwrap();
    harness
        .store
        .set_candidates(colliding, &[candidate("Album/One.flac")])
        .await
        .unwrap();
    harness
        .store
        .set_state(colliding, RequestState::Transferring)
        .await
        .unwrap();
    seed_attempt(&harness, colliding, vec![candidate_file("One.flac")]).await;
    harness
        .store
        .update_attempt(
            colliding,
            1,
            &AttemptUpdate {
                state: AttemptState::Transferring,
                reason: None,
                handles: vec![Some("handle-0".to_owned())],
                delivered: Vec::new(),
                imported: Vec::new(),
            },
        )
        .await
        .unwrap();
    harness
        .transfer
        .statuses
        .lock()
        .unwrap()
        .push_back(Ok(vec![Slot::Accepted(progress_with(
            TransferPhase::ReportedComplete,
            Some("Album/One.flac"),
        ))]));
    let _ = harness.service.advance(colliding).await.unwrap();

    harness
        .importer
        .outcomes
        .lock()
        .unwrap()
        .push_back(Ok(ImportOutcome {
            items: vec![ImportedItem {
                release_id: Some(release_id().to_string()),
                recording_id: None,
                path: PathBuf::from("/library/Album/One.flac"),
            }],
        }));
    assert_eq!(harness.service.advance(owner).await.unwrap(), Step::Done);
    assert_eq!(state(&harness, owner).await, RequestState::Completed);
    assert!(
        !staging.join("Album/One.flac").exists(),
        "the owner finishes and cleans its own file after the collision"
    );
    assert!(
        harness
            .store
            .claimed_paths(owner, 1)
            .await
            .unwrap()
            .is_empty()
    );
}

#[sqlx::test]
async fn private_cleanup_never_traverses_preexisting_bad_nodes(pool: PgPool) {
    for node in ["root", "marker", "attempt", "destination"] {
        let base = std::env::temp_dir().join(format!("whio-aq-{}", Uuid::now_v7()));
        let staging = base.join("staging");
        let outside = base.join("outside");
        let harness = Harness::new(pool.clone(), staging.clone(), Clock::system()).await;
        let id = seeded(
            &harness,
            AcquisitionTarget::Release(release_id()),
            RequestState::Importing,
            vec![candidate("One.flac")],
        )
        .await;
        let one = staging_file(&harness, "One.flac");
        seed_attempt_with_delivered(&harness, id, &[one]).await;
        harness
            .store
            .set_terminal(id, RequestState::Completed, None)
            .await
            .unwrap();
        let marker = staging.join(".whio-import");
        let private = marker.join(format!("{id}-1"));
        let outside_private = match node {
            "root" => outside.join(".whio-import").join(format!("{id}-1")),
            "marker" => outside.join(format!("{id}-1")),
            _ => outside.clone(),
        };
        std::fs::create_dir_all(&outside_private).unwrap();
        let secret = outside_private.join("One.flac");
        std::fs::write(&secret, b"outside audio").unwrap();
        match node {
            "root" => {
                std::fs::rename(&staging, base.join("saved-staging")).unwrap();
                std::os::unix::fs::symlink(&outside, &staging).unwrap();
            }
            "marker" => {
                std::os::unix::fs::symlink(&outside, &marker).unwrap();
            }
            "attempt" => {
                std::fs::create_dir(&marker).unwrap();
                std::os::unix::fs::symlink(&outside, &private).unwrap();
            }
            "destination" => {
                std::fs::create_dir_all(&private).unwrap();
                std::os::unix::fs::symlink(&secret, private.join("One.flac")).unwrap();
            }
            _ => unreachable!(),
        }

        assert_eq!(
            harness.service.advance(id).await.unwrap(),
            Step::Done,
            "{node}"
        );
        assert_eq!(state(&harness, id).await, RequestState::Completed, "{node}");
        assert_eq!(std::fs::read(&secret).unwrap(), b"outside audio", "{node}");
        assert!(outside_private.is_dir(), "{node}");
        assert!(harness.store.claimed_paths(id, 1).await.unwrap().is_empty());
        std::fs::remove_dir_all(base).unwrap();
    }
}

#[sqlx::test]
async fn a_terminal_request_resumes_its_unfinished_cleanup(pool: PgPool) {
    let staging = std::env::temp_dir().join(format!("whio-aq-{}", Uuid::now_v7()));
    let harness = Harness::new(pool, staging.clone(), Clock::system()).await;
    let id = seeded(
        &harness,
        AcquisitionTarget::Release(release_id()),
        RequestState::Importing,
        vec![candidate("Album/One.flac")],
    )
    .await;
    std::fs::create_dir_all(staging.join("Album")).unwrap();
    std::fs::write(staging.join("Album/One.flac"), b"audio").unwrap();
    seed_attempt_with_delivered(&harness, id, &["Album/One.flac".to_owned()]).await;

    harness
        .store
        .record_attempt(
            id,
            1,
            &AttemptUpdate {
                state: AttemptState::Imported,
                reason: None,
                handles: vec![Some("handle-0".to_owned())],
                delivered: vec![PathBuf::from("Album/One.flac")],
                imported: Vec::new(),
            },
            RequestState::Completed,
            None,
        )
        .await
        .unwrap();

    assert_eq!(harness.service.advance(id).await.unwrap(), Step::Done);
    assert_eq!(
        state(&harness, id).await,
        RequestState::Completed,
        "the terminal outcome is not overwritten"
    );
    assert!(
        !staging.join("Album/One.flac").exists(),
        "cleanup resumes for a terminal request"
    );
    assert!(harness.store.claimed_paths(id, 1).await.unwrap().is_empty());
}

#[path = "service_finish_tests.rs"]
mod finish_tests;

#[path = "service_race_tests.rs"]
mod race_tests;
