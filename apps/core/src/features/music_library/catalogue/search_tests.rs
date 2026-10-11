use crate::features::music_library::storage::contracts::{
    CatalogueWrite, DuplicateReport, LibraryRead, SourceLookup, StateStore,
};
use std::{
    collections::VecDeque,
    sync::{
        Arc as StdArc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

use async_trait::async_trait;
use parking_lot::Mutex;
use tokio::sync::{Semaphore, mpsc};
use tracing::field::{Field, Visit};
use tracing_subscriber::{Layer, layer::SubscriberExt};

use super::*;
use crate::{
    features::music_library::catalogue::{
        ArtistListFilter, CatalogueCandidate, CataloguePage, SearchKind, SearchLimit, SearchOffset,
        SearchQuery, SearchRequest, SearchWindow, SearchWindows,
    },
    features::music_library::{
        domain::{
            Album, AlbumId, AlbumMetadata, AlbumTrack, Artist, ArtistCredit, ArtistId, ProviderId,
            SourceIdentity, SourceScope, Track, TrackMetadata,
        },
        storage::contracts::TrackRepositoryError,
        storage::postgres::PostgresTrackRepository,
    },
    platform::request::RequestId,
};
use uuid::Uuid;

fn is_owned(sources: &[SourceIdentity]) -> bool {
    sources.iter().any(|source| {
        source.provider_id().kind()
            == Some(crate::features::music_library::domain::ProviderKind::Local)
    })
}

struct StubProvider {
    responses: Mutex<VecDeque<Result<Vec<CatalogueCandidate>, ProviderError>>>,
    delay: Duration,
}

struct FailingTrackRepository;

#[async_trait]
impl CatalogueWrite for FailingTrackRepository {
    async fn get_or_create(
        &self,
        _source: SourceIdentity,
        _metadata: TrackMetadata,
    ) -> Result<Track, TrackRepositoryError> {
        Err(TrackRepositoryError::Unavailable)
    }

    async fn get_or_create_provider(
        &self,
        _source: SourceIdentity,
        _metadata: TrackMetadata,
    ) -> Result<Track, TrackRepositoryError> {
        Err(TrackRepositoryError::Unavailable)
    }

    async fn add_album_track(
        &self,
        _album_id: &AlbumId,
        _track_id: &TrackId,
        _membership: &AlbumTrack,
    ) -> Result<(), TrackRepositoryError> {
        Err(TrackRepositoryError::Unavailable)
    }
}

#[async_trait]
impl LibraryRead for FailingTrackRepository {
    async fn find_track(
        &self,
        _track_id: &crate::features::music_library::domain::TrackId,
    ) -> Result<Option<Track>, TrackRepositoryError> {
        Err(TrackRepositoryError::Unavailable)
    }

    async fn page_tracks(
        &self,
        _offset: u32,
        _limit: u32,
        _order: crate::features::music_library::domain::TrackOrder,
        _filter: &crate::features::music_library::domain::TrackListFilter,
        _user: Option<Uuid>,
    ) -> Result<crate::features::music_library::domain::TrackPage, TrackRepositoryError> {
        Err(TrackRepositoryError::Unavailable)
    }

    async fn track_views(
        &self,
        _track_ids: &[crate::features::music_library::domain::TrackId],
        _user: Option<Uuid>,
    ) -> Result<Vec<crate::features::music_library::domain::TrackView>, TrackRepositoryError> {
        Err(TrackRepositoryError::Unavailable)
    }
}

#[async_trait]
impl SourceLookup for FailingTrackRepository {
    async fn find_sources(
        &self,
        _track_id: &crate::features::music_library::domain::TrackId,
    ) -> Result<Vec<SourceIdentity>, TrackRepositoryError> {
        Err(TrackRepositoryError::Unavailable)
    }
}

#[async_trait]
impl StateStore for FailingTrackRepository {}

#[async_trait]
impl DuplicateReport for FailingTrackRepository {}

impl StubProvider {
    fn new(responses: Vec<Result<Vec<CatalogueCandidate>, ProviderError>>) -> Self {
        Self {
            responses: Mutex::new(responses.into()),
            delay: Duration::ZERO,
        }
    }

    fn delayed(
        responses: Vec<Result<Vec<CatalogueCandidate>, ProviderError>>,
        delay: Duration,
    ) -> Self {
        Self {
            responses: Mutex::new(responses.into()),
            delay,
        }
    }
}

#[async_trait]
impl CatalogueProvider for StubProvider {
    async fn search_tracks(
        &self,
        _search: &CatalogueSearch,
        _context: &RequestContext,
    ) -> Result<Vec<CatalogueCandidate>, ProviderError> {
        if !self.delay.is_zero() {
            tokio::time::sleep(self.delay).await;
        }
        self.responses.lock().pop_front().unwrap()
    }
}

fn candidate(external_id: &str, title: &str) -> CatalogueCandidate {
    let provider_id = ProviderId::new("youtube_music".to_owned()).unwrap();
    let source =
        SourceIdentity::new(provider_id, SourceScope::Global, external_id.to_owned()).unwrap();
    let metadata = TrackMetadata::from_names(
        title.to_owned(),
        vec!["Daft Punk".to_owned()],
        Some(337_000),
    )
    .unwrap();

    CatalogueCandidate::new(source, metadata)
}

fn search() -> SearchRequest {
    SearchRequest::uniform(
        SearchQuery::new("Daft Punk".to_owned()).unwrap(),
        window(10, 0),
    )
}

fn search_with_offset(limit: u32, offset: u32) -> SearchRequest {
    SearchRequest::uniform(
        SearchQuery::new("Daft Punk".to_owned()).unwrap(),
        window(limit, offset),
    )
}

fn window(limit: u32, offset: u32) -> SearchWindow {
    SearchWindow::new(
        SearchLimit::new(limit).unwrap(),
        SearchOffset::new(offset).unwrap(),
    )
}

fn context() -> RequestContext {
    RequestContext::new(RequestId::new("request-123".to_owned()).unwrap())
}

struct Fixture {
    store: Arc<PostgresTrackRepository>,
}

impl Fixture {
    fn new(pool: sqlx::PgPool) -> Self {
        Self {
            store: Arc::new(PostgresTrackRepository::new(pool.clone())),
        }
    }

    fn library(&self) -> Arc<dyn LibraryRead> {
        Arc::clone(&self.store) as Arc<dyn LibraryRead>
    }

    fn catalogue(&self) -> Arc<dyn CatalogueWrite> {
        Arc::clone(&self.store) as Arc<dyn CatalogueWrite>
    }

    fn sources(&self) -> Arc<dyn SourceLookup> {
        Arc::clone(&self.store) as Arc<dyn SourceLookup>
    }

    fn state(&self) -> Arc<dyn StateStore> {
        Arc::clone(&self.store) as Arc<dyn StateStore>
    }

    fn service(&self, providers: Vec<Arc<dyn CatalogueProvider>>) -> SearchService {
        SearchService::with_providers(
            self.library(),
            self.catalogue(),
            self.sources(),
            self.state(),
            providers,
        )
    }

    fn bare(&self) -> SearchService {
        SearchService::new(
            self.library(),
            self.catalogue(),
            self.sources(),
            self.state(),
        )
    }
}

fn service(pool: sqlx::PgPool, providers: Vec<Arc<dyn CatalogueProvider>>) -> SearchService {
    Fixture::new(pool.clone()).service(providers)
}

struct KindProvider {
    track_calls: StdArc<AtomicUsize>,
    album_calls: StdArc<AtomicUsize>,
    artist_calls: StdArc<AtomicUsize>,
    album: AlbumMetadata,
    artist: ArtistCredit,
}

#[async_trait]
impl CatalogueProvider for KindProvider {
    async fn search_tracks(
        &self,
        _search: &CatalogueSearch,
        _context: &RequestContext,
    ) -> Result<Vec<CatalogueCandidate>, ProviderError> {
        self.track_calls.fetch_add(1, Ordering::SeqCst);
        Ok(Vec::new())
    }

    async fn search_albums(
        &self,
        _search: &CatalogueSearch,
        _context: &RequestContext,
    ) -> Result<
        crate::features::music_library::catalogue::CataloguePage<AlbumMetadata>,
        ProviderError,
    > {
        self.album_calls.fetch_add(1, Ordering::SeqCst);
        Ok(
            crate::features::music_library::catalogue::CataloguePage::new(
                vec![self.album.clone()],
                None,
            ),
        )
    }

    async fn search_artists(
        &self,
        _search: &CatalogueSearch,
        _context: &RequestContext,
    ) -> Result<crate::features::music_library::catalogue::CataloguePage<ArtistCredit>, ProviderError>
    {
        self.artist_calls.fetch_add(1, Ordering::SeqCst);
        Ok(
            crate::features::music_library::catalogue::CataloguePage::new(
                vec![self.artist.clone()],
                None,
            ),
        )
    }
}

fn provider(stub: impl CatalogueProvider + 'static) -> Arc<dyn CatalogueProvider> {
    Arc::new(stub) as Arc<dyn CatalogueProvider>
}

struct HangingProvider;

#[async_trait]
impl CatalogueProvider for HangingProvider {
    async fn search_tracks(
        &self,
        _search: &CatalogueSearch,
        _context: &RequestContext,
    ) -> Result<Vec<CatalogueCandidate>, ProviderError> {
        std::future::pending().await
    }
}

struct ConcurrencyProbe {
    current: StdArc<AtomicUsize>,
    maximum: StdArc<AtomicUsize>,
}

#[async_trait]
impl CatalogueProvider for ConcurrencyProbe {
    async fn search_tracks(
        &self,
        _search: &CatalogueSearch,
        _context: &RequestContext,
    ) -> Result<Vec<CatalogueCandidate>, ProviderError> {
        let active = self.current.fetch_add(1, Ordering::SeqCst) + 1;
        self.maximum.fetch_max(active, Ordering::SeqCst);
        tokio::time::sleep(Duration::from_millis(10)).await;
        self.current.fetch_sub(1, Ordering::SeqCst);
        Ok(Vec::new())
    }
}

struct CrossKindGate {
    current: StdArc<AtomicUsize>,
    maximum: StdArc<AtomicUsize>,
    started: mpsc::UnboundedSender<()>,
    release: StdArc<Semaphore>,
}

impl CrossKindGate {
    async fn enter(&self) {
        let active = self.current.fetch_add(1, Ordering::SeqCst) + 1;
        self.maximum.fetch_max(active, Ordering::SeqCst);
        self.started.send(()).unwrap();
        self.release.clone().acquire_owned().await.unwrap().forget();
        self.current.fetch_sub(1, Ordering::SeqCst);
    }
}

struct CrossKindProvider {
    gate: CrossKindGate,
}

#[async_trait]
impl CatalogueProvider for CrossKindProvider {
    async fn search_tracks(
        &self,
        _search: &CatalogueSearch,
        _context: &RequestContext,
    ) -> Result<Vec<CatalogueCandidate>, ProviderError> {
        self.gate.enter().await;
        Ok(Vec::new())
    }

    async fn search_albums(
        &self,
        _search: &CatalogueSearch,
        _context: &RequestContext,
    ) -> Result<
        crate::features::music_library::catalogue::CataloguePage<AlbumMetadata>,
        ProviderError,
    > {
        self.gate.enter().await;
        Ok(crate::features::music_library::catalogue::CataloguePage::new(Vec::new(), None))
    }

    async fn search_artists(
        &self,
        _search: &CatalogueSearch,
        _context: &RequestContext,
    ) -> Result<crate::features::music_library::catalogue::CataloguePage<ArtistCredit>, ProviderError>
    {
        self.gate.enter().await;
        Ok(crate::features::music_library::catalogue::CataloguePage::new(Vec::new(), None))
    }
}

#[sqlx::test]
async fn all_kinds_share_the_source_concurrency_bound(pool: sqlx::PgPool) {
    let (started, mut started_rx) = mpsc::unbounded_channel();
    let current = StdArc::new(AtomicUsize::new(0));
    let maximum = StdArc::new(AtomicUsize::new(0));
    let release = StdArc::new(Semaphore::new(0));
    let provider_count = MAX_SOURCE_CONCURRENCY;
    let providers = (0..provider_count)
        .map(|_| {
            provider(CrossKindProvider {
                gate: CrossKindGate {
                    current: StdArc::clone(&current),
                    maximum: StdArc::clone(&maximum),
                    started: started.clone(),
                    release: StdArc::clone(&release),
                },
            })
        })
        .collect();
    let catalogue = Arc::new(service(pool, providers));
    let request = search();
    let context = context();
    let search_task = tokio::spawn(async move { catalogue.search(&request, None, &context).await });
    for _ in 0..MAX_SOURCE_CONCURRENCY {
        wait_for_provider_start(&mut started_rx).await;
    }
    assert_eq!(current.load(Ordering::SeqCst), MAX_SOURCE_CONCURRENCY);
    release.add_permits(MAX_SOURCE_CONCURRENCY);

    let remaining = provider_count * 3 - MAX_SOURCE_CONCURRENCY;
    for _ in 0..remaining {
        wait_for_provider_start(&mut started_rx).await;
        release.add_permits(1);
    }

    search_task.await.unwrap().unwrap();
    assert_eq!(maximum.load(Ordering::SeqCst), MAX_SOURCE_CONCURRENCY);
    assert_eq!(current.load(Ordering::SeqCst), 0);
}

async fn wait_for_provider_start(receiver: &mut mpsc::UnboundedReceiver<()>) {
    tokio::time::timeout(Duration::from_secs(2), receiver.recv())
        .await
        .expect("provider call started before the hang deadline")
        .expect("provider channel stays open");
}

struct LimitRecordingProvider {
    limit: StdArc<Mutex<Vec<u8>>>,
}

#[async_trait]
impl CatalogueProvider for LimitRecordingProvider {
    async fn search_tracks(
        &self,
        search: &CatalogueSearch,
        _context: &RequestContext,
    ) -> Result<Vec<CatalogueCandidate>, ProviderError> {
        self.limit.lock().push(search.limit().get());
        Ok(Vec::new())
    }
}

struct PagedProvider {
    total: u32,
}

#[async_trait]
impl CatalogueProvider for PagedProvider {
    async fn search_tracks(
        &self,
        search: &CatalogueSearch,
        _context: &RequestContext,
    ) -> Result<Vec<CatalogueCandidate>, ProviderError> {
        let start = search.offset().get();
        let end = start
            .saturating_add(u32::from(search.limit().get()))
            .min(self.total);
        Ok((start..end)
            .map(|index| candidate(&format!("p-{index}"), &format!("P{index}")))
            .collect())
    }
}

#[sqlx::test]
async fn materialize_assigns_stable_ids_uses_latest_metadata_and_dedupes(pool: sqlx::PgPool) {
    let resolver = StubProvider::new(vec![
        Ok(vec![
            candidate("source-a", "Old title"),
            candidate("source-b", "Other"),
        ]),
        Ok(vec![candidate("source-a", "New title")]),
        Ok(vec![candidate("dup", "Same"), candidate("dup", "Same")]),
    ]);
    let catalogue = service(pool.clone(), vec![provider(resolver)]);

    let first = catalogue.search(&search(), None, &context()).await.unwrap();
    let second = catalogue.search(&search(), None, &context()).await.unwrap();
    let (first_a, first_b) = (&first.tracks[0], &first.tracks[1]);
    assert_ne!(first_a.track().id(), first_b.track().id());
    assert_eq!(first_a.track().id(), second.tracks[0].track().id());
    assert_eq!(first_a.track().title(), "Old title");
    assert_eq!(second.tracks[0].track().title(), "New title");
    assert_eq!(first_a.sources()[0].external_id(), "source-a");
    assert_eq!(first_b.sources()[0].external_id(), "source-b");

    let third = catalogue.search(&search(), None, &context()).await.unwrap();
    let titles = third
        .tracks
        .iter()
        .map(|hit| hit.track().title().to_owned())
        .collect::<Vec<_>>();
    assert_eq!(
        titles.len(),
        3,
        "the two known tracks join the one discovery, not three"
    );
    assert_eq!(
        titles
            .iter()
            .filter(|title| title.as_str() == "Same")
            .count(),
        1,
        "the duplicate provider page materialises one track"
    );
}

#[test]
fn merged_hits_keep_each_provider_once_in_query_order() {
    fn source(provider: &str, external_id: &str) -> SourceIdentity {
        SourceIdentity::new(
            ProviderId::new(provider.to_owned()).unwrap(),
            SourceScope::Named("home-server".to_owned()),
            external_id.to_owned(),
        )
        .unwrap()
    }

    let track = || {
        Track::new(
            TrackId::new(),
            TrackMetadata::from_names("song".to_owned(), vec!["artist".to_owned()], None).unwrap(),
        )
    };
    let mut hit = SearchHit::new(track(), None, vec![source("navidrome", "nd-1")]);
    hit.merge(SearchHit::new(
        track(),
        None,
        vec![source("youtube_music", "vid-1")],
    ));
    hit.merge(SearchHit::new(
        track(),
        None,
        vec![source("navidrome", "nd-1")],
    ));

    assert_eq!(
        hit.sources()
            .iter()
            .map(|source| source.provider_id().as_str())
            .collect::<Vec<_>>(),
        vec!["navidrome", "youtube_music"]
    );
}

#[sqlx::test]
async fn provider_order_decides_rank(pool: sqlx::PgPool) {
    let slow_preferred = StubProvider::delayed(
        vec![Ok(vec![candidate("preferred", "Preferred")])],
        Duration::from_millis(30),
    );
    let fast_fallback = StubProvider::delayed(
        vec![Ok(vec![candidate("fallback", "Fallback")])],
        Duration::from_millis(1),
    );
    let catalogue = service(
        pool.clone(),
        vec![provider(slow_preferred), provider(fast_fallback)],
    );
    let outcome = catalogue.search(&search(), None, &context()).await.unwrap();
    assert_eq!(
        outcome
            .tracks
            .iter()
            .map(|hit| hit.track().title())
            .collect::<Vec<_>>(),
        vec!["Preferred", "Fallback"]
    );
}

#[sqlx::test]
async fn failures_degrade_partially_and_error_in_total(pool: sqlx::PgPool) {
    let broken = StubProvider::new(vec![Err(ProviderError::ProviderUnavailable)]);
    let empty = StubProvider::new(vec![Ok(Vec::new())]);
    let working = StubProvider::new(vec![Ok(vec![candidate("ytm-1", "YTM Song")])]);
    let catalogue = service(
        pool.clone(),
        vec![provider(broken), provider(empty), provider(working)],
    );
    let outcome = catalogue.search(&search(), None, &context()).await.unwrap();
    assert_eq!(outcome.tracks.len(), 1);
    assert_eq!(outcome.tracks[0].track().title(), "YTM Song");
}

#[sqlx::test]
async fn total_provider_failure_is_not_silenced(pool: sqlx::PgPool) {
    let broken = StubProvider::new(vec![Err(ProviderError::ProviderUnavailable)]);
    let result = service(pool.clone(), vec![provider(broken)])
        .search(&search(), None, &context())
        .await;
    assert!(matches!(
        result,
        Err(CatalogueError::Provider(ProviderError::ProviderUnavailable))
    ));
}

#[sqlx::test]
async fn access_denial_takes_priority_over_other_providers(pool: sqlx::PgPool) {
    let denied = StubProvider::new(vec![Err(ProviderError::Access)]);
    let working = StubProvider::new(vec![Ok(vec![candidate("ytm-1", "YTM Song")])]);
    let result = service(pool.clone(), vec![provider(denied), provider(working)])
        .search(&search(), None, &context())
        .await;
    assert!(matches!(
        result,
        Err(CatalogueError::Provider(ProviderError::Access))
    ));
}

#[sqlx::test]
async fn aggregate_pagination_caps_slices_and_clamps(pool: sqlx::PgPool) {
    let first = StubProvider::new(vec![Ok((0..6)
        .map(|index| candidate(&format!("a-{index}"), &format!("A{index}")))
        .collect())]);
    let second = StubProvider::new(vec![Ok((0..6)
        .map(|index| candidate(&format!("b-{index}"), &format!("B{index}")))
        .collect())]);
    let catalogue = service(pool.clone(), vec![provider(first), provider(second)]);
    let outcome = catalogue.search(&search(), None, &context()).await.unwrap();
    assert_eq!(outcome.tracks.len(), 10);
    assert_eq!(outcome.tracks[0].track().title(), "A0");
    assert_eq!(outcome.tracks[6].track().title(), "B0");
}

#[sqlx::test]
async fn aggregate_pagination_offsets_provider_results(pool: sqlx::PgPool) {
    let resolver = StubProvider::new(vec![Ok((0..25)
        .map(|index| candidate(&format!("source-{index}"), &format!("Song {index}")))
        .collect())]);
    let catalogue = service(pool.clone(), vec![provider(resolver)]);
    let outcome = catalogue
        .search(&search_with_offset(10, 10), None, &context())
        .await
        .unwrap();
    assert_eq!(outcome.tracks.len(), 10);
    assert_eq!(outcome.tracks[0].track().title(), "Song 10");
    assert_eq!(outcome.tracks[9].track().title(), "Song 19");
}

#[sqlx::test]
async fn aggregate_pagination_caps_provider_window(pool: sqlx::PgPool) {
    let limits = StdArc::new(Mutex::new(Vec::new()));
    let resolver = LimitRecordingProvider {
        limit: StdArc::clone(&limits),
    };
    service(pool.clone(), vec![provider(resolver)])
        .search(&search_with_offset(10, 20), None, &context())
        .await
        .unwrap();
    assert_eq!(*limits.lock(), vec![25]);
}

#[sqlx::test]
async fn deep_pages_walk_the_provider_window(pool: sqlx::PgPool) {
    let catalogue = service(pool.clone(), vec![provider(PagedProvider { total: 80 })]);

    let outcome = catalogue
        .search(&search_with_offset(10, 30), None, &context())
        .await
        .unwrap();

    assert_eq!(outcome.tracks.len(), 10);
    assert_eq!(outcome.tracks[0].track().title(), "P30");
    assert_eq!(outcome.tracks[9].track().title(), "P39");
}

const WITHIN_SEARCH_BUDGET: Duration = Duration::from_secs(2);
const BEYOND_SEARCH_BUDGET: Duration = Duration::ZERO;

#[sqlx::test]
async fn the_within_budget_search_is_complete(pool: sqlx::PgPool) {
    let working = StubProvider::new(vec![Ok(vec![candidate("ytm-1", "YTM Song")])]);
    let catalogue = service(
        pool.clone(),
        vec![provider(HangingProvider), provider(working)],
    )
    .with_search_budget(WITHIN_SEARCH_BUDGET);
    let started = Instant::now();
    let (hits, _status) = catalogue
        .discover_tracks(
            &SearchQuery::new("Daft Punk".to_owned()).unwrap(),
            10,
            &context(),
        )
        .await
        .unwrap();
    assert!(started.elapsed() < Duration::from_secs(5));
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].track().title(), "YTM Song");

    let current = StdArc::new(AtomicUsize::new(0));
    let maximum = StdArc::new(AtomicUsize::new(0));
    let providers: Vec<Arc<dyn CatalogueProvider>> = (0..10)
        .map(|_| {
            Arc::new(ConcurrencyProbe {
                current: StdArc::clone(&current),
                maximum: StdArc::clone(&maximum),
            }) as Arc<dyn CatalogueProvider>
        })
        .collect();
    service(pool.clone(), providers)
        .search(&search(), None, &context())
        .await
        .unwrap();
    assert!(maximum.load(Ordering::SeqCst) <= MAX_SOURCE_CONCURRENCY);
}

