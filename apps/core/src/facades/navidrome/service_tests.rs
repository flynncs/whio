use super::*;
use crate::features::identity::{domain::UserId, secrets::CredentialKey};
use crate::features::music_library::test_support::{
    CatalogueWrite, LibraryRead, SourceLookup, StateStore,
};
use crate::{
    features::music_library::catalogue::ProviderError,
    features::music_library::catalogue::{CatalogueCandidate, CatalogueProvider},
    features::music_library::{
        domain::{ProviderId, SourceIdentity, SourceScope, Track, TrackId, TrackMetadata},
        test_support::PostgresTrackRepository,
    },
    platform::request::{RequestContext, RequestId},
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
    CredentialKey::from_base64("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA").expect("valid key")
}

struct StubCredentialStore {
    user: UserId,
    credential: Uuid,
}

#[async_trait]
impl crate::features::identity::test_support::CredentialRepository for StubCredentialStore {
    async fn find_user_by_username(
        &self,
        _username: &str,
    ) -> Result<
        Option<crate::features::identity::domain::User>,
        crate::features::identity::test_support::CredentialStoreError,
    > {
        Ok(None)
    }

    async fn find_active_api_key_by_digest(
        &self,
        _digest: [u8; 32],
    ) -> Result<
        Option<crate::features::identity::domain::ApiKeyCredential>,
        crate::features::identity::test_support::CredentialStoreError,
    > {
        Ok(None)
    }

    async fn list_active_app_passwords(
        &self,
        _user_id: UserId,
    ) -> Result<
        Vec<crate::features::identity::domain::AppPasswordCredential>,
        crate::features::identity::test_support::CredentialStoreError,
    > {
        Ok(Vec::new())
    }

    async fn find_active_app_password(
        &self,
        credential_id: Uuid,
    ) -> Result<
        Option<crate::features::identity::domain::AppPasswordCredential>,
        crate::features::identity::test_support::CredentialStoreError,
    > {
        Ok((credential_id == self.credential).then(|| {
            crate::features::identity::domain::AppPasswordCredential {
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

fn services(
    pool: sqlx::PgPool,
    responses: Vec<Result<Vec<CatalogueCandidate>, ProviderError>>,
) -> (SearchService, LibraryService) {
    let stub = Arc::new(StubCatalogue {
        responses: Mutex::new(responses.into()),
    });
    let store = Arc::new(PostgresTrackRepository::new(pool.clone()));
    let search = SearchService::with_providers(
        Arc::clone(&store) as Arc<dyn LibraryRead>,
        Arc::clone(&store) as Arc<dyn CatalogueWrite>,
        Arc::clone(&store) as Arc<dyn SourceLookup>,
        Arc::clone(&store) as Arc<dyn StateStore>,
        vec![stub as Arc<dyn CatalogueProvider>],
    );
    let library = LibraryService::new(
        Arc::clone(&store) as Arc<dyn LibraryRead>,
        Arc::clone(&store) as Arc<dyn SourceLookup>,
        Arc::clone(&store) as Arc<dyn StateStore>,
    );
    (search, library)
}

fn catalogue(
    pool: sqlx::PgPool,
    responses: Vec<Result<Vec<CatalogueCandidate>, ProviderError>>,
) -> SearchService {
    services(pool.clone(), responses).0
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

fn paged_catalogue(pool: sqlx::PgPool, total: u32) -> SearchService {
    let store = Arc::new(PostgresTrackRepository::new(pool.clone()));
    SearchService::with_providers(
        Arc::clone(&store) as Arc<dyn LibraryRead>,
        Arc::clone(&store) as Arc<dyn CatalogueWrite>,
        Arc::clone(&store) as Arc<dyn SourceLookup>,
        Arc::clone(&store) as Arc<dyn StateStore>,
        vec![Arc::new(PagedCatalogue { total }) as Arc<dyn CatalogueProvider>],
    )
}

fn query(title: Option<&str>, start: Option<u32>, end: Option<i64>) -> SongListQuery {
    SongListQuery {
        title: title.map(str::to_owned),
        album_id: None,
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

#[sqlx::test]
async fn auth_gates_search_and_detail(pool: sqlx::PgPool) {
    let (catalogue, library) = services(pool.clone(), vec![]);
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
        &library,
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

#[sqlx::test]
async fn search_maps_tracks_short_circuits_empty_and_clamps_ranges(pool: sqlx::PgPool) {
    let auth = auth();
    let result = search_songs(
        &catalogue(pool.clone(), vec![]),
        &auth.service,
        &context(),
        &auth.token,
        &query(None, None, None),
        SourceTags::Off,
    )
    .await;
    assert!(matches!(result, Ok(songs) if songs.is_empty()));

    let songs = search_songs(
        &catalogue(pool.clone(), vec![Ok(vec![candidate()])]),
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
        &catalogue(pool.clone(), vec![Ok(vec![candidate()])]),
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

#[sqlx::test]
async fn an_unbounded_end_is_not_a_search_error(pool: sqlx::PgPool) {
    let auth = auth();
    let songs = search_songs(
        &catalogue(pool.clone(), vec![Ok(vec![candidate()])]),
        &auth.service,
        &context(),
        &auth.token,
        &query(Some("song"), Some(0), Some(-1)),
        SourceTags::Off,
    )
    .await
    .expect("an unbounded end is accepted");
    assert_eq!(songs.len(), 1);
}

#[sqlx::test]
async fn bounded_search_window_pages_offsets_and_rejects_beyond(pool: sqlx::PgPool) {
    let auth = auth();
    let result = search_songs(
        &catalogue(pool.clone(), vec![]),
        &auth.service,
        &context(),
        &auth.token,
        &query(Some("song"), Some(1_001), Some(1_010)),
        SourceTags::Off,
    )
    .await;
    assert!(matches!(result, Err(SongError::InvalidSearch)));

    let catalogue = paged_catalogue(pool.clone(), 25);
    async fn songs(
        catalogue: &SearchService,
        auth: &TestAuth,
        context: &RequestContext,
        start: u32,
        end: i64,
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

#[sqlx::test]
async fn detail_returns_songs_and_rejects_unknown_ids(pool: sqlx::PgPool) {
    let (catalogue, library) = services(pool.clone(), vec![Ok(vec![candidate()])]);
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
        &library,
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
        &library,
        &auth.service,
        &TrackId::new().to_string(),
        &auth.token,
        SourceTags::Off,
    )
    .await;
    assert!(matches!(result, Err(SongError::NotFound)));

    let result = song_detail(
        &library,
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
    let album = crate::features::music_library::domain::Album::new(
        crate::features::music_library::domain::AlbumId::new(),
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
