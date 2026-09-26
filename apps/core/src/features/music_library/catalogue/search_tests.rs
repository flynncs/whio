use crate::features::music_library::storage::contracts::{
    CatalogueWrite, DuplicateReport, LibraryRead, SourceLookup, StateStore,
};
use std::{
    collections::VecDeque,
    sync::{
        Arc as StdArc,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

use async_trait::async_trait;
use parking_lot::Mutex;

use super::*;
use crate::{
    features::music_library::catalogue::{
        CatalogueCandidate, SearchLimit, SearchOffset, SearchQuery, SearchRequest, SearchWindow,
    },
    features::music_library::{
        domain::{
            AlbumMetadata, ArtistCredit, ProviderId, SourceIdentity, SourceScope, Track,
            TrackMetadata,
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
    async fn search(
        &self,
        _search: &CatalogueSearch,
        _user: Option<&Uuid>,
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

fn provider(stub: impl CatalogueProvider + 'static) -> Arc<dyn CatalogueProvider> {
    Arc::new(stub) as Arc<dyn CatalogueProvider>
}

struct HangingProvider;

#[async_trait]
impl CatalogueProvider for HangingProvider {
    async fn search(
        &self,
        _search: &CatalogueSearch,
        _user: Option<&Uuid>,
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
    async fn search(
        &self,
        _search: &CatalogueSearch,
        _user: Option<&Uuid>,
        _context: &RequestContext,
    ) -> Result<Vec<CatalogueCandidate>, ProviderError> {
        let active = self.current.fetch_add(1, Ordering::SeqCst) + 1;
        self.maximum.fetch_max(active, Ordering::SeqCst);
        tokio::time::sleep(Duration::from_millis(10)).await;
        self.current.fetch_sub(1, Ordering::SeqCst);
        Ok(Vec::new())
    }
}

struct LimitRecordingProvider {
    limit: StdArc<Mutex<Vec<u8>>>,
}

#[async_trait]
impl CatalogueProvider for LimitRecordingProvider {
    async fn search(
        &self,
        search: &CatalogueSearch,
        _user: Option<&Uuid>,
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
    async fn search(
        &self,
        search: &CatalogueSearch,
        _user: Option<&Uuid>,
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

#[sqlx::test]
async fn slow_sources_are_bounded_by_budget_and_concurrency(pool: sqlx::PgPool) {
    let working = StubProvider::new(vec![Ok(vec![candidate("ytm-1", "YTM Song")])]);
    let catalogue = service(
        pool.clone(),
        vec![provider(HangingProvider), provider(working)],
    );
    let started = Instant::now();
    let (hits, _error, _success) = catalogue
        .fan_out(&search(), None, &context(), Duration::from_millis(250))
        .await;
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
    use whio_resolver_api::models::{
        CatalogueAlbum as AlbumDto, CatalogueArtist as ArtistDto,
        CatalogueCandidate as CandidateDto, SourceIdentity as SourceIdentityDto,
    };

    fn provider_source(external_id: &str) -> SourceIdentity {
        SourceIdentity::new(
            ProviderId::new("youtube_music".to_owned()).unwrap(),
            SourceScope::Global,
            external_id.to_owned(),
        )
        .unwrap()
    }

    let dto = CandidateDto {
        source: Box::new(SourceIdentityDto::new(
            "youtube_music".to_owned(),
            "video-1".to_owned(),
        )),
        title: "brutal".to_owned(),
        artists: vec![ArtistDto {
            name: "Olivia Rodrigo".to_owned(),
            source: Some(Box::new(SourceIdentityDto::new(
                "youtube_music".to_owned(),
                "artist-1".to_owned(),
            ))),
        }],
        duration_ms: Some(Some(144_000)),
        album: Some(Box::new(AlbumDto {
            title: "SOUR".to_owned(),
            source: Some(Box::new(SourceIdentityDto::new(
                "youtube_music".to_owned(),
                "album-1".to_owned(),
            ))),
            year: Some(Some(2021)),
            release_type: Some(Some("Album".to_owned())),
        })),
        artwork_url: None,
    };
    let candidate = CatalogueCandidate::try_from(dto).unwrap();

    let repository = Arc::new(PostgresTrackRepository::new(pool.clone()));
    let resolver = StubProvider::new(vec![Ok(vec![candidate.clone()])]);
    let catalogue = Fixture {
        store: repository.clone(),
    }
    .service(vec![provider(resolver)]);

    catalogue.search(&search(), None, &context()).await.unwrap();

    let album = repository
        .resolve_or_create_album(candidate.album().expect("the album metadata"))
        .await
        .unwrap();
    assert_eq!(
        repository.find_album_sources(album.id()).await.unwrap(),
        vec![provider_source("album-1")],
        "materialize stores the album's own identity, not the song's"
    );

    let credit = &candidate.metadata().artists()[0];
    let artist = repository.resolve_or_create_artist(credit).await.unwrap();
    assert_eq!(
        repository.find_artist_sources(artist.id()).await.unwrap(),
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