#[sqlx::test]
async fn the_beyond_budget_search_still_returns(pool: sqlx::PgPool) {
    let repository = Arc::new(PostgresTrackRepository::new(pool.clone()));
    repository
        .get_or_create(local_source(), named_metadata("Zebra"))
        .await
        .unwrap();
    let resolver = StubProvider::new(vec![Ok(vec![candidate("vid-1", "Apple")])]);
    let catalogue = Fixture {
        store: Arc::clone(&repository),
    }
    .service(vec![provider(resolver)])
    .with_search_budget(BEYOND_SEARCH_BUDGET);

    let started = Instant::now();
    let outcome = catalogue.search(&search(), None, &context()).await.unwrap();

    assert!(started.elapsed() < Duration::from_secs(5));
    assert_eq!(outcome.tracks.len(), 1);
    assert_eq!(outcome.tracks[0].track().title(), "Zebra");
    assert!(is_owned(outcome.tracks[0].sources()));
}

#[sqlx::test]
async fn search_preserves_repository_errors(_pool: sqlx::PgPool) {
    let resolver = StubProvider::new(vec![Ok(vec![candidate("source-a", "Title")])]);
    let service = SearchService::with_providers(
        Arc::new(FailingTrackRepository) as Arc<dyn LibraryRead>,
        Arc::new(FailingTrackRepository) as Arc<dyn CatalogueWrite>,
        Arc::new(FailingTrackRepository) as Arc<dyn SourceLookup>,
        Arc::new(FailingTrackRepository) as Arc<dyn StateStore>,
        vec![provider(resolver)],
    );

    let result = service.search(&search(), None, &context()).await;

    assert!(matches!(
        result,
        Err(CatalogueError::Repository(
            TrackRepositoryError::Unavailable
        ))
    ));
}

struct AlbumReadFails;

#[async_trait]
impl LibraryRead for AlbumReadFails {
    async fn find_track(
        &self,
        _track_id: &crate::features::music_library::domain::TrackId,
    ) -> Result<Option<Track>, TrackRepositoryError> {
        Ok(None)
    }

    async fn page_tracks(
        &self,
        _offset: u32,
        _limit: u32,
        _order: crate::features::music_library::domain::TrackOrder,
        _filter: &crate::features::music_library::domain::TrackListFilter,
        _user: Option<Uuid>,
    ) -> Result<crate::features::music_library::domain::TrackPage, TrackRepositoryError> {
        Err(TrackRepositoryError::Unavailable)
    }

