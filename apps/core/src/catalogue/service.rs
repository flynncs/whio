use std::sync::Arc;

use thiserror::Error;

use crate::{
    catalogue::{CatalogueResolver, CatalogueSearch},
    request::RequestContext,
    resolvers::ResolverError,
    tracks::{Track, TrackRepository},
};

#[derive(Debug, Error)]
pub enum CatalogueError {
    #[error("catalogue resolver failed")]
    Resolver(#[source] ResolverError),

    #[error("catalogue state is unavailable")]
    State,
}

pub struct CatalogueService {
    resolver: Arc<dyn CatalogueResolver>,
    track_repository: Arc<dyn TrackRepository>,
}

impl CatalogueService {
    pub fn new(
        resolver: Arc<dyn CatalogueResolver>,
        track_repository: Arc<dyn TrackRepository>,
    ) -> Self {
        Self {
            resolver,
            track_repository,
        }
    }

    pub async fn search(
        &self,
        options: &CatalogueSearch,
        context: &RequestContext,
    ) -> Result<Vec<Track>, CatalogueError> {
        let candidates = self
            .resolver
            .search(options, context)
            .await
            .map_err(CatalogueError::Resolver)?;

        let tracks = candidates
            .into_iter()
            .map(|candidate| {
                let (source, metadata) = candidate.into_parts();
                let track_id = self
                    .track_repository
                    .get_or_create_id(source)
                    .map_err(|_| CatalogueError::State)?;

                Ok(Track::new(track_id, metadata))
            })
            .collect::<Result<Vec<_>, CatalogueError>>()?;

        Ok(tracks)
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::VecDeque, sync::Mutex};

    use async_trait::async_trait;

    use super::*;
    use crate::{
        catalogue::{CatalogueCandidate, SearchLimit, SearchQuery},
        request::RequestId,
        tracks::{InMemoryTrackRepository, ProviderId, SourceIdentity, TrackMetadata},
    };

    struct StubResolver {
        responses: Mutex<VecDeque<Result<Vec<CatalogueCandidate>, ResolverError>>>,
    }

    impl StubResolver {
        fn new(responses: Vec<Result<Vec<CatalogueCandidate>, ResolverError>>) -> Self {
            Self {
                responses: Mutex::new(responses.into()),
            }
        }
    }

    #[async_trait]
    impl CatalogueResolver for StubResolver {
        async fn search(
            &self,
            _search: &CatalogueSearch,
            _context: &RequestContext,
        ) -> Result<Vec<CatalogueCandidate>, ResolverError> {
            self.responses.lock().unwrap().pop_front().unwrap()
        }
    }

    fn candidate(external_id: &str, title: &str) -> CatalogueCandidate {
        let provider_id = ProviderId::new("youtube_music".to_owned()).unwrap();
        let source = SourceIdentity::new(provider_id, external_id.to_owned()).unwrap();
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

    fn context() -> RequestContext {
        RequestContext::new(RequestId::new("request-123".to_owned()).unwrap())
    }

    #[tokio::test]
    async fn search_assigns_stable_ids_per_source_and_uses_latest_metadata() {
        let resolver = StubResolver::new(vec![
            Ok(vec![
                candidate("source-a", "Old title"),
                candidate("source-b", "Other"),
            ]),
            Ok(vec![candidate("source-a", "New title")]),
        ]);
        let service = CatalogueService::new(
            Arc::new(resolver),
            Arc::new(InMemoryTrackRepository::default()),
        );

        let first = service.search(&search(), &context()).await.unwrap();
        let second = service.search(&search(), &context()).await.unwrap();

        assert_ne!(first[0].id(), first[1].id());
        assert_eq!(first[0].id(), second[0].id());
        assert_eq!(first[0].title(), "Old title");
        assert_eq!(second[0].title(), "New title");
    }

    #[tokio::test]
    async fn search_preserves_resolver_errors() {
        let resolver = StubResolver::new(vec![Err(ResolverError::ProviderUnavailable)]);
        let service = CatalogueService::new(
            Arc::new(resolver),
            Arc::new(InMemoryTrackRepository::default()),
        );

        let result = service.search(&search(), &context()).await;

        assert!(matches!(
            result,
            Err(CatalogueError::Resolver(ResolverError::ProviderUnavailable))
        ));
    }
}
