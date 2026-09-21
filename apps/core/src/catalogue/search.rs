use std::{collections::HashMap, sync::Arc, time::Duration};

use tokio::{sync::Semaphore, task::JoinSet};
use uuid::Uuid;

use crate::{
    catalogue::{
        CatalogueCandidate, CatalogueProvider, CatalogueSearch, SearchLimit, SearchOffset,
        SearchQuery,
    },
    providers::ProviderError,
    request::RequestContext,
    tracks::{
        AlbumSummary, AlbumTrack, ArtistSummary, CatalogueWrite, LibraryRead, SearchConfidence,
        SearchTerms, SourceLookup, StateStore, TrackId, best_field_quality,
    },
};

use super::{CatalogueError, DedupMode};

pub use crate::tracks::TrackHit as SearchHit;

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
        search: &CatalogueSearch,
        user: Option<&Uuid>,
        context: &RequestContext,
    ) -> Result<SearchOutcome, CatalogueError> {
        let window = Self::provider_target(search);
        let query = search.query().as_str().to_owned();

        let known = self
            .library
            .search_tracks(&query, window, user.copied())
            .await
            .map_err(CatalogueError::Repository)?;
        let (fresh, first_error, any_success) =
            self.fan_out(search, user, context, PROVIDER_BUDGET).await;
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
            .search_albums(&query, window, user.copied())
            .await
            .map_err(CatalogueError::Repository)?;
        let artists = self
            .library
            .search_artists(&query, window, user.copied())
            .await
            .map_err(CatalogueError::Repository)?;

        Self::finalize(
            search,
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
        search: &CatalogueSearch,
        user: Option<&Uuid>,
        context: &RequestContext,
        budget: Duration,
    ) -> (Vec<SearchHit>, Option<CatalogueError>, bool) {
        let mut tasks = JoinSet::new();
        let concurrency = Arc::new(Semaphore::new(MAX_SOURCE_CONCURRENCY));
        let query = search.query().clone();
        let target = Self::provider_target(search);
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
        search: &CatalogueSearch,
        known: Vec<SearchHit>,
        fresh: Vec<SearchHit>,
        albums: Vec<AlbumSummary>,
        artists: Vec<ArtistSummary>,
        first_error: Option<CatalogueError>,
        any_success: bool,
    ) -> Result<SearchOutcome, CatalogueError> {
        let limit = search.limit().get() as usize;
        let offset = search.offset().get() as usize;
        let terms = SearchTerms::new(search.query().as_str());
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
            library_hits.truncate(Self::library_window(Self::provider_target(search)) as usize);
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
            tracks: ordered.into_iter().skip(offset).take(limit).collect(),
            albums: albums.into_iter().skip(offset).take(limit).collect(),
            artists: artists.into_iter().skip(offset).take(limit).collect(),
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

    fn provider_target(search: &CatalogueSearch) -> u32 {
        u32::from(search.limit().get()).saturating_add(search.offset().get())
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
            hits.extend(Self::materialize(&catalogue, &sources, candidates).await?);
            fetched = fetched.saturating_add(page_len);
            if page_len < page_size {
                break;
            }
        }
        Ok(hits)
    }

    async fn materialize(
        catalogue: &Arc<dyn CatalogueWrite>,
        sources: &Arc<dyn SourceLookup>,
        candidates: Vec<CatalogueCandidate>,
    ) -> Result<Vec<SearchHit>, CatalogueError> {
        let mut hits = Vec::with_capacity(candidates.len());
        for candidate in candidates {
            let source = candidate.source().clone();
            let artwork = candidate.artwork().cloned();
            let album_metadata = candidate.album().cloned();
            let metadata = candidate.metadata().clone();
            let credits = metadata.artists().to_vec();

            let track = catalogue
                .get_or_create(source.clone(), metadata)
                .await
                .map_err(CatalogueError::Repository)?;

            let album = match &album_metadata {
                Some(album_metadata) => {
                    let album = catalogue
                        .resolve_or_create_album(album_metadata)
                        .await
                        .map_err(CatalogueError::Repository)?;
                    if let Some(provider) = album_metadata.provider() {
                        catalogue
                            .set_album_source(album.id(), provider)
                            .await
                            .map_err(CatalogueError::Repository)?;
                    }
                    catalogue
                        .set_album_track(album.id(), track.id(), &AlbumTrack::unplaced())
                        .await
                        .map_err(CatalogueError::Repository)?;

                    Some(album)
                }
                None => {
                    catalogue
                        .clear_album_track(track.id())
                        .await
                        .map_err(CatalogueError::Repository)?;

                    None
                }
            };

            for credit in &credits {
                if let Some(provider) = credit.provider() {
                    let artist = catalogue
                        .resolve_or_create_artist(credit)
                        .await
                        .map_err(CatalogueError::Repository)?;
                    catalogue
                        .set_artist_source(artist.id(), provider)
                        .await
                        .map_err(CatalogueError::Repository)?;
                }
            }

            if let Some(artwork) = artwork
                && let Err(error) = sources.set_source_artwork(&source, &artwork).await
            {
                tracing::warn!(%error, "artwork could not be stored");
            }

            hits.push(SearchHit::new(track, album, vec![source]));
        }
        Ok(hits)
    }
}