    async fn track_views(
        &self,
        _track_ids: &[crate::features::music_library::domain::TrackId],
        _user: Option<Uuid>,
    ) -> Result<Vec<crate::features::music_library::domain::TrackView>, TrackRepositoryError> {
        Ok(Vec::new())
    }

    async fn search_albums(
        &self,
        _query: &str,
        _limit: u32,
        _user: Option<Uuid>,
    ) -> Result<Vec<crate::features::music_library::domain::AlbumSummary>, TrackRepositoryError>
    {
        Err(TrackRepositoryError::Unavailable)
    }
}

#[sqlx::test]
async fn a_repository_error_in_one_kind_fails_the_search(pool: sqlx::PgPool) {
    let repository = Arc::new(PostgresTrackRepository::new(pool));
    let resolver = StubProvider::new(vec![Ok(vec![candidate("source-a", "Title")])]);
    let service = SearchService::with_providers(
        Arc::new(AlbumReadFails) as Arc<dyn LibraryRead>,
        repository.clone() as Arc<dyn CatalogueWrite>,
        repository.clone() as Arc<dyn SourceLookup>,
        repository as Arc<dyn StateStore>,
        vec![provider(resolver)],
    )
    .with_dedup_mode(DedupMode::Off);

    let result = service.search(&search(), None, &context()).await;

    assert!(matches!(
        result,
        Err(CatalogueError::Repository(
            TrackRepositoryError::Unavailable
        ))
    ));
}

#[sqlx::test]
async fn per_kind_discovery_materializes_only_the_requested_kind(pool: sqlx::PgPool) {
    let album_source = provider_source_for_test("album-source");
    let album_artist_source = provider_source_for_test("album-artist-source");
    let artist_source = provider_source_for_test("artist-source");
    let album = AlbumMetadata::new("The Marías".to_owned())
        .unwrap()
        .with_provider(Some(album_source))
        .with_album_artist(
            ArtistCredit::new("The Marías".to_owned(), None)
                .unwrap()
                .with_provider(Some(album_artist_source)),
        );
    let artist = ArtistCredit::new("Beyoncé".to_owned(), None)
        .unwrap()
        .with_provider(Some(artist_source));
    let track_calls = StdArc::new(AtomicUsize::new(0));
    let album_calls = StdArc::new(AtomicUsize::new(0));
    let artist_calls = StdArc::new(AtomicUsize::new(0));
    let stub = KindProvider {
        track_calls: StdArc::clone(&track_calls),
        album_calls: StdArc::clone(&album_calls),
        artist_calls: StdArc::clone(&artist_calls),
        album,
        artist,
    };
    let catalogue = service(pool, vec![provider(stub)]);
    let query = SearchQuery::new("marias".to_owned()).unwrap();
    let context = context();

    let albums = catalogue
        .search_albums(&query, window(10, 0), None, &context)
        .await
        .unwrap();
    assert_eq!(albums.len(), 1);
    assert_eq!(albums[0].album().title(), "The Marías");
    assert_eq!(artist_calls.load(Ordering::SeqCst), 0);
    assert_eq!(album_calls.load(Ordering::SeqCst), 1);

    let artists = catalogue
        .search_artists(
            &SearchQuery::new("beyonce".to_owned()).unwrap(),
            window(10, 0),
            &ArtistListFilter::default(),
            None,
            &context,
        )
        .await
        .unwrap();
    assert_eq!(artists.len(), 1);
    assert_eq!(artists[0].artist().name(), "Beyoncé");
    assert_eq!(album_calls.load(Ordering::SeqCst), 1);
    assert_eq!(artist_calls.load(Ordering::SeqCst), 1);
}

#[sqlx::test]
async fn a_zero_window_for_each_kind_skips_provider_operations(pool: sqlx::PgPool) {
    let track_calls = StdArc::new(AtomicUsize::new(0));
    let album_calls = StdArc::new(AtomicUsize::new(0));
    let artist_calls = StdArc::new(AtomicUsize::new(0));
    let stub = KindProvider {
        track_calls: StdArc::clone(&track_calls),
        album_calls: StdArc::clone(&album_calls),
        artist_calls: StdArc::clone(&artist_calls),
        album: AlbumMetadata::new("Unused".to_owned()).unwrap(),
        artist: ArtistCredit::new("Unused".to_owned(), None).unwrap(),
    };
    let catalogue = service(pool, vec![provider(stub)]);
    let context = context();
    let cases = [
        (None, Some(window(1, 0)), Some(window(1, 0)), 0usize),
        (Some(window(1, 0)), None, Some(window(1, 0)), 1usize),
        (Some(window(1, 0)), Some(window(1, 0)), None, 2usize),
    ];

    for (tracks, albums, artists, omitted) in cases {
        let before = [
            track_calls.load(Ordering::SeqCst),
            album_calls.load(Ordering::SeqCst),
            artist_calls.load(Ordering::SeqCst),
        ];
        let request = SearchRequest::new(
            SearchQuery::new("unused".to_owned()).unwrap(),
            SearchWindows::new(tracks, albums, artists),
            ArtistListFilter::default(),
        );
        catalogue.search(&request, None, &context).await.unwrap();
        let after = [
            track_calls.load(Ordering::SeqCst),
            album_calls.load(Ordering::SeqCst),
            artist_calls.load(Ordering::SeqCst),
        ];
        assert_eq!(after[omitted], before[omitted]);
        for kind in 0..after.len() {
            if kind != omitted {
                assert!(after[kind] > before[kind]);
            }
        }
    }

    let no_kinds = SearchRequest::new(
        SearchQuery::new("unused".to_owned()).unwrap(),
        SearchWindows::new(None, None, None),
        ArtistListFilter::default(),
    );
    let before = [
        track_calls.load(Ordering::SeqCst),
        album_calls.load(Ordering::SeqCst),
        artist_calls.load(Ordering::SeqCst),
    ];
    catalogue.search(&no_kinds, None, &context).await.unwrap();
    assert_eq!(
        [
            track_calls.load(Ordering::SeqCst),
            album_calls.load(Ordering::SeqCst),
            artist_calls.load(Ordering::SeqCst),
        ],
        before
    );
}

fn provider_source_for_test(external_id: &str) -> SourceIdentity {
    SourceIdentity::new(
        ProviderId::new("youtube_music".to_owned()).unwrap(),
        SourceScope::Global,
        external_id.to_owned(),
    )
    .unwrap()
}

#[sqlx::test]
async fn provider_materialization_joins_the_library_artist_album_and_track(pool: sqlx::PgPool) {
    let repository = Arc::new(PostgresTrackRepository::new(pool.clone()));
    let library = repository
        .get_or_create(
            local_source_at("glue-song.flac"),
            TrackMetadata::new(
                "Glue Song".to_owned(),
                vec![
                    ArtistCredit::new("beabadoobee".to_owned(), Some("mb-bea".to_owned())).unwrap(),
                ],
                Some(180_000),
            )
            .unwrap(),
        )
        .await
        .unwrap();
    let library_artist_id: Uuid = sqlx::query_scalar(
        "SELECT artist_id FROM track_artist_credits WHERE track_id = $1 AND position = 0",
    )
    .bind(library.id().as_uuid())
    .fetch_one(&pool)
    .await
    .unwrap();
    let mut library_albums = Vec::new();
    let mut library_memberships = Vec::new();
    for (index, title) in [
        "Beatopia",
        "Fake It Flowers",
        "This Is How Tomorrow Moves",
        "Switchblade",
    ]
    .into_iter()
    .enumerate()
    {
        let album = repository
            .resolve_or_create_album(
                &AlbumMetadata::new(title.to_owned())
                    .unwrap()
                    .with_album_artist(
                        ArtistCredit::new("beabadoobee".to_owned(), Some("mb-bea".to_owned()))
                            .unwrap(),
                    ),
            )
            .await
            .unwrap();
        let album_track = if index == 0 {
            library.id().clone()
        } else {
            repository
                .get_or_create(
                    local_source_at(&format!("{title}.flac")),
                    TrackMetadata::new(
                        format!("{title} track"),
                        vec![
                            ArtistCredit::new("beabadoobee".to_owned(), Some("mb-bea".to_owned()))
                                .unwrap(),
                        ],
                        Some(180_000),
                    )
                    .unwrap(),
                )
                .await
                .unwrap()
                .id()
                .clone()
        };
        repository
            .set_album_track(
                album.id(),
                &album_track,
                &crate::features::music_library::domain::AlbumTrack::unplaced(),
            )
            .await
            .unwrap();
        library_memberships.push((album.id().clone(), album_track));
        library_albums.push(album);
    }
    let artwork_id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO artwork_assets (id, subject_kind, subject_id, role, source_provider_id, source_external_id, revision, variant, content_digest, content_type, width, height, byte_size, file_name, fetched_at, last_used_at) VALUES ($1, 'artist', $2, 'wide', 'local', 'legacy-art', 1, 'original', repeat('a', 64), 'image/jpeg', 1, 1, 1, 'legacy.jpg', CURRENT_TIMESTAMP, CURRENT_TIMESTAMP)",
    )
    .bind(artwork_id)
    .bind(library_artist_id)
    .execute(&pool)
    .await
    .unwrap();
    let video_source = provider_source_for_test("video-glue-song");
    let mut provider_candidates = Vec::new();
    for (index, library_album) in library_albums.iter().enumerate() {
        let album_title = library_album.title().to_owned();
        let track_title = if index == 0 {
            "Glue Song".to_owned()
        } else {
            format!("{album_title} track")
        };
        let artist_source = provider_source_for_test(&format!("artist-bea-{index}"));
        let album_external_id = if index == 0 {
            "album-beatopia".to_owned()
        } else {
            format!("album-{index}")
        };
        let album_source = provider_source_for_test(&album_external_id);
        let credit = ArtistCredit::new("beabadoobee".to_owned(), None)
            .unwrap()
            .with_provider(Some(artist_source));
        let duration = if index == 0 { 181_000 } else { 180_000 };
        let metadata =
            TrackMetadata::new(track_title, vec![credit.clone()], Some(duration)).unwrap();
        let album = AlbumMetadata::new(album_title)
            .unwrap()
            .with_album_artist(credit)
            .with_provider(Some(album_source));
        let source = if index == 0 {
            video_source.clone()
        } else {
            provider_source_for_test(&format!("video-album-{index}"))
        };
        provider_candidates.push(CatalogueCandidate::new(source, metadata).with_album(Some(album)));
    }
    let catalogue = Fixture {
        store: repository.clone(),
    }
    .service(vec![provider(StubProvider::new(vec![Ok(
        provider_candidates,
    )]))]);
    let request = SearchRequest::uniform(
        SearchQuery::new("beabadoobee".to_owned()).unwrap(),
        window(10, 0),
    );

    catalogue.search(&request, None, &context()).await.unwrap();

    assert_eq!(
        repository.find_sources(library.id()).await.unwrap(),
        vec![local_source_at("glue-song.flac"), video_source]
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM artists")
            .fetch_one(&pool)
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM albums")
            .fetch_one(&pool)
            .await
            .unwrap(),
        4
    );
    for (index, library_album) in library_albums.iter().enumerate() {
        let source_id = if index == 0 {
            "album-beatopia".to_owned()
        } else {
            format!("album-{index}")
        };
        assert_eq!(
            sqlx::query_scalar::<_, Uuid>(
                "SELECT album_id FROM album_sources WHERE external_id = $1",
            )
            .bind(source_id)
            .fetch_one(&pool)
            .await
            .unwrap(),
            *library_album.id().as_uuid()
        );
    }
    let memberships = sqlx::query_as::<_, (Uuid, Uuid)>(
        "SELECT album_id, track_id FROM album_tracks ORDER BY album_id",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    let mut expected_memberships = library_memberships
        .iter()
        .map(|(album_id, track_id)| (*album_id.as_uuid(), *track_id.as_uuid()))
        .collect::<Vec<_>>();
    expected_memberships.sort();
    assert_eq!(memberships, expected_memberships);
    assert_eq!(
        sqlx::query_scalar::<_, Uuid>("SELECT id FROM artwork_assets WHERE id = $1")
            .bind(artwork_id)
            .fetch_one(&pool)
            .await
            .unwrap(),
        artwork_id
    );
}

