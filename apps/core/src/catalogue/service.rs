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
    tracks::{
        AlbumEntry, AlbumId, AlbumListFilter, AlbumOrder, AlbumSummary, AlbumTrack, ArtistId,
        ArtistSummary, GenreSummary, LibraryState, ProviderKind, SourceIdentity, Starred,
        StateTarget, Track, TrackId, TrackRepository, TrackRepositoryError,
    },
};

pub use crate::tracks::TrackHit as SearchHit;

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
    pub tracks: Vec<SearchHit>,
    pub albums: Vec<AlbumSummary>,
    pub artists: Vec<ArtistSummary>,
}

pub struct AlbumDetail {
    pub album: AlbumSummary,
    pub entries: Vec<AlbumEntry>,
}

pub struct ArtistDetail {
    pub artist: ArtistSummary,
    pub albums: Vec<AlbumSummary>,
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

    pub async fn track_detail(
        &self,
        track_id: &TrackId,
        user: Option<&Uuid>,
    ) -> Result<Option<SearchHit>, CatalogueError> {
        let Some(track) = self.track(track_id).await? else {
            return Ok(None);
        };
        let album = self
            .track_repository
            .find_album_for_track(track_id)
            .await
            .map_err(CatalogueError::Repository)?;
        let sources = self.sources(track_id).await?;
        let state = match user {
            Some(user) => self
                .track_repository
                .find_track_state(track_id, *user)
                .await
                .map_err(CatalogueError::Repository)?,
            None => LibraryState::default(),
        };

        Ok(Some(
            SearchHit::new(track, album, sources).with_state(state),
        ))
    }

    pub async fn album_list(
        &self,
        offset: u32,
        limit: u32,
        order: AlbumOrder,
        filter: AlbumListFilter,
        user: Option<&Uuid>,
    ) -> Result<Vec<AlbumSummary>, CatalogueError> {
        self.track_repository
            .list_albums(offset, limit, order, filter, user.copied())
            .await
            .map_err(CatalogueError::Repository)
    }

    pub async fn genres(&self) -> Result<Vec<GenreSummary>, CatalogueError> {
        self.track_repository
            .list_genres()
            .await
            .map_err(CatalogueError::Repository)
    }

    pub async fn starred(&self, user: Uuid) -> Result<Starred, CatalogueError> {
        self.track_repository
            .starred(user)
            .await
            .map_err(CatalogueError::Repository)
    }

    pub async fn set_starred(
        &self,
        user: Uuid,
        target: StateTarget<'_>,
        starred: bool,
    ) -> Result<(), CatalogueError> {
        self.track_repository
            .set_starred(user, target, starred)
            .await
            .map_err(CatalogueError::Repository)
    }

    pub async fn set_rating(
        &self,
        user: Uuid,
        target: StateTarget<'_>,
        rating: Option<u8>,
    ) -> Result<(), CatalogueError> {
        self.track_repository
            .set_rating(user, target, rating)
            .await
            .map_err(CatalogueError::Repository)
    }

    pub async fn rate_id(
        &self,
        user: Uuid,
        raw_id: &str,
        rating: Option<u8>,
    ) -> Result<bool, CatalogueError> {
        if let Ok(track_id) = raw_id.parse::<TrackId>()
            && self.track(&track_id).await?.is_some()
        {
            self.set_rating(user, StateTarget::Track(&track_id), rating)
                .await?;
            return Ok(true);
        }
        if let Ok(album_id) = raw_id.parse::<AlbumId>()
            && self
                .track_repository
                .find_album(&album_id)
                .await
                .map_err(CatalogueError::Repository)?
                .is_some()
        {
            self.set_rating(user, StateTarget::Album(&album_id), rating)
                .await?;
            return Ok(true);
        }
        if let Ok(artist_id) = raw_id.parse::<ArtistId>()
            && self
                .track_repository
                .find_artist(&artist_id, Some(user))
                .await
                .map_err(CatalogueError::Repository)?
                .is_some()
        {
            self.set_rating(user, StateTarget::Artist(&artist_id), rating)
                .await?;
            return Ok(true);
        }

        Ok(false)
    }

    pub async fn record_play(
        &self,
        user: Uuid,
        track_id: &TrackId,
        played_at: chrono::DateTime<chrono::Utc>,
    ) -> Result<(), CatalogueError> {
        self.track_repository
            .record_play(user, track_id, played_at)
            .await
            .map_err(CatalogueError::Repository)
    }

    pub async fn album_count(&self) -> Result<u32, CatalogueError> {
        self.track_repository
            .count_albums()
            .await
            .map_err(CatalogueError::Repository)
    }

    pub async fn artist_count(&self) -> Result<u32, CatalogueError> {
        self.track_repository
            .count_artists()
            .await
            .map_err(CatalogueError::Repository)
    }

    pub async fn album_detail(
        &self,
        album_id: &AlbumId,
        user: Option<&Uuid>,
    ) -> Result<Option<AlbumDetail>, CatalogueError> {
        let Some(album) = self
            .track_repository
            .find_album_summary(album_id, user.copied())
            .await
            .map_err(CatalogueError::Repository)?
        else {
            return Ok(None);
        };

        let entries = self
            .track_repository
            .find_album_entries(album_id, user.copied())
            .await
            .map_err(CatalogueError::Repository)?;

        Ok(Some(AlbumDetail { album, entries }))
    }

    pub async fn artist_list(
        &self,
        offset: u32,
        limit: u32,
        user: Option<&Uuid>,
    ) -> Result<Vec<ArtistSummary>, CatalogueError> {
        self.track_repository
            .list_artists(offset, limit, user.copied())
            .await
            .map_err(CatalogueError::Repository)
    }

