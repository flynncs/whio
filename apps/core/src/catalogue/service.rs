use std::{collections::HashMap, sync::Arc, time::Duration};
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
    tracks::{SourceIdentity, Track, TrackId, TrackRepository, TrackRepositoryError},
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
    pub hits: Vec<SearchHit>,
}

/// a matched track with the sources that returned it. provenance is observed
/// per search, not stored on the track, so the same track can be reached
/// through several providers without either one owning it.
#[derive(Debug, Clone)]
pub struct SearchHit {
    track: Track,
    sources: Vec<SourceIdentity>,
}

impl SearchHit {
    pub fn new(track: Track, sources: Vec<SourceIdentity>) -> Self {
        Self { track, sources }
    }

    pub fn track(&self) -> &Track {
        &self.track
    }

    pub fn into_track(self) -> Track {
        self.track
    }

    pub fn sources(&self) -> &[SourceIdentity] {
        &self.sources
    }

    /// keeps first-seen source order, which is the order the providers were
    /// queried in.
    fn merge(&mut self, other: SearchHit) {
        for source in other.sources {
            if !self.sources.contains(&source) {
                self.sources.push(source);
            }
        }
    }
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

    /// every source observed for one track, for facades that render a track
    /// outside a search result.
    pub async fn sources(&self, track_id: &TrackId) -> Result<Vec<SourceIdentity>, CatalogueError> {
        self.track_repository
            .find_sources(track_id)
            .await
            .map_err(CatalogueError::Repository)
    }

    pub async fn search(
        &self,
        search: &CatalogueSearch,
        user: Option<&Uuid>,
        context: &RequestContext,
    ) -> Result<SearchOutcome, CatalogueError> {
        let (hits, first_error, any_success) =
            self.fan_out(search, user, context, PROVIDER_BUDGET).await;
        Self::finalize(search, hits, first_error, any_success)
    }

    async fn fan_out(
        &self,
        search: &CatalogueSearch,
        user: Option<&Uuid>,
        context: &RequestContext,
        budget: Duration,
    ) -> (Vec<SearchHit>, Option<CatalogueError>, bool) {
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
            .collect::<Vec<Option<Result<Vec<SearchHit>, CatalogueError>>>>();
        while let Some(result) = tasks.join_next().await {
            let (index, result) = result.expect("search tasks never panic");
            results[index] = Some(result);
        }

        let mut hits = Vec::new();
        let mut first_error = None;
        let mut any_success = false;
        // Preserve source priority regardless of completion order.
        for result in results.into_iter().flatten() {
            match result {
                Ok(page) => {
                    any_success = true;
                    hits.extend(page);
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
        (hits, first_error, any_success)
    }

    fn finalize(
        search: &CatalogueSearch,
        hits: Vec<SearchHit>,
        first_error: Option<CatalogueError>,
        any_success: bool,
    ) -> Result<SearchOutcome, CatalogueError> {
        let limit = search.limit().get() as usize;
        let offset = search.offset().get() as usize;
        let mut positions: HashMap<TrackId, usize> = HashMap::new();
        let mut unique: Vec<SearchHit> = Vec::new();
        for hit in hits {
            match positions.get(hit.track().id()).copied() {
                // the first observation decides rank; later ones only add the
                // providers that also returned the track.
                Some(index) => unique[index].merge(hit),
                None => {
                    positions.insert(hit.track().id().clone(), unique.len());
                    unique.push(hit);
                }
            }
        }
        let hits: Vec<SearchHit> = unique.into_iter().skip(offset).take(limit).collect();

        if !any_success && let Some(error) = first_error {
            return Err(error);
        }
        if let Some(error @ CatalogueError::Provider(ProviderError::Access)) = first_error {
            return Err(error);
        }

        Ok(SearchOutcome { hits })
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
) -> Result<Vec<SearchHit>, CatalogueError> {
    let mut hits = Vec::new();
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
        hits.extend(materialize(&repository, candidates).await?);
        fetched = fetched.saturating_add(page_len);
        // a short page means the provider has no more results for this query.
        if page_len < page_size {
            break;
        }
    }
    Ok(hits)
}

async fn materialize(
    repository: &Arc<dyn TrackRepository>,
    candidates: Vec<CatalogueCandidate>,
) -> Result<Vec<SearchHit>, CatalogueError> {
    let mut hits = Vec::with_capacity(candidates.len());
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

        hits.push(SearchHit::new(track, vec![source]));
    }
    Ok(hits)
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
        let (first_a, first_b) = (&first.hits[0], &first.hits[1]);
        assert_ne!(first_a.track().id(), first_b.track().id());
        assert_eq!(first_a.track().id(), second.hits[0].track().id());
        assert_eq!(first_a.track().title(), "Old title");
        assert_eq!(second.hits[0].track().title(), "New title");
        assert_eq!(first_a.sources()[0].external_id(), "source-a");
        assert_eq!(first_b.sources()[0].external_id(), "source-b");

        let third = catalogue.search(&search(), None, &context()).await.unwrap();
        assert_eq!(third.hits.len(), 1);
        assert_eq!(third.hits[0].sources().len(), 1);
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
                TrackMetadata::new("song".to_owned(), vec!["artist".to_owned()], None).unwrap(),
            )
        };
        let mut hit = SearchHit::new(track(), vec![source("navidrome", "nd-1")]);
        hit.merge(SearchHit::new(
            track(),
            vec![source("youtube_music", "vid-1")],
        ));
        hit.merge(SearchHit::new(track(), vec![source("navidrome", "nd-1")]));