#[sqlx::test]
async fn a_matched_track_joins_the_provider_album_too(pool: sqlx::PgPool) {
    let repository = Arc::new(PostgresTrackRepository::new(pool.clone()));
    let library_track = repository
        .get_or_create(
            local_source_at("care.flac"),
            TrackMetadata::from_names(
                "Care".to_owned(),
                vec!["beabadoobee".to_owned()],
                Some(200_000),
            )
            .unwrap(),
        )
        .await
        .unwrap();
    let artist = ArtistCredit::new("beabadoobee".to_owned(), None).unwrap();
    let library_album = repository
        .resolve_or_create_album(
            &AlbumMetadata::new("Fake It Flowers".to_owned())
                .unwrap()
                .with_album_artist(artist.clone()),
        )
        .await
        .unwrap();
    repository
        .set_album_track(
            library_album.id(),
            library_track.id(),
            &AlbumTrack::unplaced(),
        )
        .await
        .unwrap();

    let candidate = CatalogueCandidate::new(
        provider_source_for_test("video-care"),
        TrackMetadata::from_names(
            "Care".to_owned(),
            vec!["beabadoobee".to_owned()],
            Some(200_000),
        )
        .unwrap(),
    )
    .with_album(Some(
        AlbumMetadata::new("Care".to_owned())
            .unwrap()
            .with_album_artist(artist)
            .with_provider(Some(provider_source_for_test("MPREb_care"))),
    ));

    let write = Arc::clone(&repository) as Arc<dyn CatalogueWrite>;
    let lookup = Arc::clone(&repository) as Arc<dyn SourceLookup>;
    let hits = super::super::materialize::materialize(&write, &lookup, vec![candidate])
        .await
        .unwrap();

    let provider_album_id = hits[0].album().expect("the single is materialized").id();
    assert_ne!(provider_album_id, library_album.id());
    assert_eq!(hits[0].track().id(), library_track.id());

    let mut memberships = sqlx::query_as::<_, (Uuid, Uuid)>(
        "SELECT album_id, track_id FROM album_tracks WHERE track_id = $1",
    )
    .bind(library_track.id().as_uuid())
    .fetch_all(&pool)
    .await
    .unwrap();
    memberships.sort();
    let mut expected = vec![
        (*library_album.id().as_uuid(), *library_track.id().as_uuid()),
        (*provider_album_id.as_uuid(), *library_track.id().as_uuid()),
    ];
    expected.sort();
    assert_eq!(memberships, expected);
    assert_eq!(
        repository.find_sources(library_track.id()).await.unwrap(),
        vec![
            local_source_at("care.flac"),
            provider_source_for_test("video-care")
        ]
    );
}

#[sqlx::test]
async fn a_matched_track_without_an_incoming_album_keeps_its_memberships(pool: sqlx::PgPool) {
    let repository = Arc::new(PostgresTrackRepository::new(pool.clone()));
    let library_track = repository
        .get_or_create(
            local_source_at("care.flac"),
            TrackMetadata::from_names(
                "Care".to_owned(),
                vec!["beabadoobee".to_owned()],
                Some(200_000),
            )
            .unwrap(),
        )
        .await
        .unwrap();
    let library_album = repository
        .resolve_or_create_album(
            &AlbumMetadata::new("Fake It Flowers".to_owned())
                .unwrap()
                .with_album_artist(ArtistCredit::new("beabadoobee".to_owned(), None).unwrap()),
        )
        .await
        .unwrap();
    repository
        .set_album_track(
            library_album.id(),
            library_track.id(),
            &AlbumTrack::unplaced(),
        )
        .await
        .unwrap();

    let candidate = CatalogueCandidate::new(
        provider_source_for_test("video-care"),
        TrackMetadata::from_names(
            "Care".to_owned(),
            vec!["beabadoobee".to_owned()],
            Some(200_000),
        )
        .unwrap(),
    );

    let write = Arc::clone(&repository) as Arc<dyn CatalogueWrite>;
    let lookup = Arc::clone(&repository) as Arc<dyn SourceLookup>;
    let hits = super::super::materialize::materialize(&write, &lookup, vec![candidate])
        .await
        .unwrap();

    assert_eq!(hits[0].track().id(), library_track.id());
    assert!(hits[0].album().is_none());
    assert_eq!(
        sqlx::query_scalar::<_, Uuid>("SELECT album_id FROM album_tracks WHERE track_id = $1")
            .bind(library_track.id().as_uuid())
            .fetch_all(&pool)
            .await
            .unwrap(),
        vec![*library_album.id().as_uuid()]
    );
    assert_eq!(
        repository.find_sources(library_track.id()).await.unwrap(),
        vec![
            local_source_at("care.flac"),
            provider_source_for_test("video-care")
        ]
    );
}

#[sqlx::test]
async fn identified_provider_candidates_keep_the_existing_album_membership_rules(
    pool: sqlx::PgPool,
) {
    let repository = Arc::new(PostgresTrackRepository::new(pool.clone()));
    let with_album = repository
        .get_or_create(
            local_source_at("identified-with-album.flac"),
            TrackMetadata::new(
                "Identified With Album".to_owned(),
                vec![ArtistCredit::new("Artist".to_owned(), None).unwrap()],
                Some(180_000),
            )
            .unwrap()
            .with_recording_id(Some("recording-with-album".to_owned()))
            .unwrap(),
        )
        .await
        .unwrap();
    let without_album = repository
        .get_or_create(
            local_source_at("identified-without-album.flac"),
            TrackMetadata::new(
                "Identified Without Album".to_owned(),
                vec![ArtistCredit::new("Artist".to_owned(), None).unwrap()],
                Some(180_000),
            )
            .unwrap()
            .with_isrc(Some("ISRC-KEEP-RULE".to_owned()))
            .unwrap(),
        )
        .await
        .unwrap();
    let previous_album = repository
        .resolve_or_create_album(&AlbumMetadata::new("Previous".to_owned()).unwrap())
        .await
        .unwrap();
    repository
        .set_album_track(
            previous_album.id(),
            without_album.id(),
            &crate::features::music_library::domain::AlbumTrack::unplaced(),
        )
        .await
        .unwrap();

    let with_album_candidate = CatalogueCandidate::new(
        provider_source_for_test("identified-with-album-video"),
        TrackMetadata::new(
            "Identified With Album".to_owned(),
            vec![ArtistCredit::new("Artist".to_owned(), None).unwrap()],
            Some(180_000),
        )
        .unwrap()
        .with_recording_id(Some("recording-with-album".to_owned()))
        .unwrap(),
    )
    .with_album(Some(
        AlbumMetadata::new("Incoming".to_owned())
            .unwrap()
            .with_album_artist(ArtistCredit::new("Artist".to_owned(), None).unwrap()),
    ));
    let without_album_candidate = CatalogueCandidate::new(
        provider_source_for_test("identified-without-album-video"),
        TrackMetadata::new(
            "Identified Without Album".to_owned(),
            vec![ArtistCredit::new("Artist".to_owned(), None).unwrap()],
            Some(180_000),
        )
        .unwrap()
        .with_isrc(Some("ISRC-KEEP-RULE".to_owned()))
        .unwrap(),
    );
    let catalogue = Fixture {
        store: repository.clone(),
    }
    .service(vec![provider(StubProvider::new(vec![Ok(vec![
        with_album_candidate,
        without_album_candidate,
    ])]))]);
    let request = SearchRequest::uniform(
        SearchQuery::new("Identified".to_owned()).unwrap(),
        window(10, 0),
    );

    catalogue.search(&request, None, &context()).await.unwrap();

    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM album_tracks WHERE track_id = $1")
            .bind(with_album.id().as_uuid())
            .fetch_one(&pool)
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM album_tracks WHERE track_id = $1")
            .bind(without_album.id().as_uuid())
            .fetch_one(&pool)
            .await
            .unwrap(),
        0
    );
}

#[sqlx::test]
async fn a_provider_album_is_registered_with_its_source(pool: sqlx::PgPool) {
    let repository = Arc::new(PostgresTrackRepository::new(pool.clone()));
    let base = candidate("ytm-album-track", "Song");
    let source = base.source().clone();
    let album_metadata = AlbumMetadata::new("Discovery".to_owned())
        .unwrap()
        .with_album_artist(ArtistCredit::new("Daft Punk".to_owned(), None).unwrap())
        .with_provider(Some(source.clone()));
    let candidate = CatalogueCandidate::new(source, base.metadata().clone())
        .with_album(Some(album_metadata.clone()));
    let resolver = StubProvider::new(vec![Ok(vec![candidate])]);
    let catalogue = Fixture {
        store: repository.clone(),
    }
    .service(vec![provider(resolver)]);

    let outcome = catalogue.search(&search(), None, &context()).await.unwrap();
    assert_eq!(outcome.tracks.len(), 1);

    let album = repository
        .resolve_or_create_album(&album_metadata)
        .await
        .unwrap();
    let sources = repository.find_album_sources(album.id()).await.unwrap();
    assert_eq!(sources.len(), 1);
    assert_eq!(sources[0].provider_id().as_str(), "youtube_music");

    let entries = repository
        .find_album_entries(album.id(), None)
        .await
        .unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].track().id(), outcome.tracks[0].track().id());
}

