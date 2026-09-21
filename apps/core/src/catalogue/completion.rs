use std::{sync::Arc, time::Duration};

use chrono::Utc;
use uuid::Uuid;

use crate::{
    providers::ProviderError,
    request::{RequestContext, RequestId},
    tracks::{
        AlbumId, CatalogueWrite, ProviderEntityState, ProviderRegistry, SourceIdentity,
        SourceLookup, TrackRepositoryError,
    },
};

use super::{AlbumDetail, CatalogueError, CatalogueProvider, FetchedAlbum, LibraryService};

const COMPLETION_BUDGET: Duration = Duration::from_secs(30);
const MAX_ERROR: usize = 500;

pub struct CompletionService {
    library: Arc<LibraryService>,
    catalogue: Option<Arc<dyn CatalogueWrite>>,
    sources: Option<Arc<dyn SourceLookup>>,
    providers: Vec<Arc<dyn CatalogueProvider>>,
    registry: ProviderRegistry,
    budget: Duration,
}

impl CompletionService {
    pub fn new(
        library: Arc<LibraryService>,
        catalogue: Arc<dyn CatalogueWrite>,
        sources: Arc<dyn SourceLookup>,
        providers: Vec<Arc<dyn CatalogueProvider>>,
    ) -> Self {
        Self {
            library,
            catalogue: Some(catalogue),
            sources: Some(sources),
            providers,
            registry: ProviderRegistry::builtin(),
            budget: COMPLETION_BUDGET,
        }
    }

    pub fn disabled(library: Arc<LibraryService>) -> Self {
        Self {
            library,
            catalogue: None,
            sources: None,
            providers: Vec::new(),
            registry: ProviderRegistry::builtin(),
            budget: COMPLETION_BUDGET,
        }
    }

    pub fn with_registry(mut self, registry: ProviderRegistry) -> Self {
        self.registry = registry;
        self
    }

    pub fn with_budget(mut self, budget: Duration) -> Self {
        self.budget = budget;
        self
    }

    pub async fn album_detail(
        &self,
        album_id: &AlbumId,
        user: Option<&Uuid>,
    ) -> Result<Option<AlbumDetail>, CatalogueError> {
        let (Some(catalogue), Some(sources)) = (&self.catalogue, &self.sources) else {
            return self.library.album_detail(album_id, user).await;
        };

        let Some(detail) = self.library.album_detail(album_id, user).await? else {
            return Ok(None);
        };

        if self.owned(&detail) {
            return Ok(Some(detail));
        }

        let provider_source = sources
            .find_album_sources(album_id)
            .await
            .map_err(CatalogueError::Repository)?
            .into_iter()
            .find(|source| !self.registry.is_library(source.provider_id()));

        let Some(provider_source) = provider_source else {
            return Ok(Some(detail));
        };

        let member_count = detail.entries.len() as u32;
        if self
            .settled(sources, &provider_source, member_count)
            .await?
        {
            return Ok(Some(detail));
        }

        match self.fetch(&provider_source).await {
            Ok(fetched) => {
                let reported_count = fetched.reported_count();
                super::materialize::materialize(catalogue, sources, fetched.into_candidates())
                    .await?;
                let state = self.state(reported_count, None)?;
                catalogue
                    .set_provider_entity_state(&provider_source, &state)
                    .await
                    .map_err(CatalogueError::Repository)?;

                self.library.album_detail(album_id, user).await
            }
            Err(error) => {
                let state = self.state(None, Some(error.to_string()))?;
                catalogue
                    .set_provider_entity_state(&provider_source, &state)
                    .await
                    .map_err(CatalogueError::Repository)?;

                Ok(Some(detail))
            }
        }
    }

    fn owned(&self, detail: &AlbumDetail) -> bool {
        detail.entries.iter().any(|entry| {
            entry
                .sources()
                .iter()
                .any(|source| self.registry.is_library(source.provider_id()))
        })
    }

    async fn settled(
        &self,
        sources: &Arc<dyn SourceLookup>,
        provider_source: &SourceIdentity,
        member_count: u32,
    ) -> Result<bool, CatalogueError> {
        let Some(state) = sources
            .find_provider_entity_state(provider_source)
            .await
            .map_err(CatalogueError::Repository)?
        else {
            return Ok(false);
        };

        Ok(state.last_error().is_none() && !state.is_partial(member_count))
    }

