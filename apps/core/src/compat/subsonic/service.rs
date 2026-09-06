use thiserror::Error;
use whio_subsonic_api::models;

use crate::{
    catalogue::{CatalogueError, CatalogueSearch, CatalogueService, SearchLimit, SearchQuery},
    request::RequestContext,
    tracks::Track,
};

#[derive(Debug, Error)]
pub(crate) enum SearchError {
    #[error("invalid search parameters")]
    InvalidSearch,

    #[error("catalogue search failed")]
    Catalogue(#[source] CatalogueError),
}

pub(crate) fn child_from_track(track: &Track) -> models::Child {
    models::Child {
        id: track.id().to_string(),
        title: track.title().to_owned(),
        is_dir: false,
        artist: Some(track.artists().join(", ")),
        duration: track.duration_ms().map(|ms| (ms / 1000) as i32),
        r#type: Some(models::GenericMediaType::Music),
        media_type: Some(models::MediaType::Song),
        ..Default::default()
    }
}

pub(crate) async fn search_songs(
    catalogue: &CatalogueService,
    context: &RequestContext,
    query: &str,
    count: Option<&str>,
) -> Result<Vec<models::Child>, SearchError> {
    if query.is_empty() {
        return Ok(Vec::new());
    }

    const DEFAULT_COUNT: u32 = 20;
    let limit = match count {
        None => DEFAULT_COUNT,
        Some(raw) => raw
            .trim()
            .parse::<u32>()
            .map_err(|_| SearchError::InvalidSearch)?,
    };

    let search = match (SearchQuery::new(query.to_owned()), SearchLimit::new(limit)) {
        (Ok(query), Ok(limit)) => CatalogueSearch::new(query, limit),
        _ => return Err(SearchError::InvalidSearch),
    };

    catalogue
        .search(&search, context)
        .await
        .map(|tracks| tracks.iter().map(child_from_track).collect())
        .map_err(SearchError::Catalogue)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        catalogue::{CatalogueCandidate, CatalogueResolver},
        request::{RequestContext, RequestId},
        resolver::ResolverError,
        tracks::{InMemoryTrackRepository, ProviderId, SourceIdentity, SourceScope, TrackMetadata},
    };
    use async_trait::async_trait;
    use std::{
        collections::VecDeque,
        sync::{Arc, Mutex},
    };

    struct StubResolver {
        responses: Mutex<VecDeque<Result<Vec<CatalogueCandidate>, ResolverError>>>,
    }

    #[async_trait]
    impl CatalogueResolver for StubResolver {
        async fn search(
            &self,
            _search: &CatalogueSearch,
            _context: &RequestContext,
        ) -> Result<Vec<CatalogueCandidate>, ResolverError> {
            self.responses
                .lock()
                .expect("responses lock")
                .pop_front()
                .expect("unexpected catalogue call")
        }
    }

    fn catalogue(
        responses: Vec<Result<Vec<CatalogueCandidate>, ResolverError>>,
    ) -> CatalogueService {
        let resolver = Arc::new(StubResolver {
            responses: Mutex::new(responses.into()),
        });
        CatalogueService::new(resolver, Arc::new(InMemoryTrackRepository::default()))
    }

    fn context() -> RequestContext {
        RequestContext::new(RequestId::generate())
    }

    fn candidate() -> CatalogueCandidate {
        CatalogueCandidate::new(
            SourceIdentity::new(
                ProviderId::new("youtube".to_owned()).expect("valid provider"),
                SourceScope::Global,
                "vid-1".to_owned(),
            )
            .expect("valid source"),
            TrackMetadata::new("song".to_owned(), vec!["artist".to_owned()], Some(200_000))
                .expect("valid metadata"),
        )
    }

    #[tokio::test]
    async fn tracks_map_to_children() {
        let catalogue = catalogue(vec![Ok(vec![candidate()])]);
        let children = search_songs(&catalogue, &context(), "song", Some("10"))
            .await
            .expect("search succeeds");

        assert_eq!(children.len(), 1);
        assert_eq!(children[0].title, "song");
        assert_eq!(children[0].artist.as_deref(), Some("artist"));
        assert_eq!(children[0].duration, Some(200));
        assert!(!children[0].id.is_empty());
    }

    #[tokio::test]
    async fn empty_query_returns_empty_without_calling_the_resolver() {
        let catalogue = catalogue(vec![]);
        let children = search_songs(&catalogue, &context(), "", None)
            .await
            .expect("search succeeds");
        assert!(children.is_empty());
    }

    #[tokio::test]
    async fn unparseable_count_is_rejected() {
        let catalogue = catalogue(vec![]);
        let result = search_songs(&catalogue, &context(), "song", Some("many")).await;
        assert!(matches!(result, Err(SearchError::InvalidSearch)));
    }
}