#[sqlx::test]
async fn provider_album_and_artist_identity_round_trip_through_materialize(pool: sqlx::PgPool) {
    fn provider_source(external_id: &str) -> SourceIdentity {
        SourceIdentity::new(
            ProviderId::new("youtube_music".to_owned()).unwrap(),
            SourceScope::Global,
            external_id.to_owned(),
        )
        .unwrap()
    }

    let artist = ArtistCredit::new("Olivia Rodrigo".to_owned(), None)
        .unwrap()
        .with_provider(Some(provider_source("artist-1")));
    let metadata = TrackMetadata::new("brutal".to_owned(), vec![artist], Some(144_000)).unwrap();
    let album = AlbumMetadata::new("SOUR".to_owned())
        .unwrap()
        .with_provider(Some(provider_source("album-1")))
        .with_year(Some(2021))
        .unwrap()
        .with_release_type(Some("Album".to_owned()))
        .unwrap();
    let candidate =
        CatalogueCandidate::new(provider_source("video-1"), metadata).with_album(Some(album));
    let renamed_artist = ArtistCredit::new("Olivia Rodrigo (renamed)".to_owned(), None)
        .unwrap()
        .with_provider(Some(provider_source("artist-1")));
    let renamed_metadata = TrackMetadata::new(
        "brutal (renamed)".to_owned(),
        vec![renamed_artist.clone()],
        Some(144_000),
    )
    .unwrap();
    let renamed_album = AlbumMetadata::new("SOUR (renamed)".to_owned())
        .unwrap()
        .with_provider(Some(provider_source("album-1")))
        .with_album_artist(renamed_artist);
    let renamed_candidate = CatalogueCandidate::new(provider_source("video-1"), renamed_metadata)
        .with_album(Some(renamed_album));

    let repository = Arc::new(PostgresTrackRepository::new(pool.clone()));
    let resolver = StubProvider::new(vec![Ok(vec![candidate]), Ok(vec![renamed_candidate])]);
    let catalogue = Fixture {
        store: repository.clone(),
    }
    .service(vec![provider(resolver)]);

    catalogue.search(&search(), None, &context()).await.unwrap();
    let original_album = repository
        .find_album_by_source(&provider_source("album-1"))
        .await
        .unwrap()
        .unwrap();
    let original_artist = repository
        .find_artist_by_source(&provider_source("artist-1"))
        .await
        .unwrap()
        .unwrap();

    catalogue.search(&search(), None, &context()).await.unwrap();

    let renamed_album = repository
        .find_album_by_source(&provider_source("album-1"))
        .await
        .unwrap()
        .unwrap();
    let renamed_artist = repository
        .find_artist_by_source(&provider_source("artist-1"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(original_album.id(), renamed_album.id());
    assert_eq!(original_artist.id(), renamed_artist.id());
    assert_eq!(
        repository
            .find_album_sources(original_album.id())
            .await
            .unwrap(),
        vec![provider_source("album-1")],
        "materialize stores the album's own identity, not the song's"
    );
    assert_eq!(
        repository
            .find_artist_sources(original_artist.id())
            .await
            .unwrap(),
        vec![provider_source("artist-1")],
        "materialize stores the artist's provider identity"
    );
}

fn local_source() -> SourceIdentity {
    local_source_at("path-a")
}

fn local_source_at(path: &str) -> SourceIdentity {
    SourceIdentity::new(
        ProviderId::new("local".to_owned()).unwrap(),
        SourceScope::Named("/music".to_owned()),
        path.to_owned(),
    )
    .unwrap()
}

fn named_metadata(title: &str) -> TrackMetadata {
    TrackMetadata::from_names(title.to_owned(), vec!["Daft Punk".to_owned()], Some(1_000)).unwrap()
}

#[sqlx::test]
async fn a_library_without_providers_still_searches(pool: sqlx::PgPool) {
    let repository = Arc::new(PostgresTrackRepository::new(pool.clone()));
    repository
        .get_or_create(local_source(), named_metadata("Zebra"))
        .await
        .unwrap();
    let catalogue = Fixture { store: repository }.bare();

    let outcome = catalogue.search(&search(), None, &context()).await.unwrap();

    assert_eq!(outcome.tracks.len(), 1);
    assert_eq!(outcome.tracks[0].track().title(), "Zebra");
    assert!(is_owned(outcome.tracks[0].sources()));
}

#[sqlx::test]
async fn a_library_without_providers_answers_a_miss_with_nothing(pool: sqlx::PgPool) {
    let catalogue = Fixture::new(pool.clone()).bare();

    let outcome = catalogue.search(&search(), None, &context()).await.unwrap();

    assert!(outcome.tracks.is_empty());
}

#[sqlx::test]
async fn owned_tracks_outrank_fresh_provider_discoveries(pool: sqlx::PgPool) {
    let repository = Arc::new(PostgresTrackRepository::new(pool.clone()));
    repository
        .get_or_create(local_source(), named_metadata("Zebra"))
        .await
        .unwrap();
    let resolver = StubProvider::new(vec![Ok(vec![candidate("vid-1", "Apple")])]);
    let catalogue = Fixture { store: repository }.service(vec![provider(resolver)]);

    let outcome = catalogue.search(&search(), None, &context()).await.unwrap();

    let titles = outcome
        .tracks
        .iter()
        .map(|hit| hit.track().title().to_owned())
        .collect::<Vec<_>>();
    assert_eq!(titles, ["Zebra", "Apple"]);
    assert!(is_owned(outcome.tracks[0].sources()));
    assert!(!is_owned(outcome.tracks[1].sources()));
}

#[sqlx::test]
async fn a_strong_library_match_is_reported_as_strong(pool: sqlx::PgPool) {
    let repository = Arc::new(PostgresTrackRepository::new(pool.clone()));
    repository
        .get_or_create(local_source(), named_metadata("Zebra"))
        .await
        .unwrap();
    let catalogue = Fixture { store: repository }.bare();
    let search =
        SearchRequest::uniform(SearchQuery::new("zebra".to_owned()).unwrap(), window(10, 0));

    let outcome = catalogue.search(&search, None, &context()).await.unwrap();

    assert_eq!(outcome.confidence, SearchConfidence::Strong);
    assert_eq!(outcome.tracks[0].match_quality().unwrap().as_str(), "exact");
}

#[sqlx::test]
async fn a_stopword_query_is_weak_confidence(pool: sqlx::PgPool) {
    let repository = Arc::new(PostgresTrackRepository::new(pool.clone()));
    repository
        .get_or_create(local_source(), named_metadata("The End"))
        .await
        .unwrap();
    let catalogue = Fixture { store: repository }.bare();
    let search = SearchRequest::uniform(SearchQuery::new("the".to_owned()).unwrap(), window(10, 0));

    let outcome = catalogue.search(&search, None, &context()).await.unwrap();

    assert_eq!(outcome.tracks.len(), 1);
    assert_eq!(outcome.confidence, SearchConfidence::Weak);
}

#[sqlx::test]
async fn a_weak_query_puts_provider_discoveries_first(pool: sqlx::PgPool) {
    let repository = Arc::new(PostgresTrackRepository::new(pool.clone()));
    repository
        .get_or_create(local_source(), named_metadata("The End"))
        .await
        .unwrap();
    let resolver = StubProvider::new(vec![Ok(vec![candidate("vid-1", "Provider Song")])]);
    let catalogue = Fixture { store: repository }.service(vec![provider(resolver)]);
    let search = SearchRequest::uniform(SearchQuery::new("the".to_owned()).unwrap(), window(10, 0));

    let outcome = catalogue.search(&search, None, &context()).await.unwrap();

    assert_eq!(outcome.confidence, SearchConfidence::Weak);
    assert!(outcome.tracks[0].match_quality().is_none());
    assert!(outcome.tracks[0].sources()[0].provider_id().as_str() == "youtube_music");
}

#[sqlx::test]
async fn the_library_cannot_exhaust_the_page(pool: sqlx::PgPool) {
    let repository = Arc::new(PostgresTrackRepository::new(pool.clone()));
    for index in 0..10u32 {
        repository
            .get_or_create(
                local_source_at(&format!("path-{index}")),
                named_metadata(&format!("Library {index}")),
            )
            .await
            .unwrap();
    }
    let resolver = StubProvider::new(vec![Ok((0..10)
        .map(|index| candidate(&format!("p-{index}"), &format!("Provider {index}")))
        .collect())]);
    let catalogue = Fixture { store: repository }.service(vec![provider(resolver)]);

    let outcome = catalogue.search(&search(), None, &context()).await.unwrap();

    assert_eq!(outcome.tracks.len(), 10);
    assert_eq!(outcome.confidence, SearchConfidence::Strong);
    let provider_hits = outcome
        .tracks
        .iter()
        .filter(|hit| hit.match_quality().is_none())
        .count();
    assert!(
        provider_hits >= 3,
        "the library left room for provider discoveries, found {provider_hits}"
    );
}

#[sqlx::test]
async fn the_library_fills_the_page_when_providers_return_nothing(pool: sqlx::PgPool) {
    let repository = Arc::new(PostgresTrackRepository::new(pool.clone()));
    for index in 0..10u32 {
        repository
            .get_or_create(
                local_source_at(&format!("path-{index}")),
                named_metadata(&format!("Library {index}")),
            )
            .await
            .unwrap();
    }
    let resolver = StubProvider::new(vec![Ok(Vec::new())]);
    let catalogue = Fixture { store: repository }.service(vec![provider(resolver)]);

    let outcome = catalogue.search(&search(), None, &context()).await.unwrap();

    assert_eq!(outcome.tracks.len(), 10);
    assert!(
        outcome
            .tracks
            .iter()
            .all(|hit| hit.match_quality().is_some())
    );
}

#[sqlx::test]
async fn the_mux_runs_only_requested_kinds(pool: sqlx::PgPool) {
    let repository = Arc::new(PostgresTrackRepository::new(pool.clone()));
    let album = repository
        .resolve_or_create_album(
            &AlbumMetadata::new("Daft Punk Album".to_owned())
                .unwrap()
                .with_album_artist(ArtistCredit::new("Daft Punk".to_owned(), None).unwrap()),
        )
        .await
        .unwrap();
    let track = repository
        .get_or_create(local_source(), named_metadata("Daft Punk Song"))
        .await
        .unwrap();
    repository
        .set_album_track(album.id(), track.id(), &AlbumTrack::unplaced())
        .await
        .unwrap();

    let limits = StdArc::new(Mutex::new(Vec::new()));
    let catalogue = Fixture { store: repository }.service(vec![provider(LimitRecordingProvider {
        limit: StdArc::clone(&limits),
    })]);
    let query = SearchQuery::new("Daft Punk".to_owned()).unwrap();

    let albums_and_artists = SearchRequest::new(
        query.clone(),
        SearchWindows::new(None, Some(window(10, 0)), Some(window(10, 0))),
        ArtistListFilter::default(),
    );
    let outcome = catalogue
        .search(&albums_and_artists, None, &context())
        .await
        .unwrap();
    assert!(outcome.tracks.is_empty());
    assert_eq!(outcome.confidence, SearchConfidence::None);
    assert_eq!(outcome.albums.len(), 1);
    assert_eq!(outcome.artists.len(), 1);
    assert!(
        limits.lock().is_empty(),
        "a suppressed track kind makes no provider call"
    );

    let tracks_only = SearchRequest::new(
        query.clone(),
        SearchWindows::new(Some(window(10, 0)), None, None),
        ArtistListFilter::default(),
    );
    catalogue
        .search(&tracks_only, None, &context())
        .await
        .unwrap();
    assert_eq!(
        limits.lock().len(),
        1,
        "the track kind calls the provider once"
    );

    let nothing = SearchRequest::new(
        query.clone(),
        SearchWindows::new(None, None, None),
        ArtistListFilter::default(),
    );
    let outcome = catalogue.search(&nothing, None, &context()).await.unwrap();
    assert!(outcome.tracks.is_empty());
    assert!(outcome.albums.is_empty());
    assert!(outcome.artists.is_empty());
    assert_eq!(outcome.confidence, SearchConfidence::None);
    assert_eq!(
        limits.lock().len(),
        1,
        "no requested kind makes no provider call"
    );
}

#[sqlx::test]
async fn a_failing_kind_degrades_when_another_kind_answers(pool: sqlx::PgPool) {
    let repository = Arc::new(PostgresTrackRepository::new(pool.clone()));
    repository
        .resolve_or_create_album(
            &AlbumMetadata::new("Answer Album".to_owned())
                .unwrap()
                .with_album_artist(ArtistCredit::new("Answer Artist".to_owned(), None).unwrap()),
        )
        .await
        .unwrap();
    let catalogue =
        Fixture { store: repository }.service(vec![provider(StubProvider::new(vec![Err(
            ProviderError::ProviderUnavailable,
        )]))]);

    let request = SearchRequest::new(
        SearchQuery::new("answer".to_owned()).unwrap(),
        SearchWindows::new(Some(window(10, 0)), Some(window(10, 0)), None),
        ArtistListFilter::default(),
    );
    let outcome = catalogue.search(&request, None, &context()).await.unwrap();

    assert!(outcome.tracks.is_empty());
    assert_eq!(outcome.albums.len(), 1);
    assert_eq!(outcome.confidence, SearchConfidence::None);
}
#[test]
fn the_default_budget_is_unchanged() {
    let store = Arc::new(FailingTrackRepository);
    let bare = SearchService::new(
        Arc::clone(&store) as Arc<dyn LibraryRead>,
        Arc::clone(&store) as Arc<dyn CatalogueWrite>,
        Arc::clone(&store) as Arc<dyn SourceLookup>,
        Arc::clone(&store) as Arc<dyn StateStore>,
    );
    assert_eq!(bare.dedup_budget, Duration::from_millis(50));
    let with_providers = SearchService::with_providers(
        Arc::clone(&store) as Arc<dyn LibraryRead>,
        Arc::clone(&store) as Arc<dyn CatalogueWrite>,
        Arc::clone(&store) as Arc<dyn SourceLookup>,
        Arc::clone(&store) as Arc<dyn StateStore>,
        Vec::new(),
    );
    assert_eq!(with_providers.dedup_budget, Duration::from_millis(50));
}

#[test]
fn the_default_search_budget_is_unchanged() {
    let store = Arc::new(FailingTrackRepository);
    let bare = SearchService::new(
        Arc::clone(&store) as Arc<dyn LibraryRead>,
        Arc::clone(&store) as Arc<dyn CatalogueWrite>,
        Arc::clone(&store) as Arc<dyn SourceLookup>,
        Arc::clone(&store) as Arc<dyn StateStore>,
    );
    assert_eq!(bare.search_budget, Duration::from_secs(45));
    let with_providers = SearchService::with_providers(
        Arc::clone(&store) as Arc<dyn LibraryRead>,
        Arc::clone(&store) as Arc<dyn CatalogueWrite>,
        Arc::clone(&store) as Arc<dyn SourceLookup>,
        Arc::clone(&store) as Arc<dyn StateStore>,
        Vec::new(),
    );
    assert_eq!(with_providers.search_budget, Duration::from_secs(45));
}

#[test]
fn the_default_response_budget_is_unchanged() {
    let store = Arc::new(FailingTrackRepository);
    let bare = SearchService::new(
        Arc::clone(&store) as Arc<dyn LibraryRead>,
        Arc::clone(&store) as Arc<dyn CatalogueWrite>,
        Arc::clone(&store) as Arc<dyn SourceLookup>,
        Arc::clone(&store) as Arc<dyn StateStore>,
    );
    assert_eq!(bare.response_budget, Duration::from_secs(3));
    let with_providers = SearchService::with_providers(
        Arc::clone(&store) as Arc<dyn LibraryRead>,
        Arc::clone(&store) as Arc<dyn CatalogueWrite>,
        Arc::clone(&store) as Arc<dyn SourceLookup>,
        Arc::clone(&store) as Arc<dyn StateStore>,
        Vec::new(),
    );
    assert_eq!(with_providers.response_budget, Duration::from_secs(3));
}

const RESPONSE_BUDGET: Duration = Duration::from_secs(3);
const PROVIDER_BUDGET: Duration = Duration::from_secs(45);
const HANG: Duration = Duration::from_secs(1);
const EDGE: Duration = Duration::from_millis(10);

async fn response_at_budget<T>(task: &mut tokio::task::JoinHandle<Result<T, CatalogueError>>) -> T {
    tokio::time::advance(RESPONSE_BUDGET - EDGE).await;
    assert!(
        tokio::time::timeout(Duration::from_millis(1), &mut *task)
            .await
            .is_err(),
        "the response returned before the response budget"
    );
    tokio::time::advance(EDGE - Duration::from_millis(1)).await;
    tokio::time::timeout(HANG, &mut *task)
        .await
        .expect("the response did not return at the response budget")
        .expect("the response task joins")
        .expect("the response is a success")
}

async fn wait_for_materialised(
    receiver: &mut mpsc::UnboundedReceiver<Materialised>,
) -> Materialised {
    tokio::time::timeout(Duration::from_secs(1), receiver.recv())
        .await
        .expect("materialisation before the hang deadline")
        .expect("a materialisation signal")
}

async fn wait_for_settled(receiver: &mut mpsc::UnboundedReceiver<()>) {
    tokio::time::timeout(Duration::from_secs(1), receiver.recv())
        .await
        .expect("settlement before the hang deadline")
        .expect("a settlement signal");
}

enum Materialised {
    Track(TrackId),
    Album,
    Artist,
}

struct MemoryStore {
    hits: Mutex<Vec<SearchHit>>,
    albums: Mutex<Vec<Album>>,
    artists: Mutex<Vec<Artist>>,
    materialized: mpsc::UnboundedSender<Materialised>,
    fail_tracks: AtomicBool,
    fail_albums: AtomicBool,
}

impl MemoryStore {
    fn new(materialized: mpsc::UnboundedSender<Materialised>) -> Self {
        Self {
            hits: Mutex::new(Vec::new()),
            albums: Mutex::new(Vec::new()),
            artists: Mutex::new(Vec::new()),
            materialized,
            fail_tracks: AtomicBool::new(false),
            fail_albums: AtomicBool::new(false),
        }
    }

    fn insert(&self, hit: SearchHit) {
        self.hits.lock().push(hit);
    }

    fn by_title(&self, title: &str) -> Vec<SearchHit> {
        self.hits
            .lock()
            .iter()
            .filter(|hit| hit.track().title() == title)
            .cloned()
            .collect()
    }

    fn album_titles(&self) -> Vec<String> {
        self.albums
            .lock()
            .iter()
            .map(|album| album.title().to_owned())
            .collect()
    }

    fn artist_names(&self) -> Vec<String> {
        self.artists
            .lock()
            .iter()
            .map(|artist| artist.name().to_owned())
            .collect()
    }

    fn fail_tracks(&self) {
        self.fail_tracks.store(true, Ordering::SeqCst);
    }

    fn fail_albums(&self) {
        self.fail_albums.store(true, Ordering::SeqCst);
    }

    fn store(
        &self,
        source: SourceIdentity,
        metadata: TrackMetadata,
    ) -> Result<Track, TrackRepositoryError> {
        if self.fail_tracks.load(Ordering::SeqCst) {
            return Err(TrackRepositoryError::Unavailable);
        }
        let track = Track::new(TrackId::new(), metadata);
        self.hits
            .lock()
            .push(SearchHit::new(track.clone(), None, vec![source]));
        let _ = self
            .materialized
            .send(Materialised::Track(track.id().clone()));
        Ok(track)
    }
}

#[async_trait]
impl CatalogueWrite for MemoryStore {
    async fn get_or_create(
        &self,
        source: SourceIdentity,
        metadata: TrackMetadata,
    ) -> Result<Track, TrackRepositoryError> {
        self.store(source, metadata)
    }

    async fn get_or_create_provider(
        &self,
        source: SourceIdentity,
        metadata: TrackMetadata,
    ) -> Result<Track, TrackRepositoryError> {
        self.store(source, metadata)
    }

    async fn resolve_or_create_album(
        &self,
        metadata: &AlbumMetadata,
    ) -> Result<Album, TrackRepositoryError> {
        if self.fail_albums.load(Ordering::SeqCst) {
            return Err(TrackRepositoryError::Unavailable);
        }
        let album = Album::new(
            AlbumId::new(),
            metadata.title().to_owned(),
            None,
            metadata.year(),
            metadata.release_type().map(str::to_owned),
            metadata.label().map(str::to_owned),
            metadata.musicbrainz_release_group_id().map(str::to_owned),
            metadata.musicbrainz_release_id().map(str::to_owned),
        )
        .map_err(|_| TrackRepositoryError::InvalidData)?;
        self.albums.lock().push(album.clone());
        let _ = self.materialized.send(Materialised::Album);
        Ok(album)
    }

    async fn resolve_or_create_artist(
        &self,
        credit: &ArtistCredit,
    ) -> Result<Artist, TrackRepositoryError> {
        let artist = Artist::new(
            ArtistId::new(),
            credit.name().to_owned(),
            None,
            credit.musicbrainz_artist_id().map(str::to_owned),
        )
        .map_err(|_| TrackRepositoryError::InvalidData)?;
        self.artists.lock().push(artist.clone());
        let _ = self.materialized.send(Materialised::Artist);
        Ok(artist)
    }

    async fn add_album_track(
        &self,
        _album_id: &AlbumId,
        _track_id: &TrackId,
        _membership: &AlbumTrack,
    ) -> Result<(), TrackRepositoryError> {
        Ok(())
    }
}

#[async_trait]
impl LibraryRead for MemoryStore {
    async fn find_track(&self, track_id: &TrackId) -> Result<Option<Track>, TrackRepositoryError> {
        Ok(self
            .hits
            .lock()
            .iter()
            .find(|hit| hit.track().id() == track_id)
            .map(|hit| hit.track().clone()))
    }

    async fn page_tracks(
        &self,
        _offset: u32,
        _limit: u32,
        _order: crate::features::music_library::domain::TrackOrder,
        _filter: &crate::features::music_library::domain::TrackListFilter,
        _user: Option<Uuid>,
    ) -> Result<crate::features::music_library::domain::TrackPage, TrackRepositoryError> {
        Err(TrackRepositoryError::Unavailable)
    }

    async fn track_views(
        &self,
        _track_ids: &[TrackId],
        _user: Option<Uuid>,
    ) -> Result<Vec<crate::features::music_library::domain::TrackView>, TrackRepositoryError> {
        Ok(Vec::new())
    }

    async fn search_tracks(
        &self,
        query: &str,
        limit: u32,
        _user: Option<Uuid>,
    ) -> Result<Vec<SearchHit>, TrackRepositoryError> {
        let needle = query.to_lowercase();
        Ok(self
            .hits
            .lock()
            .iter()
            .filter(|hit| {
                hit.track().title().to_lowercase().contains(&needle)
                    || hit
                        .track()
                        .artists()
                        .iter()
                        .any(|artist| artist.name().to_lowercase().contains(&needle))
            })
            .take(limit as usize)
            .cloned()
            .collect())
    }
}

#[async_trait]
impl SourceLookup for MemoryStore {
    async fn find_sources(
        &self,
        track_id: &TrackId,
    ) -> Result<Vec<SourceIdentity>, TrackRepositoryError> {
        Ok(self
            .hits
            .lock()
            .iter()
            .find(|hit| hit.track().id() == track_id)
            .map(|hit| hit.sources().to_vec())
            .unwrap_or_default())
    }
}

#[async_trait]
impl StateStore for MemoryStore {}

#[async_trait]
impl DuplicateReport for MemoryStore {}

fn owned_hit(title: &str, artist: &str) -> SearchHit {
    let track = Track::new(
        TrackId::new(),
        TrackMetadata::from_names(title.to_owned(), vec![artist.to_owned()], Some(1_000)).unwrap(),
    );
    SearchHit::new(track, None, vec![local_source_at("memory")])
}

fn album_metadata() -> AlbumMetadata {
    AlbumMetadata::new("Beatopia".to_owned())
        .unwrap()
        .with_provider(Some(provider_source_for_test("album-1")))
}

fn artist_credit() -> ArtistCredit {
    ArtistCredit::new("beabadoobee".to_owned(), None)
        .unwrap()
        .with_provider(Some(provider_source_for_test("artist-1")))
}

fn memory_service(
    store: &StdArc<MemoryStore>,
    providers: Vec<StdArc<dyn CatalogueProvider>>,
) -> SearchService {
    SearchService::with_providers(
        StdArc::clone(store) as StdArc<dyn LibraryRead>,
        StdArc::clone(store) as StdArc<dyn CatalogueWrite>,
        StdArc::clone(store) as StdArc<dyn SourceLookup>,
        StdArc::clone(store) as StdArc<dyn StateStore>,
        providers,
    )
}

fn held_service(
    store: &StdArc<MemoryStore>,
    providers: Vec<StdArc<dyn CatalogueProvider>>,
) -> SearchService {
    memory_service(store, providers).with_response_budget(RESPONSE_BUDGET)
}

fn track_search() -> SearchRequest {
    SearchRequest::new(
        SearchQuery::new("Daft Punk".to_owned()).unwrap(),
        SearchWindows::new(Some(window(10, 0)), None, None),
        ArtistListFilter::default(),
    )
}

struct Gate {
    started: mpsc::UnboundedSender<()>,
    finished: Option<mpsc::UnboundedSender<()>>,
    release: StdArc<Semaphore>,
    gated_calls: usize,
    calls: AtomicUsize,
    active: AtomicUsize,
    maximum: AtomicUsize,
}

struct GateCall(Option<mpsc::UnboundedSender<()>>);

impl Drop for GateCall {
    fn drop(&mut self) {
        if let Some(finished) = &self.0 {
            let _ = finished.send(());
        }
    }
}

impl Gate {
    fn new(
        started: mpsc::UnboundedSender<()>,
        release: StdArc<Semaphore>,
        gated_calls: usize,
    ) -> Self {
        Self {
            started,
            finished: None,
            release,
            gated_calls,
            calls: AtomicUsize::new(0),
            active: AtomicUsize::new(0),
            maximum: AtomicUsize::new(0),
        }
    }

    fn with_finished(mut self, finished: mpsc::UnboundedSender<()>) -> Self {
        self.finished = Some(finished);
        self
    }

    async fn enter(&self) {
        let _call = GateCall(self.finished.clone());
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
        self.maximum.fetch_max(active, Ordering::SeqCst);
        let _ = self.started.send(());
        if call < self.gated_calls {
            self.release.clone().acquire_owned().await.unwrap().forget();
        }
        self.active.fetch_sub(1, Ordering::SeqCst);
    }

    fn call_count(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }

    fn maximum(&self) -> usize {
        self.maximum.load(Ordering::SeqCst)
    }
}

struct HeldTrackProvider {
    gate: StdArc<Gate>,
    responses: Mutex<VecDeque<Result<Vec<CatalogueCandidate>, ProviderError>>>,
    settled: Option<mpsc::UnboundedSender<()>>,
}

impl HeldTrackProvider {
    fn new(
        gate: StdArc<Gate>,
        responses: Vec<Result<Vec<CatalogueCandidate>, ProviderError>>,
    ) -> Self {
        Self {
            gate,
            responses: Mutex::new(responses.into()),
            settled: None,
        }
    }

    fn with_settled(mut self, settled: mpsc::UnboundedSender<()>) -> Self {
        self.settled = Some(settled);
        self
    }
}

#[async_trait]
impl CatalogueProvider for HeldTrackProvider {
    async fn search_tracks(
        &self,
        _search: &CatalogueSearch,
        _context: &RequestContext,
    ) -> Result<Vec<CatalogueCandidate>, ProviderError> {
        self.gate.enter().await;
        let result = self.responses.lock().pop_front().unwrap_or(Ok(Vec::new()));
        if let Some(settled) = &self.settled {
            let _ = settled.send(());
        }
        result
    }
}

struct HeldAlbumProvider {
    gate: StdArc<Gate>,
    responses: Mutex<VecDeque<Result<CataloguePage<AlbumMetadata>, ProviderError>>>,
    settled: Option<mpsc::UnboundedSender<()>>,
}

impl HeldAlbumProvider {
    fn new(
        gate: StdArc<Gate>,
        responses: Vec<Result<CataloguePage<AlbumMetadata>, ProviderError>>,
    ) -> Self {
        Self {
            gate,
            responses: Mutex::new(responses.into()),
            settled: None,
        }
    }

    fn with_settled(mut self, settled: mpsc::UnboundedSender<()>) -> Self {
        self.settled = Some(settled);
        self
    }
}

#[async_trait]
impl CatalogueProvider for HeldAlbumProvider {
    async fn search_tracks(
        &self,
        _search: &CatalogueSearch,
        _context: &RequestContext,
    ) -> Result<Vec<CatalogueCandidate>, ProviderError> {
        Ok(Vec::new())
    }

    async fn search_albums(
        &self,
        _search: &CatalogueSearch,
        _context: &RequestContext,
    ) -> Result<CataloguePage<AlbumMetadata>, ProviderError> {
        self.gate.enter().await;
        let result = self
            .responses
            .lock()
            .pop_front()
            .unwrap_or(Ok(CataloguePage::new(Vec::new(), None)));
        if let Some(settled) = &self.settled {
            let _ = settled.send(());
        }
        result
    }
}

struct HeldArtistProvider {
    gate: StdArc<Gate>,
    responses: Mutex<VecDeque<Result<CataloguePage<ArtistCredit>, ProviderError>>>,
    settled: Option<mpsc::UnboundedSender<()>>,
}

impl HeldArtistProvider {
    fn new(
        gate: StdArc<Gate>,
        responses: Vec<Result<CataloguePage<ArtistCredit>, ProviderError>>,
    ) -> Self {
        Self {
            gate,
            responses: Mutex::new(responses.into()),
            settled: None,
        }
    }
}

#[async_trait]
impl CatalogueProvider for HeldArtistProvider {
    async fn search_tracks(
        &self,
        _search: &CatalogueSearch,
        _context: &RequestContext,
    ) -> Result<Vec<CatalogueCandidate>, ProviderError> {
        Ok(Vec::new())
    }

    async fn search_artists(
        &self,
        _search: &CatalogueSearch,
        _context: &RequestContext,
    ) -> Result<CataloguePage<ArtistCredit>, ProviderError> {
        self.gate.enter().await;
        let result = self
            .responses
            .lock()
            .pop_front()
            .unwrap_or(Ok(CataloguePage::new(Vec::new(), None)));
        if let Some(settled) = &self.settled {
            let _ = settled.send(());
        }
        result
    }
}

#[derive(Clone, Default)]
struct LogCapture {
    entries: StdArc<Mutex<Vec<String>>>,
    recorded: StdArc<tokio::sync::Notify>,
}

impl LogCapture {
    fn entries(&self) -> Vec<String> {
        self.entries.lock().clone()
    }

    fn entries_matching(&self, needle: &str) -> Vec<String> {
        self.entries()
            .into_iter()
            .filter(|entry| entry.contains(needle))
            .collect()
    }

    async fn recorded(&self, needle: &str) {
        loop {
            let notified = self.recorded.notified();
            if !self.entries_matching(needle).is_empty() {
                return;
            }
            notified.await;
        }
    }
}

struct FieldVisitor {
    entries: Vec<String>,
}

impl Visit for FieldVisitor {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        self.entries.push(format!("{}={:?}", field.name(), value));
    }
}

impl<S: tracing::Subscriber> Layer<S> for LogCapture {
    fn on_event(
        &self,
        event: &tracing::Event<'_>,
        _context: tracing_subscriber::layer::Context<'_, S>,
    ) {
        let mut visitor = FieldVisitor {
            entries: Vec::new(),
        };
        event.record(&mut visitor);
        self.entries.lock().push(format!(
            "{} {}",
            event.metadata().level(),
            visitor.entries.join(" ")
        ));
        self.recorded.notify_waiters();
    }
}

#[tokio::test(start_paused = true)]
async fn a_provider_inside_the_budget_is_unchanged() {
    let (materialized, _receiver) = mpsc::unbounded_channel();
    let store = StdArc::new(MemoryStore::new(materialized));
    let catalogue = memory_service(
        &store,
        vec![provider(StubProvider::new(vec![Ok(vec![candidate(
            "ytm-1", "YTM Song",
        )])]))],
    );

    let outcome = catalogue.search(&search(), None, &context()).await.unwrap();

    assert_eq!(outcome.tracks.len(), 1);
    assert_eq!(outcome.tracks[0].track().title(), "YTM Song");
}

#[tokio::test(start_paused = true)]
async fn a_held_provider_returns_library_results_at_the_budget() {
    let (materialized, mut materialized_rx) = mpsc::unbounded_channel();
    let store = StdArc::new(MemoryStore::new(materialized));
    store.insert(owned_hit("Zebra", "Daft Punk"));
    let (started_tx, mut started_rx) = mpsc::unbounded_channel();
    let release = StdArc::new(Semaphore::new(0));
    let gate = StdArc::new(Gate::new(started_tx, StdArc::clone(&release), 1));
    let held = HeldTrackProvider::new(gate, vec![Ok(vec![candidate("ytm-1", "YTM Song")])]);
    let catalogue = StdArc::new(held_service(&store, vec![provider(held)]));
    let request = track_search();
    let request_context = context();
    let service = StdArc::clone(&catalogue);
    let mut task =
        tokio::spawn(async move { service.search(&request, None, &request_context).await });
    wait_for_provider_start(&mut started_rx).await;

    let outcome = response_at_budget(&mut task).await;

    assert_eq!(outcome.tracks.len(), 1);
    assert_eq!(outcome.tracks[0].track().title(), "Zebra");
    assert!(is_owned(outcome.tracks[0].sources()));

    release.add_permits(1);
    assert!(matches!(
        wait_for_materialised(&mut materialized_rx).await,
        Materialised::Track(_)
    ));
}

#[tokio::test(start_paused = true)]
async fn a_held_provider_materialises_after_the_response() {
    let (materialized, mut materialized_rx) = mpsc::unbounded_channel();
    let store = StdArc::new(MemoryStore::new(materialized));
    let (started_tx, mut started_rx) = mpsc::unbounded_channel();
    let release = StdArc::new(Semaphore::new(0));
    let gate = StdArc::new(Gate::new(started_tx, StdArc::clone(&release), 1));
    let held = HeldTrackProvider::new(gate, vec![Ok(vec![candidate("ytm-1", "YTM Song")])]);
    let catalogue = StdArc::new(held_service(&store, vec![provider(held)]));
    let request = track_search();
    let request_context = context();
    let service = StdArc::clone(&catalogue);
    let mut task =
        tokio::spawn(async move { service.search(&request, None, &request_context).await });
    wait_for_provider_start(&mut started_rx).await;
    assert!(response_at_budget(&mut task).await.tracks.is_empty());

    release.add_permits(1);
    let stored = wait_for_materialised(&mut materialized_rx).await;

    let Materialised::Track(stored) = stored else {
        panic!("expected a materialised track");
    };
    let hits = store.by_title("YTM Song");
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].track().id(), &stored);
}

