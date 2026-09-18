use std::{collections::HashSet, sync::Arc, time::Duration};
use uuid::Uuid;

use thiserror::Error;
use tokio::{sync::Semaphore, task::JoinSet};

use crate::{
    catalogue::{
        CatalogueCandidate, CatalogueProvider, CatalogueSearch, SearchLimit, SearchOffset,
        SearchQuery,
    },
    providers::ProviderError,
    request::RequestContext,
    tracks::{Track, TrackId, TrackRepository, TrackRepositoryError},
};

#[derive(Debug, Error)]
pub enum CatalogueError {
    #[error("catalogue provider failed")]
    Provider(#[source] ProviderError),

    #[error("track repository failed")]
    Repository(#[source] TrackRepositoryError),
}

pub struct CatalogueService {
    track_repository: Arc<dyn TrackRepository>,
    providers: Vec<Arc<dyn CatalogueProvider>>,
}

pub struct SearchOutcome {
    pub tracks: Vec<Track>,
}

impl CatalogueService {
    pub fn new(track_repository: Arc<dyn TrackRepository>) -> Self {
        Self {
            track_repository,
            providers: Vec::new(),
        }
    }

    pub fn with_providers(
        track_repository: Arc<dyn TrackRepository>,
        providers: Vec<Arc<dyn CatalogueProvider>>,
    ) -> Self {
        Self {
            track_repository,
            providers,
        }
    }

    pub async fn track(&self, track_id: &TrackId) -> Result<Option<Track>, CatalogueError> {
        self.track_repository
            .find_track(track_id)
            .await
            .map_err(CatalogueError::Repository)
    }

    pub async fn search(
        &self,
        search: &CatalogueSearch,
        user: Option<&Uuid>,
        context: &RequestContext,
    ) -> Result<SearchOutcome, CatalogueError> {
        let (tracks, first_error, any_success) =
            self.fan_out(search, user, context, PROVIDER_BUDGET).await;
        Self::finalize(search, tracks, first_error, any_success)
    }

    async fn fan_out(
        &self,
        search: &CatalogueSearch,
        user: Option<&Uuid>,
        context: &RequestContext,
        budget: Duration,
    ) -> (Vec<Track>, Option<CatalogueError>, bool) {
        // Dropping the set aborts remaining tasks, so a disconnected
        // request stops its providers instead of orphaning them.
        let mut tasks = JoinSet::new();
        let concurrency = Arc::new(Semaphore::new(MAX_SOURCE_CONCURRENCY));
        let query = search.query().clone();
        let target = provider_target(search);
        for (index, provider) in self.providers.iter().enumerate() {
            let permit = Arc::clone(&concurrency);
            let pending = search_source(
                Arc::clone(provider),
                Arc::clone(&self.track_repository),
                query.clone(),
                target,
                user.copied(),
                context.clone(),
            );
            tasks.spawn(async move {
                // Provider timeout starts after the concurrency permit is acquired.
                let _permit = permit
                    .acquire_owned()
                    .await
                    .expect("catalogue concurrency semaphore never closes");
                match tokio::time::timeout(budget, pending).await {
                    Ok(result) => (index, result),
                    Err(_) => {
                        tracing::warn!("catalogue source exceeded its provider budget");
                        (
                            index,
                            Err(CatalogueError::Provider(ProviderError::ProviderUnavailable)),
                        )
                    }
                }
            });
        }

        let mut results = (0..self.providers.len())
            .map(|_| None)
            .collect::<Vec<Option<Result<Vec<Track>, CatalogueError>>>>();
        while let Some(result) = tasks.join_next().await {
            let (index, result) = result.expect("search tasks never panic");
            results[index] = Some(result);
        }

        let mut tracks = Vec::new();
        let mut first_error = None;
        let mut any_success = false;
        // Preserve source priority regardless of completion order.
        for result in results.into_iter().flatten() {
            match result {
                Ok(page) => {
                    any_success = true;
                    tracks.extend(page);
                }
                Err(CatalogueError::Provider(ProviderError::CredentialsRejected)) => {
                    // the adapter has already marked the link stale; the remaining
                    // sources still answer, so this only degrades the result.
                    tracing::warn!("catalogue source credentials were rejected");
                }
                Err(error) => {
                    tracing::warn!(%error, "catalogue source skipped");
                    if matches!(error, CatalogueError::Provider(ProviderError::Access)) {
                        first_error = Some(error);
                    } else {
                        first_error.get_or_insert(error);
                    }
                }
            }
        }
        (tracks, first_error, any_success)
    }

    fn finalize(
        search: &CatalogueSearch,
        tracks: Vec<Track>,
        first_error: Option<CatalogueError>,
        any_success: bool,
    ) -> Result<SearchOutcome, CatalogueError> {
        let limit = search.limit().get() as usize;
        let offset = search.offset().get() as usize;
        let mut seen = HashSet::new();
        let tracks: Vec<Track> = tracks
            .into_iter()
            .filter(|track| seen.insert(track.id().clone()))
            .skip(offset)
            .take(limit)
            .collect();

        if !any_success && let Some(error) = first_error {
            return Err(error);
        }
        if let Some(error @ CatalogueError::Provider(ProviderError::Access)) = first_error {
            return Err(error);
        }

        Ok(SearchOutcome { tracks })
    }
}

const PROVIDER_BUDGET: Duration = Duration::from_secs(45);
const MAX_SOURCE_CONCURRENCY: usize = 4;
const PROVIDER_PAGE_LIMIT: u32 = 25;

/// Providers page independently, so the aggregate window is built by walking
/// each provider from its first page until it has produced enough candidates.
fn provider_target(search: &CatalogueSearch) -> u32 {
    u32::from(search.limit().get()).saturating_add(search.offset().get())
}

async fn search_source(
    provider: Arc<dyn CatalogueProvider>,
    repository: Arc<dyn TrackRepository>,
    query: SearchQuery,
    target: u32,
    user: Option<Uuid>,
    context: RequestContext,
) -> Result<Vec<Track>, CatalogueError> {
    let mut tracks = Vec::new();
    let mut fetched = 0u32;
    while fetched < target {
        let page_size = (target - fetched).min(PROVIDER_PAGE_LIMIT);
        let search = CatalogueSearch::new_with_offset(
            query.clone(),
            SearchLimit::new(page_size).expect("bounded provider limit is valid"),
            SearchOffset::new(fetched).expect("bounded provider offset is valid"),
        );
        let candidates = provider
            .search(&search, user.as_ref(), &context)
            .await
            .map_err(CatalogueError::Provider)?;
        let page_len = candidates.len() as u32;
        tracks.extend(materialize(&repository, candidates).await?);
        fetched = fetched.saturating_add(page_len);
        // a short page means the provider has no more results for this query.
        if page_len < page_size {
            break;
        }
    }
    Ok(tracks)
}

async fn materialize(
    repository: &Arc<dyn TrackRepository>,
    candidates: Vec<CatalogueCandidate>,
) -> Result<Vec<Track>, CatalogueError> {
    let mut tracks = Vec::with_capacity(candidates.len());
    for candidate in candidates {
        let source = candidate.source().clone();
        let artwork = candidate.artwork().cloned();
        let metadata = candidate.metadata().clone();

        let track = repository
            .get_or_create(source.clone(), metadata)
            .await
            .map_err(CatalogueError::Repository)?;

        if let Some(artwork) = artwork
            && let Err(error) = repository.set_source_artwork(&source, &artwork).await
        {
            tracing::warn!(%error, "artwork could not be stored");
        }

        tracks.push(track);
    }
    Ok(tracks)
}

#[cfg(test)]
mod tests {
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
        catalogue::{CatalogueCandidate, SearchLimit, SearchOffset, SearchQuery},
        identity::{secrets::CredentialKey, upstream::MemoryUpstreamLinkStore},
        providers::{
            NavidromeAdapter, UpstreamCandidate, UpstreamError, UpstreamTarget, UpstreamTransport,
        },
        request::RequestId,
        tracks::{InMemoryTrackRepository, ProviderId, SourceIdentity, SourceScope, TrackMetadata},
    };
    use uuid::Uuid;

    struct StubProvider {
        responses: Mutex<VecDeque<Result<Vec<CatalogueCandidate>, ProviderError>>>,
        delay: Duration,
    }

    struct FailingTrackRepository;

    #[async_trait]
    impl TrackRepository for FailingTrackRepository {
        async fn get_or_create(
            &self,
            _source: SourceIdentity,
            _metadata: TrackMetadata,
        ) -> Result<Track, TrackRepositoryError> {
            Err(TrackRepositoryError::Unavailable)
        }

        async fn find_sources(
            &self,
            _track_id: &crate::tracks::TrackId,
        ) -> Result<Vec<SourceIdentity>, TrackRepositoryError> {
            Err(TrackRepositoryError::Unavailable)
        }

        async fn find_track(
            &self,
            _track_id: &crate::tracks::TrackId,
        ) -> Result<Option<Track>, TrackRepositoryError> {
            Err(TrackRepositoryError::Unavailable)
        }
    }

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
        let metadata = TrackMetadata::new(
            title.to_owned(),
            vec!["Daft Punk".to_owned()],
            Some(337_000),
        )
        .unwrap();

        CatalogueCandidate::new(source, metadata)
    }

