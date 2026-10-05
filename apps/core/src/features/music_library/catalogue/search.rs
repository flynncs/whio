use std::{collections::HashMap, future::Future, sync::Arc, time::Duration};

use tokio::{sync::Semaphore, task::JoinSet};
use uuid::Uuid;

use crate::{
    features::music_library::catalogue::ProviderError,
    features::music_library::catalogue::{
        CatalogueProvider, CatalogueSearch, SearchLimit, SearchOffset, SearchQuery,
    },
    features::music_library::{
        domain::{
            AlbumSummary, ArtistListFilter, ArtistSummary, SearchConfidence, SearchTerms, TrackId,
            ValidationError, best_field_quality,
        },
        storage::contracts::{CatalogueWrite, LibraryRead, SourceLookup, StateStore},
    },
    platform::request::RequestContext,
};

use super::{CatalogueError, DedupMode};

pub use crate::features::music_library::domain::TrackHit as SearchHit;

pub struct SearchService {
    library: Arc<dyn LibraryRead>,
    catalogue: Arc<dyn CatalogueWrite>,
    source_lookup: Arc<dyn SourceLookup>,
    state: Arc<dyn StateStore>,
    providers: Vec<Arc<dyn CatalogueProvider>>,
    dedup_mode: DedupMode,
    dedup_budget: Duration,
    search_budget: Duration,
    concurrency: Arc<Semaphore>,
}

pub struct SearchOutcome {
    pub tracks: Vec<SearchHit>,
    pub albums: Vec<AlbumSummary>,
    pub artists: Vec<ArtistSummary>,
    pub confidence: SearchConfidence,
}

impl SearchOutcome {
    fn is_empty(&self) -> bool {
        self.tracks.is_empty() && self.albums.is_empty() && self.artists.is_empty()
    }
}

pub struct TrackSearch {
    pub tracks: Vec<SearchHit>,
    pub confidence: SearchConfidence,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SearchWindow {
    limit: SearchLimit,
    offset: SearchOffset,
}

impl SearchWindow {
    pub fn new(limit: SearchLimit, offset: SearchOffset) -> Self {
        Self { limit, offset }
    }

    pub fn limit(self) -> SearchLimit {
        self.limit
    }

    pub fn offset(self) -> SearchOffset {
        self.offset
    }

    fn target(self) -> u32 {
        u32::from(self.limit.get()).saturating_add(self.offset.get())
    }