#[tokio::test(start_paused = true)]
async fn the_next_search_reads_background_results() {
    let (materialized, mut materialized_rx) = mpsc::unbounded_channel();
    let store = StdArc::new(MemoryStore::new(materialized));
    let (started_tx, mut started_rx) = mpsc::unbounded_channel();
    let release = StdArc::new(Semaphore::new(0));
    let gate = StdArc::new(Gate::new(started_tx, StdArc::clone(&release), 1));
    let held = HeldTrackProvider::new(
        gate,
        vec![Ok(vec![candidate("ytm-1", "YTM Song")]), Ok(Vec::new())],
    );
    let catalogue = StdArc::new(held_service(&store, vec![provider(held)]));
    let request = track_search();
    let request_context = context();
    let service = StdArc::clone(&catalogue);
    let mut task =
        tokio::spawn(async move { service.search(&request, None, &request_context).await });
    wait_for_provider_start(&mut started_rx).await;
    assert!(response_at_budget(&mut task).await.tracks.is_empty());

    release.add_permits(1);
    wait_for_materialised(&mut materialized_rx).await;

    let outcome = catalogue.search(&search(), None, &context()).await.unwrap();

    assert_eq!(outcome.tracks.len(), 1);
    assert_eq!(outcome.tracks[0].track().title(), "YTM Song");
}

