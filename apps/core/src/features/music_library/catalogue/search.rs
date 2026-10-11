use std::{collections::HashMap, future::Future, sync::Arc, time::Duration};

use tokio::sync::{Semaphore, mpsc, oneshot};
use tracing::Instrument;
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
    response_budget: Duration,
    concurrency: Arc<Semaphore>,
    outstanding: Arc<Semaphore>,
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
pub enum SearchKind {
    Tracks,
    Albums,
    Artists,
}

pub struct Discovery {
    first_error: Option<CatalogueError>,
    any_success: bool,
    tracks: Option<Vec<SearchHit>>,
    pending: usize,
}

impl Discovery {
    pub fn pending(&self) -> usize {
        self.pending
    }

    pub fn failure(self, found_results: bool) -> Option<CatalogueError> {
        match self.first_error {
            Some(error @ CatalogueError::Provider(ProviderError::Access)) => Some(error),
            Some(error) if !self.any_success && !found_results => Some(error),
            _ => None,
        }
    }

    fn take_tracks(&mut self) -> Option<Vec<SearchHit>> {
        self.tracks.take()
    }
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
            response_budget: DEFAULT_RESPONSE_BUDGET,
            concurrency: Arc::new(Semaphore::new(MAX_SOURCE_CONCURRENCY)),
            outstanding: Arc::new(Semaphore::new(MAX_OUTSTANDING_TASKS)),
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
            response_budget: DEFAULT_RESPONSE_BUDGET,
            concurrency: Arc::new(Semaphore::new(MAX_SOURCE_CONCURRENCY)),
            outstanding: Arc::new(Semaphore::new(MAX_OUTSTANDING_TASKS)),
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

    pub fn with_response_budget(mut self, budget: Duration) -> Self {
        self.response_budget = budget;
        self
    }

    #[tracing::instrument(skip_all, fields(query_len = request.query().as_str().len()))]
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

    #[tracing::instrument(skip_all, fields(kind = ?kind, target))]
    pub async fn discover(
        &self,
        kind: SearchKind,
        query: &SearchQuery,
        target: u32,
        context: &RequestContext,
    ) -> Result<Discovery, CatalogueError> {
        let target = target.min(super::MAX_SEARCH_OFFSET);
        match kind {
            SearchKind::Tracks => {
                let (tracks, mut discovery) = self.discover_tracks(query, target, context).await?;
                discovery.tracks = Some(tracks);
                Ok(discovery)
            }
            SearchKind::Albums => {
                self.discover_page_kind(
                    kind,
                    query,
                    target,
                    context,
                    |provider, search, context| async move {
                        provider.search_albums(&search, &context).await
                    },
                    |catalogue, sources, album| async move {
                        super::materialize::materialize_album(&catalogue, &sources, &album).await?;
                        Ok(())
                    },
                )
                .await
            }
            SearchKind::Artists => {
                self.discover_page_kind(
                    kind,
                    query,
                    target,
                    context,
                    |provider, search, context| async move {
                        provider.search_artists(&search, &context).await
                    },
                    |catalogue, sources, artist| async move {
                        super::materialize::materialize_artist(&catalogue, &sources, &artist)
                            .await?;
                        Ok(())
                    },
                )
                .await
            }
        }
    }

    #[tracing::instrument(skip_all, fields(kind = "track", window = ?window))]
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
        let mut discovery = self
            .discover(SearchKind::Tracks, query, window.target(), context)
            .await?;
        let fresh = discovery.take_tracks().unwrap_or_default();
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