    async fn fetch(&self, source: &SourceIdentity) -> Result<FetchedAlbum, CatalogueError> {
        let context = RequestContext::new(RequestId::generate());

        for provider in &self.providers {
            match tokio::time::timeout(self.budget, provider.fetch_album(source, &context)).await {
                Ok(Ok(fetched)) => return Ok(fetched),
                Ok(Err(ProviderError::UnsupportedProvider)) => continue,
                Ok(Err(error)) => return Err(CatalogueError::Provider(error)),
                Err(_) => {
                    return Err(CatalogueError::Provider(ProviderError::ProviderUnavailable));
                }
            }
        }

        Err(CatalogueError::Provider(ProviderError::UnsupportedProvider))
    }

    fn state(
        &self,
        reported_count: Option<u32>,
        last_error: Option<String>,
    ) -> Result<ProviderEntityState, CatalogueError> {
        let last_error = last_error.map(|error| error.chars().take(MAX_ERROR).collect());

        ProviderEntityState::new(Utc::now(), reported_count, last_error)
            .map_err(|_| CatalogueError::Repository(TrackRepositoryError::InvalidData))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    };

    use async_trait::async_trait;

    use super::*;
    use crate::{
        catalogue::{CatalogueCandidate, CatalogueSearch},
        request::RequestContext,
        tracks::{
            AlbumMetadata, AlbumTrack, ArtistCredit, InMemoryTrackRepository, LibraryRead,
            ProviderId, SourceScope, StateStore, TrackMetadata,
        },
    };

    fn provider_source(external_id: &str) -> SourceIdentity {
        SourceIdentity::new(
            ProviderId::new("youtube_music".to_owned()).unwrap(),
            SourceScope::Global,
            external_id.to_owned(),
        )
        .unwrap()
    }

    fn album_metadata() -> AlbumMetadata {
        AlbumMetadata::new("SOUR".to_owned())
            .unwrap()
            .with_album_artist(ArtistCredit::new("Olivia Rodrigo".to_owned(), None).unwrap())
            .with_provider(Some(provider_source("MPREb_album")))
    }

    fn candidate(external_id: &str, title: &str) -> CatalogueCandidate {
        let metadata = TrackMetadata::from_names(
            title.to_owned(),
            vec!["Olivia Rodrigo".to_owned()],
            Some(1_000),
        )
        .unwrap();

        CatalogueCandidate::new(provider_source(external_id), metadata)
            .with_album(Some(album_metadata()))
    }

    struct StubAlbumProvider {
        result: Mutex<Result<FetchedAlbum, ()>>,
        calls: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl CatalogueProvider for StubAlbumProvider {
        async fn search(
            &self,
            _search: &CatalogueSearch,
            _user: Option<&Uuid>,
            _context: &RequestContext,
        ) -> Result<Vec<CatalogueCandidate>, ProviderError> {
            Ok(Vec::new())
        }

        async fn fetch_album(
            &self,
            _source: &SourceIdentity,
            _context: &RequestContext,
        ) -> Result<FetchedAlbum, ProviderError> {
            self.calls.fetch_add(1, Ordering::SeqCst);

            match self.result.lock().unwrap().clone() {
                Ok(fetched) => Ok(fetched),
                Err(()) => Err(ProviderError::ProviderUnavailable),
            }
        }
    }

    async fn service(
        repository: &Arc<InMemoryTrackRepository>,
        provider: Arc<dyn CatalogueProvider>,
    ) -> (CompletionService, Arc<dyn SourceLookup>) {
        let sources = Arc::clone(repository) as Arc<dyn SourceLookup>;
        let library = Arc::new(LibraryService::new(
            Arc::clone(repository) as Arc<dyn LibraryRead>,
            Arc::clone(&sources),
            Arc::clone(repository) as Arc<dyn StateStore>,
        ));
        let completion = CompletionService::new(
            library,
            Arc::clone(repository) as Arc<dyn CatalogueWrite>,
            Arc::clone(&sources),
            vec![provider],
        )
        .with_registry(ProviderRegistry::with_enabled(["youtube_music"]));

        (completion, sources)
    }

