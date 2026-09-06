use thiserror::Error;

use crate::{
    catalogue::{CatalogueError, CatalogueSearch, CatalogueService, SearchLimit, SearchQuery},
    identity::secrets::CredentialKey,
    request::RequestContext,
    tracks::Track,
};

use super::{dto::Song, session};

#[derive(Debug, Error)]
pub(crate) enum SongError {
    #[error("not authenticated")]
    Unauthorized,

    #[error("invalid search parameters")]
    InvalidSearch,

    #[error("catalogue search failed")]
    Catalogue(#[source] CatalogueError),
}

impl From<Track> for Song {
    fn from(track: Track) -> Self {
        Self {
            id: track.id().to_string(),
            title: track.title().to_owned(),
            artist: track.artists().join(", "),
            duration: track
                .duration_ms()
                .map(|ms| ms as f32 / 1000.0)
                .unwrap_or(0.0),
        }
    }
}

pub(crate) async fn search_songs(
    catalogue: &CatalogueService,
    key: &CredentialKey,
    context: &RequestContext,
    token: &str,
    title: Option<&str>,
    start: Option<u32>,
    end: Option<u32>,
) -> Result<Vec<Song>, SongError> {
    if session::verify(token, key).is_none() {
        return Err(SongError::Unauthorized);
    }

    let q = title.unwrap_or("").trim();
    if q.is_empty() {
        return Ok(Vec::new());
    }

    let limit = match (start, end) {
        (Some(start), Some(end)) => end.saturating_sub(start).saturating_add(1).clamp(1, 25),
        _ => 25,
    };

    let search = match (SearchQuery::new(q.to_owned()), SearchLimit::new(limit)) {
        (Ok(query), Ok(limit)) => CatalogueSearch::new(query, limit),
        _ => return Err(SongError::InvalidSearch),
    };

    catalogue
        .search(&search, context)
        .await
        .map(|tracks| tracks.into_iter().map(Song::from).collect())
        .map_err(SongError::Catalogue)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::domain::UserId;
    use crate::{
        catalogue::{CatalogueCandidate, CatalogueResolver},
        request::{RequestContext, RequestId},
        resolver::ResolverError,
        tracks::{
            InMemoryTrackRepository, ProviderId, SourceIdentity, SourceScope, TrackId,
            TrackMetadata,
        },
    };
    use async_trait::async_trait;
    use std::{
        collections::VecDeque,
        sync::{Arc, Mutex},
    };
    use uuid::Uuid;

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

    fn key() -> CredentialKey {
        CredentialKey::from_base64("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA")
            .expect("valid key")
    }

    fn context() -> RequestContext {
        RequestContext::new(RequestId::generate())
    }

    fn token() -> String {
        session::mint(UserId::from_uuid(Uuid::now_v7()), &key())
    }

    fn catalogue(
        responses: Vec<Result<Vec<CatalogueCandidate>, ResolverError>>,
    ) -> CatalogueService {
        let resolver = Arc::new(StubResolver {
            responses: Mutex::new(responses.into()),
        });
        CatalogueService::new(resolver, Arc::new(InMemoryTrackRepository::default()))
    }

    fn candidate() -> CatalogueCandidate {
        CatalogueCandidate::new(
            SourceIdentity::new(
                ProviderId::new("youtube".to_owned()).expect("valid provider"),
                SourceScope::Global,
                "vid-1".to_owned(),
            )
            .expect("valid source"),
            TrackMetadata::new(
                "song".to_owned(),
                vec!["a".to_owned(), "b".to_owned()],
                Some(180_000),
            )
            .expect("valid metadata"),
        )
    }

    #[tokio::test]
    async fn forged_token_never_reaches_the_resolver() {
        let catalogue = catalogue(vec![]);
        let result = search_songs(&catalogue, &key(), &context(), "forged", None, None, None).await;
        assert!(matches!(result, Err(SongError::Unauthorized)));
    }

    #[tokio::test]
    async fn empty_title_returns_empty_without_calling_the_resolver() {
        let catalogue = catalogue(vec![]);
        let result = search_songs(&catalogue, &key(), &context(), &token(), None, None, None).await;
        assert!(matches!(result, Ok(songs) if songs.is_empty()));
    }

    #[tokio::test]
    async fn tracks_map_to_songs() {
        let catalogue = catalogue(vec![Ok(vec![candidate()])]);
        let songs = search_songs(
            &catalogue,
            &key(),
            &context(),
            &token(),
            Some("song"),
            Some(0),
            Some(9),
        )
        .await
        .expect("search succeeds");

        assert_eq!(songs.len(), 1);
        assert_eq!(songs[0].title, "song");
        assert_eq!(songs[0].artist, "a, b");
        assert_eq!(songs[0].duration, 180.0);
        assert!(!songs[0].id.is_empty());
    }

    #[tokio::test]
    async fn range_beyond_the_catalogue_cap_is_clamped_not_rejected() {
        let catalogue = catalogue(vec![Ok(vec![candidate()])]);
        let songs = search_songs(
            &catalogue,
            &key(),
            &context(),
            &token(),
            Some("song"),
            Some(0),
            Some(100),
        )
        .await
        .expect("search succeeds");

        assert_eq!(songs.len(), 1);
    }

    #[test]
    fn track_maps_to_song() {
        let song = Song::from(Track::new(
            TrackId::new(),
            TrackMetadata::new(
                "title".to_owned(),
                vec!["a".to_owned(), "b".to_owned()],
                Some(180_000),
            )
            .expect("valid metadata"),
        ));
        assert_eq!(song.title, "title");
        assert_eq!(song.artist, "a, b");
        assert_eq!(song.duration, 180.0);
        assert!(!song.id.is_empty());
    }
}