    fn search() -> CatalogueSearch {
        CatalogueSearch::new(
            SearchQuery::new("Daft Punk".to_owned()).unwrap(),
            SearchLimit::new(10).unwrap(),
        )
    }

    fn search_with_offset(limit: u32, offset: u32) -> CatalogueSearch {
        CatalogueSearch::new_with_offset(
            SearchQuery::new("Daft Punk".to_owned()).unwrap(),
            SearchLimit::new(limit).unwrap(),
            SearchOffset::new(offset).unwrap(),
        )
    }

    fn context() -> RequestContext {
        RequestContext::new(RequestId::new("request-123".to_owned()).unwrap())
    }

    fn service(providers: Vec<Arc<dyn CatalogueProvider>>) -> CatalogueService {
        CatalogueService::with_providers(Arc::new(InMemoryTrackRepository::default()), providers)
    }

    fn provider(stub: impl CatalogueProvider + 'static) -> Arc<dyn CatalogueProvider> {
        Arc::new(stub) as Arc<dyn CatalogueProvider>
    }

    type StubResult = Result<Vec<UpstreamCandidate>, UpstreamError>;

    struct StubTransport {
        response: Mutex<StubResult>,
    }

    #[async_trait]
    impl UpstreamTransport for StubTransport {
        async fn search(
            &self,
            _target: &UpstreamTarget,
            _search: &CatalogueSearch,
            _context: &RequestContext,
        ) -> StubResult {
            std::mem::replace(&mut *self.response.lock(), Err(UpstreamError::Unavailable))
        }