    pub async fn artist_detail(
        &self,
        artist_id: &ArtistId,
        user: Option<&Uuid>,
    ) -> Result<Option<ArtistDetail>, CatalogueError> {
        let Some(artist) = self
            .track_repository
            .find_artist(artist_id, user.copied())
            .await
            .map_err(CatalogueError::Repository)?
        else {
            return Ok(None);
        };

        let albums = self
            .track_repository
            .find_albums_for_artist(artist_id, user.copied())
            .await
            .map_err(CatalogueError::Repository)?;
        let state = match user {
            Some(user) => self
                .track_repository
                .find_artist_state(artist_id, *user)
                .await
                .map_err(CatalogueError::Repository)?,
            None => LibraryState::default(),
        };
        let artist = ArtistSummary::new(artist, albums.len() as u32).with_state(state);

        Ok(Some(ArtistDetail { artist, albums }))
    }

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
        let window = provider_target(search);
        let query = search.query().as_str().to_owned();

        let known = self
            .track_repository
            .search_tracks(&query, window, user.copied())
            .await
            .map_err(CatalogueError::Repository)?;
        let (fresh, first_error, any_success) =
            self.fan_out(search, user, context, PROVIDER_BUDGET).await;
        let albums = self
            .track_repository
            .search_albums(&query, window, user.copied())
            .await
            .map_err(CatalogueError::Repository)?;
        let artists = self
            .track_repository
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
        known: Vec<SearchHit>,
        fresh: Vec<SearchHit>,
        albums: Vec<AlbumSummary>,
        artists: Vec<ArtistSummary>,
        first_error: Option<CatalogueError>,
        any_success: bool,
    ) -> Result<SearchOutcome, CatalogueError> {
        let limit = search.limit().get() as usize;
        let offset = search.offset().get() as usize;
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
        unique.sort_by_key(|hit| !is_owned(hit.sources()));

        if !any_success
            && (unique.is_empty() && albums.is_empty() && artists.is_empty())
            && let Some(error) = first_error
        {
            return Err(error);
        }
        if let Some(error @ CatalogueError::Provider(ProviderError::Access)) = first_error {
            return Err(error);
        }

        Ok(SearchOutcome {
            tracks: unique.into_iter().skip(offset).take(limit).collect(),
            albums: albums.into_iter().skip(offset).take(limit).collect(),
            artists: artists.into_iter().skip(offset).take(limit).collect(),
        })
    }
}

fn is_owned(sources: &[SourceIdentity]) -> bool {
    sources
        .iter()
        .any(|source| source.provider_id().kind() == Some(ProviderKind::Local))
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
        let album_metadata = candidate.album().cloned();
        let metadata = candidate.metadata().clone();

        let track = repository
            .get_or_create(source.clone(), metadata)
            .await
            .map_err(CatalogueError::Repository)?;

        let album = match &album_metadata {
            Some(metadata) => {
                let album = repository
                    .resolve_or_create_album(metadata)
                    .await
                    .map_err(CatalogueError::Repository)?;
                repository
                    .set_album_source(album.id(), &source)
                    .await
                    .map_err(CatalogueError::Repository)?;
                repository
                    .set_album_track(album.id(), track.id(), &AlbumTrack::unplaced())
                    .await
                    .map_err(CatalogueError::Repository)?;

                Some(album)
            }
            None => {
                repository
                    .clear_album_track(track.id())
                    .await
                    .map_err(CatalogueError::Repository)?;

                None
            }
        };

        if let Some(artwork) = artwork
            && let Err(error) = repository.set_source_artwork(&source, &artwork).await
        {
            tracing::warn!(%error, "artwork could not be stored");
        }

        hits.push(SearchHit::new(track, album, vec![source]));
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
        tracks::{
            AlbumMetadata, ArtistCredit, InMemoryTrackRepository, ProviderId, SourceIdentity,
            SourceScope, TrackMetadata,
        },
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

    #[tokio::test]
    async fn a_provider_album_is_registered_with_its_source() {
        let repository = Arc::new(InMemoryTrackRepository::default());
        let base = candidate("ytm-album-track", "Song");
        let source = base.source().clone();
        let album_metadata = AlbumMetadata::new("Discovery".to_owned())
            .unwrap()
            .with_album_artist(ArtistCredit::new("Daft Punk".to_owned(), None).unwrap());
        let candidate = CatalogueCandidate::new(source, base.metadata().clone())
            .with_album(Some(album_metadata.clone()));
        let resolver = StubProvider::new(vec![Ok(vec![candidate])]);
        let catalogue =
            CatalogueService::with_providers(repository.clone(), vec![provider(resolver)]);

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

    fn local_source() -> SourceIdentity {
        SourceIdentity::new(
            ProviderId::new("local".to_owned()).unwrap(),
            SourceScope::Named("/music".to_owned()),
            "path-a".to_owned(),
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
        let catalogue = CatalogueService::new(repository);

        let outcome = catalogue.search(&search(), None, &context()).await.unwrap();

        assert_eq!(outcome.tracks.len(), 1);
        assert_eq!(outcome.tracks[0].track().title(), "Zebra");
        assert!(is_owned(outcome.tracks[0].sources()));
    }

    #[tokio::test]
    async fn owned_tracks_outrank_fresh_provider_discoveries() {
        let repository = Arc::new(InMemoryTrackRepository::default());
        repository
            .get_or_create(local_source(), named_metadata("Zebra"))
            .await
            .unwrap();
        let resolver = StubProvider::new(vec![Ok(vec![candidate("vid-1", "Apple")])]);
        let catalogue = CatalogueService::with_providers(repository, vec![provider(resolver)]);

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
}
