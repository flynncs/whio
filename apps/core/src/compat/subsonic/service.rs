use thiserror::Error;
use whio_subsonic_api::models;

use crate::{
    catalogue::{
        CatalogueError, CatalogueSearch, CatalogueService, SearchHit, SearchLimit, SearchOffset,
        SearchQuery,
    },
    compat::SourceTags,
    request::RequestContext,
};

#[derive(Debug, Error)]
pub(crate) enum SearchError {
    #[error("invalid search parameters")]
    InvalidSearch,

    #[error("catalogue search failed")]
    Catalogue(#[source] CatalogueError),
}

pub(crate) fn child_from_hit(hit: &SearchHit, tags: SourceTags) -> models::Child {
    let track = hit.track();

    models::Child {
        id: track.id().to_string(),
        title: track.title().to_owned(),
        is_dir: false,
        artist: (!track.artists().is_empty()).then(|| tags.artist(track.artists(), hit.sources())),
        album: track.album().map(str::to_owned),
        cover_art: Some(track.id().to_string()),
        duration: track.duration_ms().map(|ms| (ms / 1000) as i32),
        r#type: Some(models::GenericMediaType::Music),
        media_type: Some(models::MediaType::Song),
        ..Default::default()
    }
}

pub(crate) async fn search_songs(
    catalogue: &CatalogueService,
    context: &RequestContext,
    user: Option<&uuid::Uuid>,
    query: &str,
    count: Option<&str>,
    offset: Option<&str>,
    tags: SourceTags,
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
    }
    .min(25);
    let offset = match offset {
        None => 0,
        Some(raw) => raw
            .trim()
            .parse::<u32>()
            .map_err(|_| SearchError::InvalidSearch)?,
    };

    let search = match (
        SearchQuery::new(query.to_owned()),
        SearchLimit::new(limit),
        SearchOffset::new(offset),
    ) {
        (Ok(query), Ok(limit), Ok(offset)) => {
            CatalogueSearch::new_with_offset(query, limit, offset)
        }
        _ => return Err(SearchError::InvalidSearch),
    };