    fn slice<T>(self, items: Vec<T>) -> Vec<T> {
        items
            .into_iter()
            .skip(self.offset.get() as usize)
            .take(self.limit.get() as usize)
            .collect()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SearchWindows {
    tracks: Option<SearchWindow>,
    albums: Option<SearchWindow>,
    artists: Option<SearchWindow>,
}

impl SearchWindows {
    pub fn new(
        tracks: Option<SearchWindow>,
        albums: Option<SearchWindow>,
        artists: Option<SearchWindow>,
    ) -> Self {
        Self {
            tracks,
            albums,
            artists,
        }
    }

    pub fn uniform(window: SearchWindow) -> Self {
        Self::new(Some(window), Some(window), Some(window))
    }

    pub fn tracks(&self) -> Option<SearchWindow> {
        self.tracks
    }

    pub fn albums(&self) -> Option<SearchWindow> {
        self.albums
    }

    pub fn artists(&self) -> Option<SearchWindow> {
        self.artists
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchRequest {
    query: SearchQuery,
    windows: SearchWindows,
    artist_filter: ArtistListFilter,
}

impl SearchRequest {
    pub fn new(
        query: SearchQuery,
        windows: SearchWindows,
        artist_filter: ArtistListFilter,
    ) -> Self {
        Self {
            query,
            windows,
            artist_filter,
        }
    }

    pub fn uniform(query: SearchQuery, window: SearchWindow) -> Self {
        Self::new(
            query,
            SearchWindows::uniform(window),
            ArtistListFilter::default(),
        )
    }

    pub fn query(&self) -> &SearchQuery {
        &self.query
    }

    pub fn windows(&self) -> &SearchWindows {
        &self.windows
    }

    pub fn artist_filter(&self) -> &ArtistListFilter {
        &self.artist_filter
    }
}

impl SearchService {
    pub fn new(
        library: Arc<dyn LibraryRead>,
        catalogue: Arc<dyn CatalogueWrite>,
        sources: Arc<dyn SourceLookup>,
        state: Arc<dyn StateStore>,
    ) -> Self {
        Self {
            library,
            catalogue,
            source_lookup: sources,
            state,
            providers: Vec::new(),
            dedup_mode: DedupMode::Suppress,
            dedup_budget: super::dedup::DEFAULT_DEDUP_BUDGET,
            search_budget: DEFAULT_SEARCH_BUDGET,
            concurrency: Arc::new(Semaphore::new(MAX_SOURCE_CONCURRENCY)),
        }
    }

    pub fn with_providers(
        library: Arc<dyn LibraryRead>,
        catalogue: Arc<dyn CatalogueWrite>,
        sources: Arc<dyn SourceLookup>,
        state: Arc<dyn StateStore>,
        providers: Vec<Arc<dyn CatalogueProvider>>,
    ) -> Self {
        Self {
            library,
            catalogue,
            source_lookup: sources,
            state,
            providers,
            dedup_mode: DedupMode::Suppress,
            dedup_budget: super::dedup::DEFAULT_DEDUP_BUDGET,
            search_budget: DEFAULT_SEARCH_BUDGET,
            concurrency: Arc::new(Semaphore::new(MAX_SOURCE_CONCURRENCY)),
        }
    }

    pub fn with_dedup_mode(mut self, mode: DedupMode) -> Self {
        self.dedup_mode = mode;
        self
    }

    pub fn with_dedup_budget(mut self, budget: Duration) -> Self {
        self.dedup_budget = budget;
        self
    }

    pub fn with_search_budget(mut self, budget: Duration) -> Self {
        self.search_budget = budget;
        self
    }

    pub async fn search(
        &self,
        request: &SearchRequest,
        user: Option<&Uuid>,
        context: &RequestContext,
    ) -> Result<SearchOutcome, CatalogueError> {
        let windows = request.windows();
        let query = request.query();
        let (tracks, albums, artists) = tokio::join!(
            when(windows.tracks(), |window| self
                .search_tracks(query, window, user, context)),
            when(windows.albums(), |window| self
                .search_albums(query, window, user, context)),
            when(windows.artists(), |window| self.search_artists(
                query,
                window,
                request.artist_filter(),
                user,
                context
            )),
        );

        let mut provider_failure = None;
        let tracks = degrade_provider_failure(tracks, &mut provider_failure)?;
        let albums = degrade_provider_failure(albums, &mut provider_failure)?.unwrap_or_default();
        let artists = degrade_provider_failure(artists, &mut provider_failure)?.unwrap_or_default();

        let (tracks, confidence) = match tracks {
            Some(track) => (track.tracks, track.confidence),
            None => (Vec::new(), SearchConfidence::None),
        };
        let outcome = SearchOutcome {
            tracks,
            albums,
            artists,
            confidence,
        };

        match provider_failure {
            Some(error) if outcome.is_empty() => Err(error),
            _ => Ok(outcome),
        }
    }

    pub async fn search_tracks(
        &self,
        query: &SearchQuery,
        window: SearchWindow,
        user: Option<&Uuid>,
        context: &RequestContext,
    ) -> Result<TrackSearch, CatalogueError> {
        let known = self
            .library
            .search_tracks(query.as_str(), window.target(), user.copied())
            .await
            .map_err(CatalogueError::Repository)?;
        let (fresh, discovery) = self
            .discover_tracks(query, window.target(), context)
            .await?;
        let (known, fresh) = self.dedup_tracks(known, fresh, user, context).await;
        let confidence = library_confidence(&SearchTerms::new(query.as_str()), &known);
        let merged = merge_by_track(known.into_iter().chain(fresh));
        if let Some(error) = discovery.failure(!merged.is_empty()) {
            return Err(error);
        }

        Ok(TrackSearch {
            tracks: window.slice(order_by_confidence(merged, confidence, window.target())),
            confidence,
        })
    }

    async fn dedup_tracks(
        &self,
        known: Vec<SearchHit>,
        fresh: Vec<SearchHit>,
        user: Option<&Uuid>,
        context: &RequestContext,
    ) -> (Vec<SearchHit>, Vec<SearchHit>) {
        super::dedup::apply(
            super::dedup::Ports {
                library: &self.library,
                sources: &self.source_lookup,
                state: &self.state,
            },
            known,
            fresh,
            user.copied(),
            context,
            self.dedup_mode,
            self.dedup_budget,
        )
        .await
    }

    pub async fn search_albums(
        &self,
        query: &SearchQuery,
        window: SearchWindow,
        user: Option<&Uuid>,
        context: &RequestContext,
    ) -> Result<Vec<AlbumSummary>, CatalogueError> {
        let (metadata, discovery) = self
            .discover_page_kind(
                query,
                window.target(),
                context,
                |provider, search, context| async move {
                    provider.search_albums(&search, &context).await
                },
            )
            .await?;
        for album in &metadata {
            super::materialize::materialize_album(&self.catalogue, &self.source_lookup, album)
                .await?;
        }
        let albums = self
            .library
            .search_albums(query.as_str(), window.target(), user.copied())
            .await
            .map_err(CatalogueError::Repository)?;
        if let Some(error) = discovery.failure(!albums.is_empty()) {
            return Err(error);
        }
        Ok(window.slice(albums))
    }

    pub async fn search_artists(
        &self,
        query: &SearchQuery,
        window: SearchWindow,
        filter: &ArtistListFilter,
        user: Option<&Uuid>,
        context: &RequestContext,
    ) -> Result<Vec<ArtistSummary>, CatalogueError> {
        let (credits, discovery) = self
            .discover_page_kind(
                query,
                window.target(),
                context,
                |provider, search, context| async move {
                    provider.search_artists(&search, &context).await
                },
            )
            .await?;
        for credit in &credits {
            super::materialize::materialize_artist(&self.catalogue, &self.source_lookup, credit)
                .await?;
        }
        let artists = self
            .library
            .search_artists(query.as_str(), window.target(), user.copied(), filter)
            .await
            .map_err(CatalogueError::Repository)?;
        if let Some(error) = discovery.failure(!artists.is_empty()) {
            return Err(error);
        }
        Ok(window.slice(artists))
    }

    async fn discover_tracks(
        &self,
        query: &SearchQuery,
        target: u32,
        context: &RequestContext,
    ) -> Result<(Vec<SearchHit>, DiscoveryStatus), CatalogueError> {
        let query = query.clone();
        let context = context.clone();
        let catalogue = Arc::clone(&self.catalogue);
        let sources = Arc::clone(&self.source_lookup);
        run_provider_tasks(
            self.providers.clone(),
            Arc::clone(&self.concurrency),
            self.search_budget,
            move |provider| {
                let query = query.clone();
                let context = context.clone();
                let catalogue = Arc::clone(&catalogue);
                let sources = Arc::clone(&sources);
                async move {
                    Self::search_source(provider, catalogue, sources, query, target, context).await
                }
            },
        )
        .await
    }

    async fn discover_page_kind<T, F, Fut>(
        &self,
        query: &SearchQuery,
        target: u32,
        context: &RequestContext,
        fetch: F,
    ) -> Result<(Vec<T>, DiscoveryStatus), CatalogueError>
    where
        T: Send + 'static,
        F: Fn(Arc<dyn CatalogueProvider>, CatalogueSearch, RequestContext) -> Fut
            + Clone
            + Send
            + Sync
            + 'static,
        Fut: Future<Output = Result<super::domain::CataloguePage<T>, ProviderError>>
            + Send
            + 'static,
    {
        let query = query.clone();
        let context = context.clone();
        run_provider_tasks(
            self.providers.clone(),
            Arc::clone(&self.concurrency),
            self.search_budget,
            move |provider| {
                let query = query.clone();
                let context = context.clone();
                let fetch = fetch.clone();
                async move {
                    fetch_catalogue_pages(provider, &query, target, &context, fetch).await
                }
            },
        )
        .await
    }

    async fn search_source(
        provider: Arc<dyn CatalogueProvider>,
        catalogue: Arc<dyn CatalogueWrite>,
        sources: Arc<dyn SourceLookup>,
        query: SearchQuery,
        target: u32,
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
                .search_tracks(&search, &context)
                .await
                .map_err(CatalogueError::Provider)?;
            let page_len = candidates.len() as u32;
            hits.extend(super::materialize::materialize(&catalogue, &sources, candidates).await?);
            fetched = fetched.saturating_add(page_len);
            if page_len < page_size {
                break;
            }
        }
        Ok(hits)
    }
}

struct DiscoveryStatus {
    first_error: Option<CatalogueError>,
    any_success: bool,
}

async fn run_provider_tasks<T, F, Fut>(
    providers: Vec<Arc<dyn CatalogueProvider>>,
    concurrency: Arc<Semaphore>,
    budget: Duration,
    operation: F,
) -> Result<(Vec<T>, DiscoveryStatus), CatalogueError>
where
    T: Send + 'static,
    F: Fn(Arc<dyn CatalogueProvider>) -> Fut + Clone + Send + Sync + 'static,
    Fut: Future<Output = Result<Vec<T>, CatalogueError>> + Send + 'static,
{
    let mut tasks = JoinSet::new();
    for (index, provider) in providers.iter().cloned().enumerate() {
        let permit = Arc::clone(&concurrency);
        let operation = operation.clone();
        tasks.spawn(async move {
            let _permit = permit
                .acquire_owned()
                .await
                .expect("catalogue concurrency semaphore never closes");
            match tokio::time::timeout(budget, operation(provider)).await {
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

    let mut results = (0..providers.len())
        .map(|_| None)
        .collect::<Vec<Option<Result<Vec<T>, CatalogueError>>>>();
    while let Some(result) = tasks.join_next().await {
        let (index, result) = result.expect("search tasks never panic");
        results[index] = Some(result);
    }

    let mut items = Vec::new();
    let mut first_error = None;
    let mut access_error = None;
    let mut any_success = false;
    for result in results.into_iter().flatten() {
        match result {
            Ok(found) => {
                any_success = true;
                items.extend(found);
            }
            Err(CatalogueError::Provider(ProviderError::UnsupportedProvider)) => {}
            Err(CatalogueError::Provider(ProviderError::CredentialsRejected)) => {
                tracing::warn!("catalogue source credentials were rejected");
            }
            Err(error @ CatalogueError::Repository(_))
            | Err(error @ CatalogueError::Invalid(_)) => return Err(error),
            Err(error) => {
                tracing::warn!(%error, "catalogue source skipped");
                if matches!(error, CatalogueError::Provider(ProviderError::Access)) {
                    access_error.get_or_insert(error);
                } else {
                    first_error.get_or_insert(error);
                }
            }
        }
    }
    Ok((
        items,
        DiscoveryStatus {
            first_error: access_error.or(first_error),
            any_success,
        },
    ))
}

async fn fetch_catalogue_pages<T, F, Fut>(
    provider: Arc<dyn CatalogueProvider>,
    query: &SearchQuery,
    target: u32,
    context: &RequestContext,
    mut fetch: F,
) -> Result<Vec<T>, CatalogueError>
where
    F: FnMut(Arc<dyn CatalogueProvider>, CatalogueSearch, RequestContext) -> Fut,
    Fut: Future<Output = Result<super::domain::CataloguePage<T>, ProviderError>>,
{
    let mut items = Vec::new();
    let mut offset = 0u32;
    while offset < target {
        let limit = (target - offset).min(PROVIDER_PAGE_LIMIT);
        let search = CatalogueSearch::new_with_offset(
            query.clone(),
            SearchLimit::new(limit).expect("bounded provider limit is valid"),
            SearchOffset::new(offset).expect("provider offset is bounded"),
        );
        let expected_next = offset + limit;
        let page = fetch(Arc::clone(&provider), search, context.clone())
            .await
            .map_err(CatalogueError::Provider)?;
        let (found, next_offset) = page.into_parts();
        if found.len() as u32 > limit {
            return Err(invalid_provider_cursor());
        }
        items.extend(found);
        match next_offset {
            Some(next) if next.get() == expected_next && next.get() <= super::MAX_SEARCH_OFFSET => {
                offset = next.get();
            }
            Some(_) => return Err(invalid_provider_cursor()),
            None => break,
        }
    }
    Ok(items)
}

fn invalid_provider_cursor() -> CatalogueError {
    CatalogueError::Provider(ProviderError::InvalidCatalogueData(
        ValidationError::Invalid { field: "offset" },
    ))
}

impl DiscoveryStatus {
    fn failure(self, found_results: bool) -> Option<CatalogueError> {
        match self.first_error {
            Some(error @ CatalogueError::Provider(ProviderError::Access)) => Some(error),
            Some(error) if !self.any_success && !found_results => Some(error),
            _ => None,
        }
    }
}

fn library_confidence(terms: &SearchTerms, hits: &[SearchHit]) -> SearchConfidence {
    if hits.is_empty() {
        return SearchConfidence::None;
    }
    if terms.low_signal() {
        return SearchConfidence::Weak;
    }

    let best = best_field_quality(
        terms,
        hits.iter().flat_map(|hit| {
            std::iter::once(hit.track().title())
                .chain(hit.track().artists().iter().map(|artist| artist.name()))
        }),
    );

    match best {
        Some(quality) if quality.is_confident() => SearchConfidence::Strong,
        _ => SearchConfidence::Weak,
    }
}

fn library_window(window: u32) -> u32 {
    if window <= 1 {
        return window;
    }

    window - window.div_ceil(PROVIDER_WINDOW_DIVISOR)
}

fn degrade_provider_failure<T>(
    result: Option<Result<T, CatalogueError>>,
    provider_failure: &mut Option<CatalogueError>,
) -> Result<Option<T>, CatalogueError> {
    match result {
        Some(Ok(value)) => Ok(Some(value)),
        Some(Err(error @ CatalogueError::Provider(ProviderError::Access))) => Err(error),
        Some(Err(error @ CatalogueError::Provider(_))) => {
            provider_failure.get_or_insert(error);

            Ok(None)
        }
        Some(Err(error)) => Err(error),
        None => Ok(None),
    }
}

fn merge_by_track(hits: impl IntoIterator<Item = SearchHit>) -> Vec<SearchHit> {
    let mut positions: HashMap<TrackId, usize> = HashMap::new();
    let mut unique: Vec<SearchHit> = Vec::new();
    for hit in hits {
        match positions.get(hit.track().id()).copied() {
            Some(index) => unique[index].merge(hit),
            None => {
                positions.insert(hit.track().id().clone(), unique.len());
                unique.push(hit);
            }
        }
    }

    unique
}

fn order_by_confidence(
    hits: Vec<SearchHit>,
    confidence: SearchConfidence,
    target: u32,
) -> Vec<SearchHit> {
    let (mut library_hits, provider_hits): (Vec<SearchHit>, Vec<SearchHit>) = hits
        .into_iter()
        .partition(|hit| hit.match_quality().is_some());
    if !provider_hits.is_empty() {
        library_hits.truncate(library_window(target) as usize);
    }

    match confidence {
        SearchConfidence::Strong => library_hits.into_iter().chain(provider_hits).collect(),
        SearchConfidence::Weak | SearchConfidence::None => {
            provider_hits.into_iter().chain(library_hits).collect()
        }
    }
}

async fn when<T, F, Fut>(
    window: Option<SearchWindow>,
    search: F,
) -> Option<Result<T, CatalogueError>>
where
    F: FnOnce(SearchWindow) -> Fut,
    Fut: std::future::Future<Output = Result<T, CatalogueError>>,
{
    match window {
        Some(window) => Some(search(window).await),
        None => None,
    }
}

const DEFAULT_SEARCH_BUDGET: Duration = Duration::from_secs(45);
const MAX_SOURCE_CONCURRENCY: usize = 4;
const PROVIDER_PAGE_LIMIT: u32 = 25;
const PROVIDER_WINDOW_DIVISOR: u32 = 4;

#[cfg(all(test, feature = "postgres-integration"))]
#[path = "search_tests.rs"]
mod tests;
