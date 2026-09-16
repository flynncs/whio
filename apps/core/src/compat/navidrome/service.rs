use thiserror::Error;

use crate::{
    catalogue::{
        CatalogueError, CatalogueSearch, CatalogueService, SearchLimit, SearchOffset, SearchQuery,
    },
    identity::service::{AuthError, CredentialService},
    request::RequestContext,
    tracks::{Track, TrackId},
};

use super::{
    dto::{Song, SongListQuery},
    session,
};

#[derive(Debug, Error)]
pub(crate) enum SongError {
    #[error("not authenticated")]
    Unauthorized,

    #[error("authentication storage failed")]
    Authentication(#[source] AuthError),

    #[error("track not found")]
    NotFound,

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
    credential: &CredentialService,
    context: &RequestContext,
    token: &str,
    query: &SongListQuery,
) -> Result<Vec<Song>, SongError> {
    let user_id = match session::authorize(credential, token).await {
        Ok(Some(user_id)) => user_id,
        Ok(None) => return Err(SongError::Unauthorized),
        Err(error) => return Err(SongError::Authentication(error)),
    };
    let user_uuid = user_id.as_uuid();

    let q = query.title.as_deref().unwrap_or("").trim();
    if q.is_empty() {
        return Ok(Vec::new());
    }

    let start = query.start.unwrap_or(0);
    let end = query.end.unwrap_or_else(|| start.saturating_add(24));
    if end < start {
        return Err(SongError::InvalidSearch);
    }
    let limit = end.saturating_sub(start).saturating_add(1).clamp(1, 25);
    let search = match (
        SearchQuery::new(q.to_owned()),
        SearchLimit::new(limit),
        SearchOffset::new(start),
    ) {
        (Ok(query), Ok(limit), Ok(offset)) => {
            CatalogueSearch::new_with_offset(query, limit, offset)
        }
        _ => return Err(SongError::InvalidSearch),
    };

    let outcome = catalogue
        .search(&search, Some(&user_uuid), context)
        .await
        .map_err(SongError::Catalogue)?;

    Ok(outcome.tracks.into_iter().map(Song::from).collect())
}

pub(crate) async fn song_detail(
    catalogue: &CatalogueService,
    credential: &CredentialService,
    raw_id: &str,
    token: &str,
) -> Result<Song, SongError> {
    match session::authorize(credential, token).await {
        Ok(Some(_)) => {}
        Ok(None) => return Err(SongError::Unauthorized),
        Err(error) => return Err(SongError::Authentication(error)),
    }

    let track_id: TrackId = raw_id.parse().map_err(|_| SongError::NotFound)?;
    catalogue
        .track(&track_id)
        .await
        .map_err(SongError::Catalogue)?
        .map(Song::from)
        .ok_or(SongError::NotFound)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::{domain::UserId, secrets::CredentialKey};
    use crate::{
        catalogue::{CatalogueCandidate, CatalogueProvider},
        providers::ProviderError,
        providers::{NavidromeAdapter, UpstreamTransport},
        request::{RequestContext, RequestId},
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

    fn key() -> CredentialKey {
        CredentialKey::from_base64("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA")
            .expect("valid key")
    }

    struct StubCredentialStore {
        user: UserId,
        credential: Uuid,
    }

    #[async_trait]
    impl crate::identity::repository::CredentialRepository for StubCredentialStore {
        async fn find_user_by_username(
            &self,
            _username: &str,
        ) -> Result<
            Option<crate::identity::domain::User>,
            crate::identity::repository::CredentialStoreError,
        > {
            Ok(None)
        }

        async fn find_active_api_key_by_digest(
            &self,
            _digest: [u8; 32],
        ) -> Result<
            Option<crate::identity::domain::ApiKeyCredential>,
            crate::identity::repository::CredentialStoreError,
        > {
            Ok(None)
        }

        async fn list_active_app_passwords(
            &self,
            _user_id: UserId,
        ) -> Result<
            Vec<crate::identity::domain::AppPasswordCredential>,
            crate::identity::repository::CredentialStoreError,
        > {
            Ok(Vec::new())
        }

        async fn find_active_app_password(
            &self,
            credential_id: Uuid,
        ) -> Result<
            Option<crate::identity::domain::AppPasswordCredential>,
            crate::identity::repository::CredentialStoreError,
        > {
            Ok((credential_id == self.credential).then(|| {
                crate::identity::domain::AppPasswordCredential {
                    id: self.credential,
                    user_id: self.user,
                    encrypted_secret: Vec::new(),
                }
            }))
        }
    }

    struct TestAuth {
        service: std::sync::Arc<CredentialService>,
        token: String,
    }

    fn auth() -> TestAuth {
        let key = key();
        let user = UserId::from_uuid(Uuid::now_v7());
        let credential = Uuid::now_v7();
        let service = std::sync::Arc::new(CredentialService::new(
            std::sync::Arc::new(StubCredentialStore { user, credential }),
            key.clone(),
        ));
        let token = session::mint(user, credential, &key);
        TestAuth { service, token }
    }

    fn context() -> RequestContext {
        RequestContext::new(RequestId::generate())
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

    fn query(title: Option<&str>, start: Option<u32>, end: Option<u32>) -> SongListQuery {
        SongListQuery {
            title: title.map(str::to_owned),
            start,
            end,
        }
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
                Some(180_000),
            )
            .expect("valid metadata"),
        )
    }

    #[tokio::test]
    async fn auth_gates_search_and_detail() {
        let catalogue = catalogue(vec![]);
        let auth = auth();
        let result = search_songs(
            &catalogue,
            &auth.service,
            &context(),
            "forged",
            &query(None, None, None),
        )
        .await;
        assert!(matches!(result, Err(SongError::Unauthorized)));

        let result = song_detail(
            &catalogue,
            &auth.service,
            &TrackId::new().to_string(),
            "forged",
        )
        .await;
        assert!(matches!(result, Err(SongError::Unauthorized)));

        let dead = session::mint(UserId::from_uuid(Uuid::now_v7()), Uuid::now_v7(), &key());
        let result = search_songs(
            &catalogue,
            &auth.service,
            &context(),
            &dead,
            &query(Some("song"), None, None),
        )
        .await;
        assert!(matches!(result, Err(SongError::Unauthorized)));
    }

    #[tokio::test]
    async fn search_maps_tracks_short_circuits_empty_and_clamps_ranges() {
        let auth = auth();
        let result = search_songs(
            &catalogue(vec![]),
            &auth.service,
            &context(),
            &auth.token,
            &query(None, None, None),
        )
        .await;
        assert!(matches!(result, Ok(songs) if songs.is_empty()));

        let songs = search_songs(
            &catalogue(vec![Ok(vec![candidate()])]),
            &auth.service,
            &context(),
            &auth.token,
            &query(Some("song"), Some(0), Some(9)),
        )
        .await
        .expect("search succeeds");
        assert_eq!(songs.len(), 1);
        assert_eq!(songs[0].title, "song");
        assert_eq!(songs[0].artist, "a, b");
        assert_eq!(songs[0].duration, 180.0);
        assert!(!songs[0].id.is_empty());

        let songs = search_songs(
            &catalogue(vec![Ok(vec![candidate()])]),
            &auth.service,
            &context(),
            &auth.token,
            &query(Some("song"), Some(0), Some(100)),
        )
        .await
        .expect("search succeeds");
        assert_eq!(songs.len(), 1);
    }

    #[tokio::test]
    async fn bounded_search_window_pages_offsets_and_rejects_beyond() {
        let auth = auth();
        let result = search_songs(
            &catalogue(vec![]),
            &auth.service,
            &context(),
            &auth.token,
            &query(Some("song"), Some(1_001), Some(1_010)),
        )
        .await;
        assert!(matches!(result, Err(SongError::InvalidSearch)));

        let catalogue = paged_catalogue(25);
        async fn songs(
            catalogue: &CatalogueService,
            auth: &TestAuth,
            context: &RequestContext,
            start: u32,
            end: u32,
        ) -> Vec<Song> {
            search_songs(
                catalogue,
                &auth.service,
                context,
                &auth.token,
                &query(Some("song"), Some(start), Some(end)),
            )
            .await
            .expect("page succeeds")
        }
        let context = context();
        let first = songs(&catalogue, &auth, &context, 0, 9).await;
        let second = songs(&catalogue, &auth, &context, 10, 19).await;
        let third = songs(&catalogue, &auth, &context, 20, 29).await;
        let bounded = songs(&catalogue, &auth, &context, 25, 34).await;
        let first_titles = first
            .iter()
            .map(|song| song.title.clone())
            .collect::<Vec<_>>();
        let expected_titles = (0..10)
            .map(|index| format!("song-{index}"))
            .collect::<Vec<_>>();
        assert_eq!(first_titles, expected_titles);
        assert_eq!(second[0].title, "song-10");
        assert_eq!(second.len(), 10);
        assert_eq!(third[0].title, "song-20");
        assert_eq!(third.len(), 5);
        assert!(bounded.is_empty());
    }

    #[tokio::test]
    async fn detail_returns_songs_and_rejects_unknown_ids() {
        let catalogue = catalogue(vec![Ok(vec![candidate()])]);
        let auth = auth();
        let songs = search_songs(
            &catalogue,
            &auth.service,
            &context(),
            &auth.token,
            &query(Some("song"), Some(0), Some(9)),
        )
        .await
        .expect("search succeeds");
        let song = song_detail(&catalogue, &auth.service, &songs[0].id, &auth.token)
            .await
            .expect("detail succeeds");
        assert_eq!(song.id, songs[0].id);
        assert_eq!(song.title, "song");
        assert_eq!(song.artist, "a, b");
        assert_eq!(song.duration, 180.0);

        let result = song_detail(
            &catalogue,
            &auth.service,
            &TrackId::new().to_string(),
            &auth.token,
        )
        .await;
        assert!(matches!(result, Err(SongError::NotFound)));

        let result = song_detail(&catalogue, &auth.service, "not-a-uuid", &auth.token).await;
        assert!(matches!(result, Err(SongError::NotFound)));
    }

    struct StubUpstream;

    #[async_trait]
    impl UpstreamTransport for StubUpstream {
        async fn search(
            &self,
            _target: &crate::providers::UpstreamTarget,
            _search: &CatalogueSearch,
            _context: &RequestContext,
        ) -> Result<Vec<crate::providers::UpstreamCandidate>, crate::providers::UpstreamError>
        {
            Ok(vec![crate::providers::UpstreamCandidate {
                external_id: "nd-1".to_owned(),
                title: "Upstream Song".to_owned(),
                artists: vec!["Artist".to_owned()],
                duration_ms: Some(200_000),
            }])
        }

        async fn resolve(
            &self,
            _target: &crate::providers::UpstreamTarget,
            _external_id: &str,
            _context: &RequestContext,
        ) -> Result<String, crate::providers::UpstreamError> {
            Err(crate::providers::UpstreamError::Unavailable)
        }
    }

    #[tokio::test]
    async fn linked_user_searches_upstream_and_fallback() {
        use crate::identity::upstream::{MemoryUpstreamLinkStore, UpstreamLinkStore};

        let key = key();
        let user = UserId::from_uuid(Uuid::now_v7());
        let credential = Uuid::now_v7();
        let auth = TestAuth {
            service: std::sync::Arc::new(CredentialService::new(
                std::sync::Arc::new(StubCredentialStore { user, credential }),
                key.clone(),
            )),
            token: session::mint(user, credential, &key),
        };
        let user_uuid = user.as_uuid();
        let server = Uuid::now_v7();
        let store = MemoryUpstreamLinkStore::new();
        store.insert_link(
            &key,
            user_uuid,
            server,
            "https://music.example.test",
            "friend",
            "password",
        );

        let stub = Arc::new(StubCatalogue {
            responses: Mutex::new(vec![Ok(vec![candidate()])].into()),
        });
        let catalogue = CatalogueService::with_providers(
            Arc::new(InMemoryTrackRepository::default()),
            vec![
                Arc::new(NavidromeAdapter::new(
                    Arc::new(store) as Arc<dyn UpstreamLinkStore>,
                    key,
                    Arc::new(StubUpstream) as Arc<dyn UpstreamTransport>,
                )) as Arc<dyn CatalogueProvider>,
                stub as Arc<dyn CatalogueProvider>,
            ],
        );
        let songs = search_songs(
            &catalogue,
            &auth.service,
            &context(),
            &auth.token,
            &query(Some("song"), None, None),
        )
        .await
        .expect("linked search succeeds");

        assert_eq!(songs.len(), 2);
        assert_eq!(songs[0].title, "Upstream Song");
        assert_eq!(songs[1].title, "song");
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