        async fn resolve(
            &self,
            _target: &UpstreamTarget,
            _external_id: &str,
            _context: &RequestContext,
        ) -> Result<String, UpstreamError> {
            Err(UpstreamError::Unavailable)
        }
    }

    fn upstream_candidate() -> UpstreamCandidate {
        UpstreamCandidate {
            external_id: "song-1".to_owned(),
            title: "Upstream Song".to_owned(),
            artists: vec!["Artist".to_owned()],
            duration_ms: Some(200_000),
            album: None,
            artwork_id: None,
        }
    }

    fn linked(password: &str) -> (CredentialKey, Uuid, Uuid, MemoryUpstreamLinkStore) {
        let key =
            CredentialKey::from_base64("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA").unwrap();
        let user = Uuid::now_v7();
        let server = Uuid::now_v7();
        let store = MemoryUpstreamLinkStore::new();
        store.insert_link(
            &key,
            user,
            server,
            "https://music.example.test",
            "friend",
            password,
        );
        (key, user, server, store)
    }

    fn navidrome(
        transport: StubTransport,
        key: CredentialKey,
        store: Arc<MemoryUpstreamLinkStore>,
    ) -> NavidromeAdapter {
        NavidromeAdapter::new(store, key, Arc::new(transport))
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

    #[tokio::test]
    async fn materialize_assigns_stable_ids_uses_latest_metadata_and_dedupes() {
        let resolver = StubProvider::new(vec![
            Ok(vec![
                candidate("source-a", "Old title"),
                candidate("source-b", "Other"),
            ]),
            Ok(vec![candidate("source-a", "New title")]),
            Ok(vec![candidate("dup", "Same"), candidate("dup", "Same")]),
        ]);
        let catalogue = service(vec![provider(resolver)]);

        let first = catalogue.search(&search(), None, &context()).await.unwrap();
        let second = catalogue.search(&search(), None, &context()).await.unwrap();
        assert_ne!(first.tracks[0].id(), first.tracks[1].id());
        assert_eq!(first.tracks[0].id(), second.tracks[0].id());
        assert_eq!(first.tracks[0].title(), "Old title");
        assert_eq!(second.tracks[0].title(), "New title");

        let third = catalogue.search(&search(), None, &context()).await.unwrap();
        assert_eq!(third.tracks.len(), 1);
    }

    #[tokio::test]
    async fn provider_order_decides_rank() {
        let slow_preferred = StubProvider::delayed(
            vec![Ok(vec![candidate("preferred", "Preferred")])],
            Duration::from_millis(30),
        );
        let fast_fallback = StubProvider::delayed(
            vec![Ok(vec![candidate("fallback", "Fallback")])],
            Duration::from_millis(1),
        );
        let catalogue = service(vec![provider(slow_preferred), provider(fast_fallback)]);
        let outcome = catalogue.search(&search(), None, &context()).await.unwrap();
        assert_eq!(
            outcome.tracks.iter().map(Track::title).collect::<Vec<_>>(),
            vec!["Preferred", "Fallback"]
        );

        let (key, user, _server, store) = linked("password");
        let upstream = navidrome(
            StubTransport {
                response: Mutex::new(Ok(vec![upstream_candidate()])),
            },
            key,
            Arc::new(store),
        );
        let fallback = StubProvider::new(vec![Ok(vec![candidate("vid-1", "Fallback")])]);
        let catalogue = service(vec![provider(upstream), provider(fallback)]);
        let outcome = catalogue
            .search(&search(), Some(&user), &context())
            .await
            .unwrap();
        assert_eq!(outcome.tracks.len(), 2);
        assert_eq!(outcome.tracks[0].title(), "Upstream Song");
        assert_eq!(outcome.tracks[1].title(), "Fallback");
    }

    #[tokio::test]
    async fn failures_degrade_partially_and_error_in_total() {
        let broken = StubProvider::new(vec![Err(ProviderError::ProviderUnavailable)]);
        let empty = StubProvider::new(vec![Ok(Vec::new())]);
        let working = StubProvider::new(vec![Ok(vec![candidate("ytm-1", "YTM Song")])]);
        let catalogue = service(vec![provider(broken), provider(empty), provider(working)]);
        let outcome = catalogue.search(&search(), None, &context()).await.unwrap();
        assert_eq!(outcome.tracks.len(), 1);
        assert_eq!(outcome.tracks[0].title(), "YTM Song");

        let broken = StubProvider::new(vec![Err(ProviderError::ProviderUnavailable)]);
        let result = service(vec![provider(broken)])
            .search(&search(), None, &context())
            .await;
        assert!(matches!(
            result,
            Err(CatalogueError::Provider(ProviderError::ProviderUnavailable))
        ));

        let denied = StubProvider::new(vec![Err(ProviderError::Access)]);
        let working = StubProvider::new(vec![Ok(vec![candidate("ytm-1", "YTM Song")])]);
        let result = service(vec![provider(denied), provider(working)])
            .search(&search(), None, &context())
            .await;
        assert!(matches!(
            result,
            Err(CatalogueError::Provider(ProviderError::Access))
        ));
    }

    #[tokio::test]
    async fn stale_credentials_are_reported_and_marked() {
        let (key, user, server, store) = linked("password");
        let store = Arc::new(store);
        let adapter = navidrome(
            StubTransport {
                response: Mutex::new(Err(UpstreamError::Auth)),
            },
            key,
            Arc::clone(&store),
        );
        let error = adapter
            .search(&search(), Some(&user), &context())
            .await
            .unwrap_err();
        assert!(matches!(error, ProviderError::CredentialsRejected));
        assert!(store.is_stale(&user, &server));

        let (key, user, server, store) = linked("password");
        let store = Arc::new(store);
        let upstream = navidrome(
            StubTransport {
                response: Mutex::new(Err(UpstreamError::Auth)),
            },
            key,
            Arc::clone(&store),
        );
        let fallback = StubProvider::new(vec![Ok(vec![candidate("vid-1", "Fallback")])]);
        let catalogue = service(vec![provider(upstream), provider(fallback)]);
        let outcome = catalogue
            .search(&search(), Some(&user), &context())
            .await
            .unwrap();
        assert_eq!(outcome.tracks.len(), 1);
        assert!(store.is_stale(&user, &server));
    }

    #[tokio::test]
    async fn aggregate_pagination_caps_slices_and_clamps() {
        let first = StubProvider::new(vec![Ok((0..6)
            .map(|index| candidate(&format!("a-{index}"), &format!("A{index}")))
            .collect())]);
        let second = StubProvider::new(vec![Ok((0..6)
            .map(|index| candidate(&format!("b-{index}"), &format!("B{index}")))
            .collect())]);
        let catalogue = service(vec![provider(first), provider(second)]);
        let outcome = catalogue.search(&search(), None, &context()).await.unwrap();
        assert_eq!(outcome.tracks.len(), 10);
        assert_eq!(outcome.tracks[0].title(), "A0");
        assert_eq!(outcome.tracks[6].title(), "B0");

        let resolver = StubProvider::new(vec![Ok((0..25)
            .map(|index| candidate(&format!("source-{index}"), &format!("Song {index}")))
            .collect())]);
        let catalogue = service(vec![provider(resolver)]);
        let outcome = catalogue
            .search(&search_with_offset(10, 10), None, &context())
            .await
            .unwrap();
        assert_eq!(outcome.tracks.len(), 10);
        assert_eq!(outcome.tracks[0].title(), "Song 10");
        assert_eq!(outcome.tracks[9].title(), "Song 19");

        let limits = StdArc::new(Mutex::new(Vec::new()));
        let resolver = LimitRecordingProvider {
            limit: StdArc::clone(&limits),
        };
        service(vec![provider(resolver)])
            .search(&search_with_offset(10, 20), None, &context())
            .await
            .unwrap();
        assert_eq!(*limits.lock(), vec![25]);
    }

    #[tokio::test]
    async fn deep_pages_walk_the_provider_window() {
        let catalogue = service(vec![provider(PagedProvider { total: 80 })]);

        let outcome = catalogue
            .search(&search_with_offset(10, 30), None, &context())
            .await
            .unwrap();

        assert_eq!(outcome.tracks.len(), 10);
        assert_eq!(outcome.tracks[0].title(), "P30");
        assert_eq!(outcome.tracks[9].title(), "P39");
    }

    #[tokio::test]
    async fn slow_sources_are_bounded_by_budget_and_concurrency() {
        let working = StubProvider::new(vec![Ok(vec![candidate("ytm-1", "YTM Song")])]);
        let catalogue = service(vec![provider(HangingProvider), provider(working)]);
        let started = Instant::now();
        let (tracks, _error, _success) = catalogue
            .fan_out(&search(), None, &context(), Duration::from_millis(50))
            .await;
        assert!(started.elapsed() < Duration::from_secs(5));
        assert_eq!(tracks.len(), 1);
        assert_eq!(tracks[0].title(), "YTM Song");

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
        service(providers)
            .search(&search(), None, &context())
            .await
            .unwrap();
        assert!(maximum.load(Ordering::SeqCst) <= MAX_SOURCE_CONCURRENCY);
    }

    #[tokio::test]
    async fn search_preserves_repository_errors() {
        let resolver = StubProvider::new(vec![Ok(vec![candidate("source-a", "Title")])]);
        let service = CatalogueService::with_providers(
            Arc::new(FailingTrackRepository),
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
}
