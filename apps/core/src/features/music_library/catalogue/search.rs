use std::{collections::HashMap, sync::Arc, time::Duration};

use tokio::{sync::Semaphore, task::JoinSet};
use uuid::Uuid;

use crate::{
    features::music_library::catalogue::ProviderError,
    features::music_library::catalogue::{
        CatalogueProvider, CatalogueSearch, SearchLimit, SearchOffset, SearchQuery,
    },
    features::music_library::{
        domain::{
            AlbumSummary, ArtistSummary, SearchConfidence, SearchTerms, TrackId, best_field_quality,
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
}

pub struct SearchOutcome {
    pub tracks: Vec<SearchHit>,
    pub albums: Vec<AlbumSummary>,
    pub artists: Vec<ArtistSummary>,
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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SearchWindows {
    tracks: SearchWindow,
    albums: SearchWindow,
    artists: SearchWindow,
}

impl SearchWindows {
    pub fn new(tracks: SearchWindow, albums: SearchWindow, artists: SearchWindow) -> Self {
        Self {
            tracks,
            albums,
            artists,
        }
    }

    pub fn uniform(window: SearchWindow) -> Self {
        Self::new(window, window, window)
    }

    pub fn tracks(&self) -> SearchWindow {
        self.tracks
    }

    pub fn albums(&self) -> SearchWindow {
        self.albums
    }

    pub fn artists(&self) -> SearchWindow {
        self.artists
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchRequest {
    query: SearchQuery,
    windows: SearchWindows,
}

impl SearchRequest {
    pub fn new(query: SearchQuery, windows: SearchWindows) -> Self {
        Self { query, windows }
    }

    pub fn uniform(query: SearchQuery, window: SearchWindow) -> Self {
        Self::new(query, SearchWindows::uniform(window))
    }

    pub fn query(&self) -> &SearchQuery {
        &self.query
    }

    pub fn windows(&self) -> &SearchWindows {
        &self.windows
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
        }
    }

    pub fn with_dedup_mode(mut self, mode: DedupMode) -> Self {
        self.dedup_mode = mode;
        self
    }

    pub async fn search(
        &self,
        request: &SearchRequest,
        user: Option<&Uuid>,
        context: &RequestContext,
    ) -> Result<SearchOutcome, CatalogueError> {
        let windows = request.windows();
        let query = request.query().as_str().to_owned();

        let known = self
            .library
            .search_tracks(&query, windows.tracks().target(), user.copied())
            .await
            .map_err(CatalogueError::Repository)?;
        let (fresh, first_error, any_success) =
            self.fan_out(request, user, context, PROVIDER_BUDGET).await;
        let (known, fresh) = super::dedup::apply(
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
        )
        .await;
        let albums = self
            .library
            .search_albums(&query, windows.albums().target(), user.copied())
            .await
            .map_err(CatalogueError::Repository)?;
        let artists = self
            .library
            .search_artists(&query, windows.artists().target(), user.copied())
            .await
            .map_err(CatalogueError::Repository)?;

        Self::finalize(
            request,
            known,
            fresh,
            albums,
            artists,
            first_error,
            any_success,
        )
    }

    async fn fan_out(
        &self,
        request: &SearchRequest,
        user: Option<&Uuid>,
        context: &RequestContext,
        budget: Duration,
    ) -> (Vec<SearchHit>, Option<CatalogueError>, bool) {
        let mut tasks = JoinSet::new();
        let concurrency = Arc::new(Semaphore::new(MAX_SOURCE_CONCURRENCY));
        let query = request.query().clone();
        let target = request.windows().tracks().target();
        for (index, provider) in self.providers.iter().enumerate() {
            let permit = Arc::clone(&concurrency);
            let pending = Self::search_source(
                Arc::clone(provider),
                Arc::clone(&self.catalogue),
                Arc::clone(&self.source_lookup),
                query.clone(),
                target,
                user.copied(),
                context.clone(),
            );
            tasks.spawn(async move {
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
        for result in results.into_iter().flatten() {
            match result {
                Ok(page) => {
                    any_success = true;
                    hits.extend(page);
                }
                Err(CatalogueError::Provider(ProviderError::CredentialsRejected)) => {
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
        request: &SearchRequest,
        known: Vec<SearchHit>,
        fresh: Vec<SearchHit>,
        albums: Vec<AlbumSummary>,
        artists: Vec<ArtistSummary>,
        first_error: Option<CatalogueError>,
        any_success: bool,
    ) -> Result<SearchOutcome, CatalogueError> {
        let windows = request.windows();
        let tracks_window = windows.tracks();
        let albums_window = windows.albums();
        let artists_window = windows.artists();
        let terms = SearchTerms::new(request.query().as_str());
        let confidence = Self::library_confidence(&terms, &known);

        let mut positions: HashMap<TrackId, usize> = HashMap::new();
        let mut unique: Vec<SearchHit> = Vec::new();
        for hit in known.into_iter().chain(fresh) {
            match positions.get(hit.track().id()).copied() {
                Some(index) => unique[index].merge(hit),
                None => {
                    positions.insert(hit.track().id().clone(), unique.len());
                    unique.push(hit);
                }
            }
        }

        if !any_success
            && (unique.is_empty() && albums.is_empty() && artists.is_empty())
            && let Some(error) = first_error
        {
            return Err(error);
        }
        if let Some(error @ CatalogueError::Provider(ProviderError::Access)) = first_error {
            return Err(error);
        }

        let (mut library_hits, provider_hits): (Vec<SearchHit>, Vec<SearchHit>) = unique
            .into_iter()
            .partition(|hit| hit.match_quality().is_some());
        if !provider_hits.is_empty() {
            library_hits.truncate(Self::library_window(tracks_window.target()) as usize);
        }
        let ordered = match confidence {
            SearchConfidence::Strong => library_hits
                .into_iter()
                .chain(provider_hits)
                .collect::<Vec<_>>(),
            SearchConfidence::Weak | SearchConfidence::None => provider_hits
                .into_iter()
                .chain(library_hits)
                .collect::<Vec<_>>(),
        };

        Ok(SearchOutcome {
            tracks: ordered
                .into_iter()
                .skip(tracks_window.offset().get() as usize)
                .take(tracks_window.limit().get() as usize)
                .collect(),
            albums: albums
                .into_iter()
                .skip(albums_window.offset().get() as usize)
                .take(albums_window.limit().get() as usize)
                .collect(),
            artists: artists
                .into_iter()
                .skip(artists_window.offset().get() as usize)
                .take(artists_window.limit().get() as usize)
                .collect(),
            confidence,
        })
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

    async fn search_source(
        provider: Arc<dyn CatalogueProvider>,
        catalogue: Arc<dyn CatalogueWrite>,
        sources: Arc<dyn SourceLookup>,
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
            hits.extend(super::materialize::materialize(&catalogue, &sources, candidates).await?);
            fetched = fetched.saturating_add(page_len);
            if page_len < page_size {
                break;
            }
        }
        Ok(hits)
    }
}

const PROVIDER_BUDGET: Duration = Duration::from_secs(45);
const MAX_SOURCE_CONCURRENCY: usize = 4;
const PROVIDER_PAGE_LIMIT: u32 = 25;
const PROVIDER_WINDOW_DIVISOR: u32 = 4;

#[cfg(all(test, feature = "postgres-integration"))]
#[path = "search_tests.rs"]
mod tests;