    async fn partial_album(repository: &Arc<InMemoryTrackRepository>) -> AlbumId {
        let album = repository
            .resolve_or_create_album(&album_metadata())
            .await
            .unwrap();
        let track = repository
            .get_or_create(
                provider_source("vid-1"),
                TrackMetadata::from_names(
                    "brutal".to_owned(),
                    vec!["Olivia Rodrigo".to_owned()],
                    Some(1_000),
                )
                .unwrap(),
            )
            .await
            .unwrap();
        repository
            .set_album_source(album.id(), &provider_source("MPREb_album"))
            .await
            .unwrap();
        repository
            .set_album_track(album.id(), track.id(), &AlbumTrack::unplaced())
            .await
            .unwrap();

        album.id().clone()
    }

    #[tokio::test]
    async fn opening_a_partial_provider_album_completes_it_once() {
        let repository = Arc::new(InMemoryTrackRepository::default());
        let album_id = partial_album(&repository).await;
        let calls = Arc::new(AtomicUsize::new(0));
        let provider = Arc::new(StubAlbumProvider {
            result: Mutex::new(Ok(FetchedAlbum::new(
                vec![
                    candidate("vid-2", "traitor"),
                    candidate("vid-3", "drivers license"),
                ],
                Some(3),
            ))),
            calls: Arc::clone(&calls),
        });
        let (completion, sources) = service(&repository, provider).await;

        let detail = completion
            .album_detail(&album_id, None)
            .await
            .unwrap()
            .expect("the album is found");

        assert_eq!(detail.entries.len(), 3);
        assert_eq!(calls.load(Ordering::SeqCst), 1);

        let state = sources
            .find_provider_entity_state(&provider_source("MPREb_album"))
            .await
            .unwrap()
            .expect("completion records its freshness");
        assert_eq!(state.reported_count(), Some(3));
        assert!(!state.is_partial(3));

        completion.album_detail(&album_id, None).await.unwrap();
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "a settled album is not fetched again"
        );
    }

    #[tokio::test]
    async fn an_unavailable_provider_degrades_to_the_partial_album() {
        let repository = Arc::new(InMemoryTrackRepository::default());
        let album_id = partial_album(&repository).await;
        let provider = Arc::new(StubAlbumProvider {
            result: Mutex::new(Err(())),
            calls: Arc::new(AtomicUsize::new(0)),
        });
        let (completion, sources) = service(&repository, provider).await;

        let detail = completion
            .album_detail(&album_id, None)
            .await
            .unwrap()
            .expect("the album is still found");

        assert_eq!(detail.entries.len(), 1);
        let state = sources
            .find_provider_entity_state(&provider_source("MPREb_album"))
            .await
            .unwrap()
            .expect("a failure is recorded");
        assert_eq!(state.reported_count(), None);
        assert!(state.last_error().is_some());
    }

    #[tokio::test]
    async fn an_owned_album_never_asks_the_provider() {
        let repository = Arc::new(InMemoryTrackRepository::default());
        let album = repository
            .resolve_or_create_album(&album_metadata())
            .await
            .unwrap();
        let local = SourceIdentity::new(
            ProviderId::new("local".to_owned()).unwrap(),
            SourceScope::Named("/music".to_owned()),
            "path-a".to_owned(),
        )
        .unwrap();
        let track = repository
            .get_or_create(
                local,
                TrackMetadata::from_names(
                    "brutal".to_owned(),
                    vec!["Olivia Rodrigo".to_owned()],
                    Some(1_000),
                )
                .unwrap(),
            )
            .await
            .unwrap();
        repository
            .set_album_source(album.id(), &provider_source("MPREb_album"))
            .await
            .unwrap();
        repository
            .set_album_track(album.id(), track.id(), &AlbumTrack::unplaced())
            .await
            .unwrap();

        let calls = Arc::new(AtomicUsize::new(0));
        let provider = Arc::new(StubAlbumProvider {
            result: Mutex::new(Ok(FetchedAlbum::new(Vec::new(), None))),
            calls: Arc::clone(&calls),
        });
        let (completion, _) = service(&repository, provider).await;

        let detail = completion
            .album_detail(album.id(), None)
            .await
            .unwrap()
            .unwrap();

        assert_eq!(detail.entries.len(), 1);
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }
}
