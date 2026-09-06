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

fn filter_q(filter: Option<&str>) -> String {
    let Some(raw) = filter else {
        return String::new();
    };
    let Some(key) = raw.find("\"q\"") else {
        return String::new();
    };
    let Some(colon) = raw[key + 3..].find(':') else {
        return String::new();
    };
    let mut chars = raw[key + 3 + colon + 1..].trim_start().chars();
    if chars.next() != Some('"') {
        return String::new();
    }
    let mut out = String::new();
    let mut escaped = false;
    for c in chars {
        if escaped {
            out.push(c);
            escaped = false;
        } else if c == '\\' {
            escaped = true;
        } else if c == '"' {
            break;
        } else {
            out.push(c);
        }
    }
    out
}

fn result_limit(range: Option<&str>) -> u32 {
    const DEFAULT: u32 = 25;
    let Some(count) = range.and_then(|raw| {
        let inner = raw.trim().strip_prefix('[')?.strip_suffix(']')?;
        let (start, end) = inner.split_once(',')?;
        let (start, end): (u32, u32) = (start.trim().parse().ok()?, end.trim().parse().ok()?);
        end.checked_sub(start)?.checked_add(1)
    }) else {
        return DEFAULT;
    };
    count.clamp(1, 25)
}

pub(crate) async fn search_songs(
    catalogue: &CatalogueService,
    key: &CredentialKey,
    context: &RequestContext,
    token: &str,
    filter: Option<&str>,
    range: Option<&str>,
) -> Result<Vec<Song>, SongError> {
    if session::verify(token, key).is_none() {
        return Err(SongError::Unauthorized);
    }

    let q = filter_q(filter);
    if q.is_empty() {
        return Ok(Vec::new());
    }

    let search = match (SearchQuery::new(q), SearchLimit::new(result_limit(range))) {
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
        let result = search_songs(&catalogue, &key(), &context(), "forged", None, None).await;
        assert!(matches!(result, Err(SongError::Unauthorized)));
    }

    #[tokio::test]
    async fn empty_filter_returns_empty_without_calling_the_resolver() {
        let catalogue = catalogue(vec![]);
        let result = search_songs(&catalogue, &key(), &context(), &token(), None, None).await;
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
            Some(r#"{"q":"song"}"#),
            Some("[0,9]"),
        )
        .await
        .expect("search succeeds");

        assert_eq!(songs.len(), 1);
        assert_eq!(songs[0].title, "song");
        assert_eq!(songs[0].artist, "a, b");
        assert_eq!(songs[0].duration, 180.0);
        assert!(!songs[0].id.is_empty());
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

    #[test]
    fn filter_q_reads_the_query_value() {
        assert_eq!(filter_q(None), "");
        assert_eq!(filter_q(Some("garbage")), "");
        assert_eq!(filter_q(Some(r#"{"q":"daft punk"}"#)), "daft punk");
        assert_eq!(filter_q(Some(r#"{"q":"a\"b"}"#)), r#"a"b"#);
        assert_eq!(filter_q(Some(r#"{"other":1}"#)), "");
        assert_eq!(filter_q(Some(r#"{"q":42}"#)), "");
    }

    #[test]
    fn result_limit_follows_the_range() {
        assert_eq!(result_limit(None), 25);
        assert_eq!(result_limit(Some("[0,24]")), 25);
        assert_eq!(result_limit(Some("[0,9]")), 10);
        assert_eq!(result_limit(Some("[0,999]")), 25);
        assert_eq!(result_limit(Some("garbage")), 25);
    }
}