    #[tracing::instrument(skip_all, fields(kind = "album", window = ?window))]
    pub async fn search_albums(
        &self,
        query: &SearchQuery,
        window: SearchWindow,
        user: Option<&Uuid>,
        context: &RequestContext,
    ) -> Result<Vec<AlbumSummary>, CatalogueError> {
        let discovery = self
            .discover(SearchKind::Albums, query, window.target(), context)
            .await?;
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

    #[tracing::instrument(skip_all, fields(kind = "artist", window = ?window))]
    pub async fn search_artists(
        &self,
        query: &SearchQuery,
        window: SearchWindow,
        filter: &ArtistListFilter,
        user: Option<&Uuid>,
        context: &RequestContext,
    ) -> Result<Vec<ArtistSummary>, CatalogueError> {
        let discovery = self
            .discover(SearchKind::Artists, query, window.target(), context)
            .await?;
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
    ) -> Result<(Vec<SearchHit>, Discovery), CatalogueError> {
        let query = query.clone();
        let context = context.clone();
        let catalogue = Arc::clone(&self.catalogue);
        let sources = Arc::clone(&self.source_lookup);
        run_provider_tasks(
            self.providers.clone(),
            Arc::clone(&self.concurrency),
            Arc::clone(&self.outstanding),
            self.search_budget,
            self.response_budget,
            SearchKind::Tracks,
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

    async fn discover_page_kind<P, F, Fut, M, MFut>(
        &self,
        kind: SearchKind,
        query: &SearchQuery,
        target: u32,
        context: &RequestContext,
        fetch: F,
        materialize: M,
    ) -> Result<Discovery, CatalogueError>
    where
        P: Send + 'static,
        F: Fn(Arc<dyn CatalogueProvider>, CatalogueSearch, RequestContext) -> Fut
            + Clone
            + Send
            + Sync
            + 'static,
        Fut: Future<Output = Result<super::domain::CataloguePage<P>, ProviderError>>
            + Send
            + 'static,
        M: Fn(Arc<dyn CatalogueWrite>, Arc<dyn SourceLookup>, P) -> MFut
            + Clone
            + Send
            + Sync
            + 'static,
        MFut: Future<Output = Result<(), CatalogueError>> + Send + 'static,
    {
        let query = query.clone();
        let context = context.clone();
        let catalogue = Arc::clone(&self.catalogue);
        let sources = Arc::clone(&self.source_lookup);
        let (_, discovery) = run_provider_tasks(
            self.providers.clone(),
            Arc::clone(&self.concurrency),
            Arc::clone(&self.outstanding),
            self.search_budget,
            self.response_budget,
            kind,
            move |provider| {
                let query = query.clone();
                let context = context.clone();
                let catalogue = Arc::clone(&catalogue);
                let sources = Arc::clone(&sources);
                let fetch = fetch.clone();
                let materialize = materialize.clone();
                async move {
                    let page =
                        fetch_catalogue_pages(provider, &query, target, &context, fetch).await?;
                    for item in page {
                        materialize(Arc::clone(&catalogue), Arc::clone(&sources), item).await?;
                    }
                    Ok(Vec::<()>::new())
                }
            },
        )
        .await?;
        Ok(discovery)
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

async fn run_provider_tasks<T, F, Fut>(
    providers: Vec<Arc<dyn CatalogueProvider>>,
    concurrency: Arc<Semaphore>,
    outstanding: Arc<Semaphore>,
    search_budget: Duration,
    response_budget: Duration,
    kind: SearchKind,
    operation: F,
) -> Result<(Vec<T>, Discovery), CatalogueError>
where
    T: Send + 'static,
    F: Fn(Arc<dyn CatalogueProvider>) -> Fut + Clone + Send + Sync + 'static,
    Fut: Future<Output = Result<Vec<T>, CatalogueError>> + Send + 'static,
{
    let provider_count = providers.len();
    let deadline = tokio::time::Instant::now() + response_budget;
    let (finished_tx, mut finished_rx) =
        mpsc::unbounded_channel::<(usize, Result<Vec<T>, CatalogueError>)>();
    let mut acknowledgements = (0..provider_count)
        .map(|_| None)
        .collect::<Vec<Option<oneshot::Sender<()>>>>();
    let mut admitted = 0usize;
    for (index, provider) in providers.iter().cloned().enumerate() {
        let Ok(outstanding_permit) = Arc::clone(&outstanding).try_acquire_owned() else {
            tracing::debug!(kind = ?kind, "catalogue provider is pending: outstanding work is saturated");
            continue;
        };
        admitted += 1;
        let permit = Arc::clone(&concurrency);
        let finished = finished_tx.clone();
        let (acknowledge, acknowledged) = oneshot::channel();
        acknowledgements[index] = Some(acknowledge);
        let operation = operation.clone();
        let provider_name = std::any::type_name_of_val(provider.as_ref());
        let span =
            tracing::info_span!("catalogue_provider", provider = provider_name, kind = ?kind);
        let provider_deadline = tokio::time::Instant::now() + search_budget;
        tokio::spawn(
            async move {
                let result = match tokio::time::timeout_at(provider_deadline, async {
                    let _permit = permit
                        .acquire_owned()
                        .await
                        .expect("catalogue concurrency semaphore never closes");
                    if tokio::time::Instant::now() >= provider_deadline {
                        return Err(CatalogueError::Provider(ProviderError::ProviderUnavailable));
                    }
                    operation(provider).await
                })
                .await
                {
                    Ok(result) => result,
                    Err(_) => Err(CatalogueError::Provider(ProviderError::ProviderUnavailable)),
                };
                drop(outstanding_permit);
                let outcome = result.as_ref().err().map(failure_outcome);
                let delivered = finished.send((index, result)).is_ok();
                if delivered && acknowledged.await.is_ok() {
                    return;
                }
                if let Some(outcome) = outcome {
                    tracing::warn!(
                        kind = ?kind,
                        outcome,
                        "catalogue source failed after the response budget",
                    );
                }
            }
            .instrument(span),
        );
    }
    drop(finished_tx);

    let mut results = (0..provider_count)
        .map(|_| None)
        .collect::<Vec<Option<Result<Vec<T>, CatalogueError>>>>();
    let mut finished = 0usize;
    while finished < admitted {
        match tokio::time::timeout_at(deadline, finished_rx.recv()).await {
            Ok(Some((index, result))) => {
                results[index] = Some(result);
                finished += 1;
                if let Some(acknowledge) = acknowledgements[index].take() {
                    let _ = acknowledge.send(());
                }
            }
            Ok(None) | Err(_) => break,
        }
    }
    finished_rx.close();
    drop(acknowledgements);

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
        Discovery {
            first_error: access_error.or(first_error),
            any_success,
            tracks: None,
            pending: provider_count - finished,
        },
    ))
}

fn failure_outcome(error: &CatalogueError) -> &'static str {
    match error {
        CatalogueError::Provider(ProviderError::Access) => "access",
        CatalogueError::Provider(ProviderError::CredentialsRejected) => "credentials_rejected",
        CatalogueError::Provider(ProviderError::UnsupportedProvider) => "unsupported",
        CatalogueError::Provider(ProviderError::SourceNotFound) => "source_not_found",
        CatalogueError::Provider(ProviderError::ProviderUnavailable) => "provider_unavailable",
        CatalogueError::Provider(_) => "provider_error",
        CatalogueError::Repository(_) => "repository",
        CatalogueError::Invalid(_) => "invalid",
    }
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
const DEFAULT_RESPONSE_BUDGET: Duration = Duration::from_secs(3);
const MAX_SOURCE_CONCURRENCY: usize = 4;
const MAX_OUTSTANDING_TASKS: usize = MAX_SOURCE_CONCURRENCY * 4;
pub(super) const PROVIDER_PAGE_LIMIT: u32 = 25;
const PROVIDER_WINDOW_DIVISOR: u32 = 4;

#[cfg(all(test, feature = "postgres-integration"))]
#[path = "search_tests.rs"]
mod tests;
