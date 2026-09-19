use thiserror::Error;

use crate::{
    catalogue::{
        CatalogueError, CatalogueSearch, CatalogueService, SearchHit, SearchLimit, SearchOffset,
        SearchQuery,
    },
    compat::SourceTags,
    identity::service::{AuthError, CredentialService},
    request::RequestContext,
    tracks::TrackId,
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

fn song_from(hit: &SearchHit, tags: SourceTags) -> Song {
    let track = hit.track();

    Song {
        id: track.id().to_string(),
        title: track.title().to_owned(),
        artist: tags.artist(track.artists(), hit.sources()),
        album: hit.album().map(|album| album.title().to_owned()),
        cover_art: Some(track.id().to_string()),
        duration: track
            .duration_ms()
            .map(|ms| ms as f32 / 1000.0)
            .unwrap_or(0.0),
    }
}

pub(crate) async fn search_songs(
    catalogue: &CatalogueService,
    credential: &CredentialService,
    context: &RequestContext,
    token: &str,
    query: &SongListQuery,
    tags: SourceTags,
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

    Ok(outcome
        .hits
        .iter()
        .map(|hit| song_from(hit, tags))
        .collect())
}

pub(crate) async fn song_detail(
    catalogue: &CatalogueService,
    credential: &CredentialService,
    raw_id: &str,
    token: &str,
    tags: SourceTags,
) -> Result<Song, SongError> {
    match session::authorize(credential, token).await {
        Ok(Some(_)) => {}
        Ok(None) => return Err(SongError::Unauthorized),
        Err(error) => return Err(SongError::Authentication(error)),
    }

    let track_id: TrackId = raw_id.parse().map_err(|_| SongError::NotFound)?;
    let hit = catalogue
        .track_detail(&track_id)
        .await
        .map_err(SongError::Catalogue)?
        .ok_or(SongError::NotFound)?;

    Ok(song_from(&hit, tags))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::{domain::UserId, secrets::CredentialKey};
    use crate::{
        catalogue::{CatalogueCandidate, CatalogueProvider},
        providers::ProviderError,
        request::{RequestContext, RequestId},
        tracks::{
            InMemoryTrackRepository, ProviderId, SourceIdentity, SourceScope, Track, TrackId,
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
            TrackMetadata::from_names(
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
            TrackMetadata::from_names(
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
            SourceTags::Off,
        )
        .await;
        assert!(matches!(result, Err(SongError::Unauthorized)));

        let result = song_detail(
            &catalogue,
            &auth.service,
            &TrackId::new().to_string(),
            "forged",
            SourceTags::Off,
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
            SourceTags::Off,
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
            SourceTags::Off,
        )
        .await;
        assert!(matches!(result, Ok(songs) if songs.is_empty()));

        let songs = search_songs(
            &catalogue(vec![Ok(vec![candidate()])]),
            &auth.service,
            &context(),
            &auth.token,
            &query(Some("song"), Some(0), Some(9)),
            SourceTags::Off,
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
            SourceTags::Off,
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
            SourceTags::Off,
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
                SourceTags::Off,
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
            SourceTags::Off,
        )
        .await
        .expect("search succeeds");
        let song = song_detail(
            &catalogue,
            &auth.service,
            &songs[0].id,
            &auth.token,
            SourceTags::Off,
        )
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
            SourceTags::Off,
        )
        .await;
        assert!(matches!(result, Err(SongError::NotFound)));

        let result = song_detail(
            &catalogue,
            &auth.service,
            "not-a-uuid",
            &auth.token,
            SourceTags::Off,
        )
        .await;
        assert!(matches!(result, Err(SongError::NotFound)));
    }

    #[test]
    fn track_maps_to_song() {
        let track = Track::new(
            TrackId::new(),
            TrackMetadata::from_names(
                "title".to_owned(),
                vec!["a".to_owned(), "b".to_owned()],
                Some(180_000),
            )
            .expect("valid metadata"),
        );
        let album = crate::tracks::Album::new(
            crate::tracks::AlbumId::new(),
            "album".to_owned(),
            None,
            None,
            None,
            None,
            None,
            None,
        )
        .expect("valid album");
        let id = track.id().to_string();

        let song = song_from(
            &SearchHit::new(track, Some(album), Vec::new()),
            SourceTags::Off,
        );

        assert_eq!(song.title, "title");
        assert_eq!(song.artist, "a, b");
        assert_eq!(song.album.as_deref(), Some("album"));
        assert_eq!(song.cover_art.as_deref(), Some(id.as_str()));
        assert_eq!(song.duration, 180.0);
        assert!(!song.id.is_empty());
    }
}