#[tokio::test(start_paused = true)]
async fn a_late_provider_failure_is_logged_exactly_once() {
    let (materialized, _receiver) = mpsc::unbounded_channel();
    let store = StdArc::new(MemoryStore::new(materialized));
    let (started_tx, mut started_rx) = mpsc::unbounded_channel();
    let (settled_tx, mut settled_rx) = mpsc::unbounded_channel();
    let release = StdArc::new(Semaphore::new(0));
    let gate = StdArc::new(Gate::new(started_tx, StdArc::clone(&release), 1));
    let held = HeldTrackProvider::new(gate, vec![Err(ProviderError::ProviderUnavailable)])
        .with_settled(settled_tx);
    let catalogue = StdArc::new(held_service(&store, vec![provider(held)]));
    let capture = LogCapture::default();
    let _guard =
        tracing::subscriber::set_default(tracing_subscriber::registry().with(capture.clone()));
    let request = track_search();
    let request_context = context();
    let service = StdArc::clone(&catalogue);
    let mut task =
        tokio::spawn(async move { service.search(&request, None, &request_context).await });
    wait_for_provider_start(&mut started_rx).await;
    assert!(response_at_budget(&mut task).await.tracks.is_empty());

    release.add_permits(1);
    wait_for_settled(&mut settled_rx).await;

    let warns = capture.entries_matching("catalogue source failed after the response budget");
    assert_eq!(warns.len(), 1, "{warns:?}");
    assert!(warns[0].starts_with("WARN "), "{warns:?}");
    assert!(warns[0].contains("kind=Tracks"), "{warns:?}");
    assert!(
        warns[0].contains("outcome=\"provider_unavailable\""),
        "{warns:?}"
    );
}

#[tokio::test(start_paused = true)]
async fn a_late_repository_error_is_logged_exactly_once() {
    let (materialized, _receiver) = mpsc::unbounded_channel();
    let store = StdArc::new(MemoryStore::new(materialized));
    store.fail_tracks();
    let (started_tx, mut started_rx) = mpsc::unbounded_channel();
    let (settled_tx, mut settled_rx) = mpsc::unbounded_channel();
    let release = StdArc::new(Semaphore::new(0));
    let gate = StdArc::new(Gate::new(started_tx, StdArc::clone(&release), 1));
    let held = HeldTrackProvider::new(gate, vec![Ok(vec![candidate("ytm-1", "YTM Song")])])
        .with_settled(settled_tx);
    let catalogue = StdArc::new(held_service(&store, vec![provider(held)]));
    let capture = LogCapture::default();
    let _guard =
        tracing::subscriber::set_default(tracing_subscriber::registry().with(capture.clone()));
    let request = track_search();
    let request_context = context();
    let service = StdArc::clone(&catalogue);
    let mut task =
        tokio::spawn(async move { service.search(&request, None, &request_context).await });
    wait_for_provider_start(&mut started_rx).await;
    assert!(response_at_budget(&mut task).await.tracks.is_empty());

    release.add_permits(1);
    wait_for_settled(&mut settled_rx).await;

    let warns = capture.entries_matching("catalogue source failed after the response budget");
    assert_eq!(warns.len(), 1, "{warns:?}");
    assert!(warns[0].starts_with("WARN "), "{warns:?}");
    assert!(warns[0].contains("outcome=\"repository\""), "{warns:?}");
}

#[tokio::test(start_paused = true)]
async fn a_late_invalid_error_is_logged_exactly_once() {
    let capture = LogCapture::default();
    let _guard =
        tracing::subscriber::set_default(tracing_subscriber::registry().with(capture.clone()));
    let release = StdArc::new(Semaphore::new(0));
    let operation_release = StdArc::clone(&release);
    let (started_tx, mut started_rx) = mpsc::unbounded_channel();
    let (settled_tx, mut settled_rx) = mpsc::unbounded_channel();
    let operation = move |_provider: StdArc<dyn CatalogueProvider>| {
        let release = StdArc::clone(&operation_release);
        let started = started_tx.clone();
        let settled = settled_tx.clone();
        async move {
            let _ = started.send(());
            release.acquire_owned().await.unwrap().forget();
            let _ = settled.send(());
            Err::<Vec<()>, CatalogueError>(CatalogueError::Invalid(ValidationError::Invalid {
                field: "test",
            }))
        }
    };
    let mut task = tokio::spawn(super::run_provider_tasks(
        vec![provider(StubProvider::new(vec![]))],
        StdArc::new(Semaphore::new(MAX_SOURCE_CONCURRENCY)),
        StdArc::new(Semaphore::new(MAX_OUTSTANDING_TASKS)),
        PROVIDER_BUDGET,
        RESPONSE_BUDGET,
        SearchKind::Tracks,
        operation,
    ));
    wait_for_provider_start(&mut started_rx).await;
    let (items, discovery) = response_at_budget(&mut task).await;
    assert!(items.is_empty());
    assert_eq!(discovery.pending(), 1);

    release.add_permits(1);
    wait_for_settled(&mut settled_rx).await;

    let warns = capture.entries_matching("catalogue source failed after the response budget");
    assert_eq!(warns.len(), 1, "{warns:?}");
    assert!(warns[0].starts_with("WARN "), "{warns:?}");
    assert!(warns[0].contains("outcome=\"invalid\""), "{warns:?}");
}

#[tokio::test(start_paused = true)]
async fn a_handled_finished_error_is_logged_once() {
    let (materialized, _receiver) = mpsc::unbounded_channel();
    let store = StdArc::new(MemoryStore::new(materialized));
    let catalogue = held_service(
        &store,
        vec![provider(StubProvider::new(vec![Err(
            ProviderError::ProviderUnavailable,
        )]))],
    );
    let capture = LogCapture::default();
    let _guard =
        tracing::subscriber::set_default(tracing_subscriber::registry().with(capture.clone()));

    let result = catalogue.search(&track_search(), None, &context()).await;

    assert!(matches!(
        result,
        Err(CatalogueError::Provider(ProviderError::ProviderUnavailable))
    ));
    let skipped = capture.entries_matching("catalogue source skipped");
    assert_eq!(skipped.len(), 1, "{skipped:?}");
    assert!(skipped[0].starts_with("WARN "), "{skipped:?}");
    assert_eq!(
        capture
            .entries_matching("catalogue source failed after the response budget")
            .len(),
        0
    );
}