const PROVIDER_BUDGET: Duration = Duration::from_secs(45);
const MAX_SOURCE_CONCURRENCY: usize = 4;
const PROVIDER_PAGE_LIMIT: u32 = 25;
const PROVIDER_WINDOW_DIVISOR: u32 = 4;

#[cfg(test)]
mod tests {
    use crate::tracks::{CatalogueWrite, DuplicateReport, LibraryRead, SourceLookup, StateStore};
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
        tracks::{
            AlbumMetadata, ArtistCredit, InMemoryTrackRepository, ProviderId, SourceIdentity,
            SourceScope, Track, TrackMetadata, TrackRepositoryError,
        },
    };
    use uuid::Uuid;

    fn is_owned(sources: &[SourceIdentity]) -> bool {
        sources
            .iter()
            .any(|source| source.provider_id().kind() == Some(crate::tracks::ProviderKind::Local))
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
            _track_id: &crate::tracks::TrackId,
        ) -> Result<Option<Track>, TrackRepositoryError> {
            Err(TrackRepositoryError::Unavailable)
        }
    }

    #[async_trait]
    impl SourceLookup for FailingTrackRepository {
        async fn find_sources(
            &self,
            _track_id: &crate::tracks::TrackId,
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

    struct Fixture {
        store: Arc<InMemoryTrackRepository>,
    }

    impl Fixture {
        fn new() -> Self {
            Self {
                store: Arc::new(InMemoryTrackRepository::default()),
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

    fn service(providers: Vec<Arc<dyn CatalogueProvider>>) -> SearchService {
        Fixture::new().service(providers)
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
                TrackMetadata::from_names("song".to_owned(), vec!["artist".to_owned()], None)
                    .unwrap(),
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
                .tracks
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
        assert_eq!(outcome.tracks.len(), 1);
        assert_eq!(outcome.tracks[0].track().title(), "YTM Song");

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
        assert_eq!(outcome.tracks.len(), 10);
        assert_eq!(outcome.tracks[0].track().title(), "A0");
        assert_eq!(outcome.tracks[6].track().title(), "B0");

        let resolver = StubProvider::new(vec![Ok((0..25)
            .map(|index| candidate(&format!("source-{index}"), &format!("Song {index}")))
            .collect())]);
        let catalogue = service(vec![provider(resolver)]);
        let outcome = catalogue
            .search(&search_with_offset(10, 10), None, &context())
            .await
            .unwrap();
        assert_eq!(outcome.tracks.len(), 10);
        assert_eq!(outcome.tracks[0].track().title(), "Song 10");
        assert_eq!(outcome.tracks[9].track().title(), "Song 19");

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
        assert_eq!(outcome.tracks[0].track().title(), "P30");
        assert_eq!(outcome.tracks[9].track().title(), "P39");
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

    #[tokio::test]
    async fn a_provider_album_is_registered_with_its_source() {
        let repository = Arc::new(InMemoryTrackRepository::default());
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

    #[tokio::test]
    async fn provider_album_and_artist_identity_round_trip_through_materialize() {
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

        let repository = Arc::new(InMemoryTrackRepository::default());
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
        TrackMetadata::from_names(title.to_owned(), vec!["Daft Punk".to_owned()], Some(1_000))
            .unwrap()
    }

    #[tokio::test]
    async fn a_library_without_providers_still_searches() {
        let repository = Arc::new(InMemoryTrackRepository::default());
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

    #[tokio::test]
    async fn a_library_without_providers_answers_a_miss_with_nothing() {
        let catalogue = Fixture::new().bare();

        let outcome = catalogue.search(&search(), None, &context()).await.unwrap();

        assert!(outcome.tracks.is_empty());
    }

    #[tokio::test]
    async fn owned_tracks_outrank_fresh_provider_discoveries() {
        let repository = Arc::new(InMemoryTrackRepository::default());
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

    #[tokio::test]
    async fn a_strong_library_match_is_reported_as_strong() {
        let repository = Arc::new(InMemoryTrackRepository::default());
        repository
            .get_or_create(local_source(), named_metadata("Zebra"))
            .await
            .unwrap();
        let catalogue = Fixture { store: repository }.bare();
        let search = CatalogueSearch::new(
            SearchQuery::new("zebra".to_owned()).unwrap(),
            SearchLimit::new(10).unwrap(),
        );

        let outcome = catalogue.search(&search, None, &context()).await.unwrap();

        assert_eq!(outcome.confidence, SearchConfidence::Strong);
        assert_eq!(outcome.tracks[0].match_quality().unwrap().as_str(), "exact");
    }

    #[tokio::test]
    async fn a_stopword_query_is_weak_confidence() {
        let repository = Arc::new(InMemoryTrackRepository::default());
        repository
            .get_or_create(local_source(), named_metadata("The End"))
            .await
            .unwrap();
        let catalogue = Fixture { store: repository }.bare();
        let search = CatalogueSearch::new(
            SearchQuery::new("the".to_owned()).unwrap(),
            SearchLimit::new(10).unwrap(),
        );

        let outcome = catalogue.search(&search, None, &context()).await.unwrap();

        assert_eq!(outcome.tracks.len(), 1);
        assert_eq!(outcome.confidence, SearchConfidence::Weak);
    }

    #[tokio::test]
    async fn a_weak_query_puts_provider_discoveries_first() {
        let repository = Arc::new(InMemoryTrackRepository::default());
        repository
            .get_or_create(local_source(), named_metadata("The End"))
            .await
            .unwrap();
        let resolver = StubProvider::new(vec![Ok(vec![candidate("vid-1", "Provider Song")])]);
        let catalogue = Fixture { store: repository }.service(vec![provider(resolver)]);
        let search = CatalogueSearch::new(
            SearchQuery::new("the".to_owned()).unwrap(),
            SearchLimit::new(10).unwrap(),
        );

        let outcome = catalogue.search(&search, None, &context()).await.unwrap();

        assert_eq!(outcome.confidence, SearchConfidence::Weak);
        assert!(outcome.tracks[0].match_quality().is_none());
        assert!(outcome.tracks[0].sources()[0].provider_id().as_str() == "youtube_music");
    }

    #[tokio::test]
    async fn the_library_cannot_exhaust_the_page() {
        let repository = Arc::new(InMemoryTrackRepository::default());
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

    #[tokio::test]
    async fn the_library_fills_the_page_when_providers_return_nothing() {
        let repository = Arc::new(InMemoryTrackRepository::default());
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
}
