use std::{future::Future, sync::Arc, time::Duration};

use chrono::Utc;
use uuid::Uuid;

use crate::{
    features::music_library::availability::ProviderRegistry,
    features::music_library::catalogue::ProviderError,
    features::music_library::{
        domain::{
            AlbumId, AlbumListFilter, AlbumListScope, AlbumMetadata, AlbumOrder, AlbumPage,
            ArtistId, PageLimit, ProviderEntityState, SourceIdentity,
        },
        storage::contracts::{CatalogueWrite, SourceLookup, TrackRepositoryError},
    },
    platform::request::{RequestContext, RequestId},
};

use super::{
    AlbumDetail, ArtistDetail, CatalogueError, CatalogueProvider, FetchedAlbum, LibraryService,
};

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

    #[tracing::instrument(skip_all)]
    pub async fn artist_detail(
        &self,
        artist_id: &ArtistId,
        user: Option<&Uuid>,
    ) -> Result<Option<ArtistDetail>, CatalogueError> {
        let (Some(catalogue), Some(sources)) = (&self.catalogue, &self.sources) else {
            return self.library.artist_detail(artist_id, user).await;
        };
        if self.library.artist_detail(artist_id, user).await?.is_none() {
            return Ok(None);
        }
        let artist_sources = sources
            .find_artist_sources(artist_id)
            .await
            .map_err(CatalogueError::Repository)?
            .into_iter()
            .filter(|source| !self.registry.is_library(source.provider_id()))
            .collect::<Vec<_>>();
        for source in artist_sources {
            if sources
                .find_provider_entity_state(&source)
                .await
                .map_err(CatalogueError::Repository)?
                .is_some_and(|state| state.last_error().is_none())
            {
                continue;
            }
            match self.fetch_artist_albums(&source).await {
                Ok(albums) => {
                    for album in &albums {
                        super::materialize::materialize_album(catalogue, sources, album).await?;
                    }
                    let state = self.state(None, None)?;
                    catalogue
                        .set_provider_entity_state(&source, &state)
                        .await
                        .map_err(CatalogueError::Repository)?;
                }
                Err(error) => {
                    if matches!(error, CatalogueError::Provider(ProviderError::Access)) {
                        return Err(error);
                    }
                    let state = self.state(None, Some(error.to_string()))?;
                    catalogue
                        .set_provider_entity_state(&source, &state)
                        .await
                        .map_err(CatalogueError::Repository)?;
                }
            }
        }
        self.library.artist_detail(artist_id, user).await
    }

    async fn fetch_artist_albums(
        &self,
        source: &SourceIdentity,
    ) -> Result<Vec<AlbumMetadata>, CatalogueError> {
        self.fetch_from_providers(source, |provider, source, context| async move {
            provider.fetch_artist_albums(&source, &context).await
        })
        .await
    }

    pub async fn album_page(
        &self,
        offset: u32,
        limit: PageLimit,
        order: AlbumOrder,
        filter: &AlbumListFilter,
        user: Option<&Uuid>,
    ) -> Result<AlbumPage, CatalogueError> {
        let artist_id = filter
            .artist_id()
            .filter(|ids| ids.len() == 1)
            .and_then(|ids| ids.iter().next());
        if filter.scope() == AlbumListScope::Engaged
            && let Some(artist_id) = artist_id
        {
            if self.artist_detail(artist_id, user).await?.is_none() {
                return Ok(AlbumPage {
                    albums: Vec::new(),
                    total: 0,
                });
            }
            return self
                .library
                .artist_album_page(artist_id, offset, limit, order, filter, user)
                .await;
        }
        let total = self.library.album_count(user, filter.clone()).await?;
        let albums = self
            .library
            .album_list(offset, limit.get(), order, filter.clone(), user)
            .await?;
        Ok(AlbumPage { albums, total })
    }

    #[tracing::instrument(skip_all)]
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

        let provider_sources = sources
            .find_album_sources(album_id)
            .await
            .map_err(CatalogueError::Repository)?
            .into_iter()
            .filter(|source| !self.registry.is_library(source.provider_id()))
            .collect::<Vec<_>>();

        if provider_sources.is_empty() {
            return Ok(Some(detail));
        }

        let member_count = detail.entries.len() as u32;
        let mut failure: Option<(SourceIdentity, CatalogueError)> = None;

        for provider_source in provider_sources {
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

                    return self.library.album_detail(album_id, user).await;
                }
                Err(error) => {
                    let unusable = matches!(
                        &error,
                        CatalogueError::Provider(
                            ProviderError::SourceNotFound | ProviderError::UnsupportedProvider
                        )
                    );
                    if !unusable || failure.is_none() {
                        failure = Some((provider_source, error));
                    }
                }
            }
        }

        if let Some((provider_source, error)) = failure {
            let state = self.state(None, Some(error.to_string()))?;
            catalogue
                .set_provider_entity_state(&provider_source, &state)
                .await
                .map_err(CatalogueError::Repository)?;
        }

        Ok(Some(detail))
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
        self.fetch_from_providers(source, |provider, source, context| async move {
            provider.fetch_album(&source, &context).await
        })
        .await
    }

    async fn fetch_from_providers<T, F, Fut>(
        &self,
        source: &SourceIdentity,
        fetch: F,
    ) -> Result<T, CatalogueError>
    where
        F: Fn(Arc<dyn CatalogueProvider>, SourceIdentity, RequestContext) -> Fut,
        Fut: Future<Output = Result<T, ProviderError>>,
    {
        let context = RequestContext::new(RequestId::generate());
        for provider in &self.providers {
            match tokio::time::timeout(
                self.budget,
                fetch(Arc::clone(provider), source.clone(), context.clone()),
            )
            .await
            {
                Ok(Ok(result)) => return Ok(result),
                Ok(Err(ProviderError::UnsupportedProvider | ProviderError::SourceNotFound)) => {}
                Ok(Err(error)) => return Err(CatalogueError::Provider(error)),
                Err(_) => return Err(CatalogueError::Provider(ProviderError::ProviderUnavailable)),
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

#[cfg(all(test, feature = "postgres-integration"))]
#[path = "completion_tests.rs"]
mod tests;