#[tokio::test(start_paused = true)]
async fn a_delivered_error_dropped_without_acknowledgement_is_logged_exactly_once() {
    let capture = LogCapture::default();
    let _guard =
        tracing::subscriber::set_default(tracing_subscriber::registry().with(capture.clone()));
    let (settled_tx, mut settled_rx) = mpsc::unbounded_channel();
    let operation = move |_provider: StdArc<dyn CatalogueProvider>| {
        let settled = settled_tx.clone();
        async move {
            let _ = settled.send(());
            Err::<Vec<()>, CatalogueError>(CatalogueError::Provider(
                ProviderError::ProviderUnavailable,
            ))
        }
    };
    let mut caller = Box::pin(super::run_provider_tasks(
        vec![provider(StubProvider::new(vec![]))],
        StdArc::new(Semaphore::new(MAX_SOURCE_CONCURRENCY)),
        StdArc::new(Semaphore::new(MAX_OUTSTANDING_TASKS)),
        PROVIDER_BUDGET,
        RESPONSE_BUDGET,
        SearchKind::Tracks,
        operation,
    ));
    assert!(
        tokio::time::timeout(Duration::ZERO, caller.as_mut())
            .await
            .is_err(),
        "the caller waits for the provider"
    );

    wait_for_settled(&mut settled_rx).await;
    drop(caller);
    tokio::time::timeout(
        HANG,
        capture.recorded("catalogue source failed after the response budget"),
    )
    .await
    .expect("the late warning is recorded");

    let warns = capture.entries_matching("catalogue source failed after the response budget");
    assert_eq!(warns.len(), 1, "{warns:?}");
    assert!(warns[0].starts_with("WARN "), "{warns:?}");
    assert!(
        warns[0].contains("outcome=\"provider_unavailable\""),
        "{warns:?}"
    );
}

#[tokio::test(start_paused = true)]
async fn a_held_album_provider_materialises_after_the_response() {
    let (materialized, mut materialized_rx) = mpsc::unbounded_channel();
    let store = StdArc::new(MemoryStore::new(materialized));
    let (started_tx, mut started_rx) = mpsc::unbounded_channel();
    let release = StdArc::new(Semaphore::new(0));
    let gate = StdArc::new(Gate::new(started_tx, StdArc::clone(&release), 1));
    let held = HeldAlbumProvider::new(
        gate,
        vec![Ok(CataloguePage::new(vec![album_metadata()], None))],
    );
    let catalogue = StdArc::new(held_service(&store, vec![provider(held)]));
    let query = SearchQuery::new("Daft Punk".to_owned()).unwrap();
    let request_context = context();
    let service = StdArc::clone(&catalogue);
    let mut task = tokio::spawn(async move {
        service
            .discover(SearchKind::Albums, &query, 10, &request_context)
            .await
    });
    wait_for_provider_start(&mut started_rx).await;
    assert_eq!(response_at_budget(&mut task).await.pending(), 1);
    assert!(store.album_titles().is_empty());

    release.add_permits(1);
    assert!(matches!(
        wait_for_materialised(&mut materialized_rx).await,
        Materialised::Album
    ));
    assert_eq!(store.album_titles(), vec!["Beatopia".to_owned()]);
}

#[tokio::test(start_paused = true)]
async fn a_held_artist_provider_materialises_after_the_response() {
    let (materialized, mut materialized_rx) = mpsc::unbounded_channel();
    let store = StdArc::new(MemoryStore::new(materialized));
    let (started_tx, mut started_rx) = mpsc::unbounded_channel();
    let release = StdArc::new(Semaphore::new(0));
    let gate = StdArc::new(Gate::new(started_tx, StdArc::clone(&release), 1));
    let held = HeldArtistProvider::new(
        gate,
        vec![Ok(CataloguePage::new(vec![artist_credit()], None))],
    );
    let catalogue = StdArc::new(held_service(&store, vec![provider(held)]));
    let query = SearchQuery::new("Daft Punk".to_owned()).unwrap();
    let request_context = context();
    let service = StdArc::clone(&catalogue);
    let mut task = tokio::spawn(async move {
        service
            .discover(SearchKind::Artists, &query, 10, &request_context)
            .await
    });
    wait_for_provider_start(&mut started_rx).await;
    assert_eq!(response_at_budget(&mut task).await.pending(), 1);
    assert!(store.artist_names().is_empty());

    release.add_permits(1);
    assert!(matches!(
        wait_for_materialised(&mut materialized_rx).await,
        Materialised::Artist
    ));
    assert_eq!(store.artist_names(), vec!["beabadoobee".to_owned()]);
}

#[tokio::test(start_paused = true)]
async fn a_held_album_provider_materialises_after_caller_cancellation() {
    let (materialized, mut materialized_rx) = mpsc::unbounded_channel();
    let store = StdArc::new(MemoryStore::new(materialized));
    let (started_tx, mut started_rx) = mpsc::unbounded_channel();
    let release = StdArc::new(Semaphore::new(0));
    let gate = StdArc::new(Gate::new(started_tx, StdArc::clone(&release), 1));
    let held = HeldAlbumProvider::new(
        gate,
        vec![Ok(CataloguePage::new(vec![album_metadata()], None))],
    );
    let catalogue = StdArc::new(held_service(&store, vec![provider(held)]));
    let query = SearchQuery::new("Daft Punk".to_owned()).unwrap();
    let request_context = context();
    let service = StdArc::clone(&catalogue);
    let task = tokio::spawn(async move {
        service
            .discover(SearchKind::Albums, &query, 10, &request_context)
            .await
    });
    wait_for_provider_start(&mut started_rx).await;

    task.abort();
    assert!(matches!(task.await, Err(error) if error.is_cancelled()));

    release.add_permits(1);
    assert!(matches!(
        wait_for_materialised(&mut materialized_rx).await,
        Materialised::Album
    ));
    assert_eq!(store.album_titles(), vec!["Beatopia".to_owned()]);
}

#[tokio::test(start_paused = true)]
async fn a_late_album_storage_failure_is_logged() {
    let (materialized, _receiver) = mpsc::unbounded_channel();
    let store = StdArc::new(MemoryStore::new(materialized));
    store.fail_albums();
    let (started_tx, mut started_rx) = mpsc::unbounded_channel();
    let (settled_tx, mut settled_rx) = mpsc::unbounded_channel();
    let release = StdArc::new(Semaphore::new(0));
    let gate = StdArc::new(Gate::new(started_tx, StdArc::clone(&release), 1));
    let held = HeldAlbumProvider::new(
        gate,
        vec![Ok(CataloguePage::new(vec![album_metadata()], None))],
    )
    .with_settled(settled_tx);
    let catalogue = StdArc::new(held_service(&store, vec![provider(held)]));
    let capture = LogCapture::default();
    let _guard =
        tracing::subscriber::set_default(tracing_subscriber::registry().with(capture.clone()));
    let query = SearchQuery::new("Daft Punk".to_owned()).unwrap();
    let request_context = context();
    let service = StdArc::clone(&catalogue);
    let mut task = tokio::spawn(async move {
        service
            .discover(SearchKind::Albums, &query, 10, &request_context)
            .await
    });
    wait_for_provider_start(&mut started_rx).await;
    assert_eq!(response_at_budget(&mut task).await.pending(), 1);

    release.add_permits(1);
    wait_for_settled(&mut settled_rx).await;

    let warns = capture.entries_matching("catalogue source failed after the response budget");
    assert_eq!(warns.len(), 1, "{warns:?}");
    assert!(warns[0].starts_with("WARN "), "{warns:?}");
    assert!(warns[0].contains("outcome=\"repository\""), "{warns:?}");
}

#[tokio::test(start_paused = true)]
async fn all_providers_pending_is_an_empty_success() {
    let (materialized, _receiver) = mpsc::unbounded_channel();
    let store = StdArc::new(MemoryStore::new(materialized));
    let (started_tx, mut started_rx) = mpsc::unbounded_channel();
    let (settled_tx, mut settled_rx) = mpsc::unbounded_channel();
    let release = StdArc::new(Semaphore::new(0));
    let gate = StdArc::new(Gate::new(started_tx, StdArc::clone(&release), usize::MAX));
    let providers = vec![
        provider(
            HeldTrackProvider::new(StdArc::clone(&gate), vec![Ok(Vec::new())])
                .with_settled(settled_tx.clone()),
        ),
        provider(
            HeldTrackProvider::new(StdArc::clone(&gate), vec![Ok(Vec::new())])
                .with_settled(settled_tx.clone()),
        ),
    ];
    let catalogue = StdArc::new(held_service(&store, providers));
    let query = SearchQuery::new("Daft Punk".to_owned()).unwrap();
    let request_context = context();
    let service = StdArc::clone(&catalogue);
    let mut task =
        tokio::spawn(async move { service.discover_tracks(&query, 10, &request_context).await });
    wait_for_provider_start(&mut started_rx).await;
    wait_for_provider_start(&mut started_rx).await;

    let (hits, discovery) = response_at_budget(&mut task).await;

    assert!(hits.is_empty());
    assert_eq!(discovery.pending(), 2);

    release.add_permits(2);
    wait_for_settled(&mut settled_rx).await;
    wait_for_settled(&mut settled_rx).await;
}

#[tokio::test(start_paused = true)]
async fn a_request_beyond_the_outstanding_limit_is_pending_not_an_error() {
    let (materialized, _receiver) = mpsc::unbounded_channel();
    let store = StdArc::new(MemoryStore::new(materialized));
    let (started_tx, mut started_rx) = mpsc::unbounded_channel();
    let (finished_tx, mut finished_rx) = mpsc::unbounded_channel();
    let release = StdArc::new(Semaphore::new(0));
    let gate = StdArc::new(
        Gate::new(started_tx, StdArc::clone(&release), MAX_SOURCE_CONCURRENCY)
            .with_finished(finished_tx),
    );
    let providers = (0..MAX_OUTSTANDING_TASKS)
        .map(|_| {
            provider(HeldTrackProvider::new(
                StdArc::clone(&gate),
                vec![Ok(Vec::new())],
            ))
        })
        .collect::<Vec<_>>();
    let catalogue = StdArc::new(held_service(&store, providers));
    let query = SearchQuery::new("Daft Punk".to_owned()).unwrap();
    let request_context = context();
    let service = StdArc::clone(&catalogue);
    let spawned_query = query.clone();
    let mut task = tokio::spawn(async move {
        service
            .discover_tracks(&spawned_query, 10, &request_context)
            .await
    });
    for _ in 0..MAX_SOURCE_CONCURRENCY {
        wait_for_provider_start(&mut started_rx).await;
    }

    let (_, blocked) = catalogue
        .discover_tracks(&query, 10, &context())
        .await
        .unwrap();
    assert_eq!(blocked.pending(), MAX_OUTSTANDING_TASKS);
    assert_eq!(gate.call_count(), MAX_SOURCE_CONCURRENCY);

    let (_, held) = response_at_budget(&mut task).await;
    assert_eq!(held.pending(), MAX_OUTSTANDING_TASKS);

    release.add_permits(MAX_SOURCE_CONCURRENCY);
    for _ in 0..MAX_OUTSTANDING_TASKS {
        wait_for_settled(&mut finished_rx).await;
    }

    let (_, reused) = catalogue
        .discover_tracks(&query, 10, &context())
        .await
        .unwrap();
    assert_eq!(reused.pending(), 0);
    assert_eq!(gate.call_count(), MAX_OUTSTANDING_TASKS * 2);
}

#[tokio::test(start_paused = true)]
async fn a_task_whose_provider_deadline_passes_while_waiting_ends_without_calling_the_provider() {
    let (materialized, _receiver) = mpsc::unbounded_channel();
    let store = StdArc::new(MemoryStore::new(materialized));
    let (started_tx, mut started_rx) = mpsc::unbounded_channel();
    let (finished_tx, mut finished_rx) = mpsc::unbounded_channel();
    let release = StdArc::new(Semaphore::new(0));
    let gate = StdArc::new(
        Gate::new(started_tx, StdArc::clone(&release), MAX_SOURCE_CONCURRENCY)
            .with_finished(finished_tx),
    );
    let providers = (0..MAX_OUTSTANDING_TASKS)
        .map(|_| {
            provider(HeldTrackProvider::new(
                StdArc::clone(&gate),
                vec![Ok(Vec::new())],
            ))
        })
        .collect::<Vec<_>>();
    let catalogue = StdArc::new(held_service(&store, providers));
    let query = SearchQuery::new("Daft Punk".to_owned()).unwrap();
    let request_context = context();
    let service = StdArc::clone(&catalogue);
    let spawned_query = query.clone();
    let mut task = tokio::spawn(async move {
        service
            .discover_tracks(&spawned_query, 10, &request_context)
            .await
    });
    for _ in 0..MAX_SOURCE_CONCURRENCY {
        wait_for_provider_start(&mut started_rx).await;
    }

    let (_, discovery) = response_at_budget(&mut task).await;
    assert_eq!(discovery.pending(), MAX_OUTSTANDING_TASKS);
    assert_eq!(gate.call_count(), MAX_SOURCE_CONCURRENCY);
    assert_eq!(gate.maximum(), MAX_SOURCE_CONCURRENCY);

    tokio::time::advance(PROVIDER_BUDGET - RESPONSE_BUDGET + Duration::from_millis(1)).await;
    for _ in 0..MAX_SOURCE_CONCURRENCY {
        wait_for_settled(&mut finished_rx).await;
    }
    assert_eq!(gate.call_count(), MAX_SOURCE_CONCURRENCY);
    assert_eq!(gate.maximum(), MAX_SOURCE_CONCURRENCY);

    let (_, reused) = catalogue
        .discover_tracks(&query, 10, &context())
        .await
        .unwrap();
    assert_eq!(reused.pending(), 0);
}