    catalogue
        .search(&search, user, context)
        .await
        .map(|outcome| {
            outcome
                .hits
                .iter()
                .map(|hit| child_from_hit(hit, tags))
                .collect()
        })
        .map_err(SearchError::Catalogue)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        catalogue::{CatalogueCandidate, CatalogueProvider},
        providers::ProviderError,
        request::{RequestContext, RequestId},
        tracks::{InMemoryTrackRepository, ProviderId, SourceIdentity, SourceScope, TrackMetadata},
    };
    use async_trait::async_trait;
    use std::{
        collections::VecDeque,
        sync::{Arc, Mutex},
    };
    use uuid::Uuid;

    struct StubCatalogue {
        responses: Mutex<VecDeque<Result<Vec<CatalogueCandidate>, ProviderError>>>,
    }

    #[async_trait]
    impl CatalogueProvider for StubCatalogue {
        async fn search(
            &self,
            _search: &CatalogueSearch,
            _user: Option<&Uuid>,
            _context: &RequestContext,
        ) -> Result<Vec<CatalogueCandidate>, ProviderError> {
            self.responses
                .lock()
                .expect("responses lock")
                .pop_front()
                .expect("unexpected catalogue call")
        }
    }

    fn catalogue(
        responses: Vec<Result<Vec<CatalogueCandidate>, ProviderError>>,
    ) -> CatalogueService {
        let stub = Arc::new(StubCatalogue {
            responses: Mutex::new(responses.into()),
        });
        CatalogueService::with_providers(
            Arc::new(InMemoryTrackRepository::default()),
            vec![stub as Arc<dyn CatalogueProvider>],
        )
    }

    struct PagedCatalogue {
        total: u32,
    }

    #[async_trait]
    impl CatalogueProvider for PagedCatalogue {
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
            Ok((start..end).map(candidate_with_index).collect())
        }
    }

    fn paged_catalogue(total: u32) -> CatalogueService {
        CatalogueService::with_providers(
            Arc::new(InMemoryTrackRepository::default()),
            vec![Arc::new(PagedCatalogue { total }) as Arc<dyn CatalogueProvider>],
        )
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

    fn candidate_with_index(index: u32) -> CatalogueCandidate {
        CatalogueCandidate::new(
            SourceIdentity::new(
                ProviderId::new("youtube".to_owned()).expect("valid provider"),
                SourceScope::Global,
                format!("vid-{index}"),
            )
            .expect("valid source"),
            TrackMetadata::new(
                format!("song-{index}"),
                vec!["artist".to_owned()],
                Some(200_000),
            )
            .expect("valid metadata"),
        )
    }

    #[tokio::test]
    async fn search_maps_tracks_and_short_circuits_empty() {
        let service = catalogue(vec![Ok(vec![candidate()])]);
        let children = search_songs(
            &service,
            &context(),
            None,
            "song",
            Some("10"),
            None,
            SourceTags::Off,
        )
        .await
        .expect("search succeeds");
        assert_eq!(children.len(), 1);
        assert_eq!(children[0].title, "song");
        assert_eq!(children[0].artist.as_deref(), Some("artist"));
        assert_eq!(children[0].duration, Some(200));
        assert!(!children[0].id.is_empty());

        let children = search_songs(
            &catalogue(vec![]),
            &context(),
            None,
            "",
            None,
            None,
            SourceTags::Off,
        )
        .await
        .expect("search succeeds");
        assert!(children.is_empty());
    }

    #[tokio::test]
    async fn paging_caps_slices_and_rejects() {
        let result = search_songs(
            &catalogue(vec![]),
            &context(),
            None,
            "song",
            Some("many"),
            None,
            SourceTags::Off,
        )
        .await;
        assert!(matches!(result, Err(SearchError::InvalidSearch)));

        let result = search_songs(
            &catalogue(vec![]),
            &context(),
            None,
            "song",
            None,
            Some("1001"),
            SourceTags::Off,
        )
        .await;
        assert!(matches!(result, Err(SearchError::InvalidSearch)));

        let service = paged_catalogue(25);
        async fn songs(
            catalogue: &CatalogueService,
            context: &RequestContext,
            count: &str,
            offset: &str,
        ) -> Vec<whio_subsonic_api::models::Child> {
            search_songs(
                catalogue,
                context,
                None,
                "song",
                Some(count),
                Some(offset),
                SourceTags::Off,
            )
            .await
            .expect("page succeeds")
        }
        let context = context();
        let first = songs(&service, &context, "10", "0").await;
        let second = songs(&service, &context, "10", "10").await;
        let third = songs(&service, &context, "10", "20").await;
        let bounded = songs(&service, &context, "10", "25").await;
        assert_eq!(first.len(), 10);
        assert_eq!(first[0].title, "song-0");
        assert_eq!(second[0].title, "song-10");
        assert_eq!(third.len(), 5);
        assert!(bounded.is_empty());

        let service = paged_catalogue(25);
        let songs = search_songs(
            &service,
            &context,
            None,
            "song",
            Some("100"),
            None,
            SourceTags::Off,
        )
        .await
        .expect("search succeeds");
        assert_eq!(songs.len(), 25);
    }

    fn hit(artists: Vec<String>) -> SearchHit {
        let track = crate::tracks::Track::new(
            crate::tracks::TrackId::new(),
            TrackMetadata::new("song".to_owned(), artists, Some(200_000))
                .expect("valid metadata")
                .with_album(Some("album".to_owned()))
                .expect("valid album"),
        );
        let source = SourceIdentity::new(
            ProviderId::new("youtube_music".to_owned()).expect("valid provider"),
            SourceScope::Global,
            "vid-1".to_owned(),
        )
        .expect("valid source");

        SearchHit::new(track, vec![source])
    }

    #[test]
    fn child_carries_album_and_cover_art() {
        let hit = hit(vec!["artist".to_owned()]);
        let id = hit.track().id().to_string();

        let child = child_from_hit(&hit, SourceTags::Off);

        assert_eq!(child.album.as_deref(), Some("album"));
        assert_eq!(child.cover_art.as_deref(), Some(id.as_str()));
    }

    #[test]
    fn child_tags_the_artist_only_when_configured() {
        let hit = hit(vec!["artist".to_owned(), "featured".to_owned()]);

        assert_eq!(
            child_from_hit(&hit, SourceTags::Off).artist.as_deref(),
            Some("artist, featured")
        );
        assert_eq!(
            child_from_hit(&hit, SourceTags::ArtistPrefix)
                .artist
                .as_deref(),
            Some("[YTM] artist, featured")
        );
    }
}