        assert_eq!(
            hit.sources()
                .iter()
                .map(|source| source.provider_id().as_str())
                .collect::<Vec<_>>(),
            vec!["navidrome", "youtube_music"]
        );
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
            outcome
                .hits
                .iter()
                .map(|hit| hit.track().title())
                .collect::<Vec<_>>(),
            vec!["Preferred", "Fallback"]
        );
    }

    #[tokio::test]
    async fn failures_degrade_partially_and_error_in_total() {
        let broken = StubProvider::new(vec![Err(ProviderError::ProviderUnavailable)]);
        let empty = StubProvider::new(vec![Ok(Vec::new())]);
        let working = StubProvider::new(vec![Ok(vec![candidate("ytm-1", "YTM Song")])]);
        let catalogue = service(vec![provider(broken), provider(empty), provider(working)]);
        let outcome = catalogue.search(&search(), None, &context()).await.unwrap();
        assert_eq!(outcome.hits.len(), 1);
        assert_eq!(outcome.hits[0].track().title(), "YTM Song");

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
    async fn aggregate_pagination_caps_slices_and_clamps() {
        let first = StubProvider::new(vec![Ok((0..6)
            .map(|index| candidate(&format!("a-{index}"), &format!("A{index}")))
            .collect())]);
        let second = StubProvider::new(vec![Ok((0..6)
            .map(|index| candidate(&format!("b-{index}"), &format!("B{index}")))
            .collect())]);
        let catalogue = service(vec![provider(first), provider(second)]);
        let outcome = catalogue.search(&search(), None, &context()).await.unwrap();
        assert_eq!(outcome.hits.len(), 10);
        assert_eq!(outcome.hits[0].track().title(), "A0");
        assert_eq!(outcome.hits[6].track().title(), "B0");

        let resolver = StubProvider::new(vec![Ok((0..25)
            .map(|index| candidate(&format!("source-{index}"), &format!("Song {index}")))
            .collect())]);
        let catalogue = service(vec![provider(resolver)]);
        let outcome = catalogue
            .search(&search_with_offset(10, 10), None, &context())
            .await
            .unwrap();
        assert_eq!(outcome.hits.len(), 10);
        assert_eq!(outcome.hits[0].track().title(), "Song 10");
        assert_eq!(outcome.hits[9].track().title(), "Song 19");

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

        assert_eq!(outcome.hits.len(), 10);
        assert_eq!(outcome.hits[0].track().title(), "P30");
        assert_eq!(outcome.hits[9].track().title(), "P39");
    }

    #[tokio::test]
    async fn slow_sources_are_bounded_by_budget_and_concurrency() {
        let working = StubProvider::new(vec![Ok(vec![candidate("ytm-1", "YTM Song")])]);
        let catalogue = service(vec![provider(HangingProvider), provider(working)]);
        let started = Instant::now();
        let (hits, _error, _success) = catalogue
            .fan_out(&search(), None, &context(), Duration::from_millis(50))
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
