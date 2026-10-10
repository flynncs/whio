use super::*;
use crate::features::identity::{domain::UserId, secrets::CredentialKey};
use crate::features::music_library::domain::{
    AlbumMetadata, AlbumTrack, ArtistCredit, ProviderId, SelectorText, SourceIdentity, SourceScope,
    Track, TrackMetadata,
};
use crate::features::music_library::test_support::{
    CatalogueWrite, LibraryRead, PostgresTrackRepository, SourceLookup, StateStore,
};
use async_trait::async_trait;
use std::sync::Arc;
use uuid::Uuid;

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

    async fn list_users(
        &self,
    ) -> Result<
        Vec<crate::features::identity::domain::User>,
        crate::features::identity::test_support::CredentialStoreError,
    > {
        Err(crate::features::identity::test_support::CredentialStoreError::Unavailable)
    }

    async fn find_user_by_id(
        &self,
        _id: UserId,
    ) -> Result<
        Option<crate::features::identity::domain::User>,
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

fn key() -> CredentialKey {
    CredentialKey::from_base64("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA").expect("valid key")
}

fn auth() -> TestAuth {
    let key = key();
    let user = UserId::from_uuid(Uuid::now_v7());
    let credential = Uuid::now_v7();
    let service = std::sync::Arc::new(CredentialService::new(
        std::sync::Arc::new(StubCredentialStore { user, credential }),
        key.clone(),
        crate::features::identity::LoginThrottleConfig::default(),
    ));
    let token = session::mint(user, credential, &key);

    TestAuth { service, token }
}

fn library(pool: sqlx::PgPool) -> Arc<LibraryService> {
    let store = Arc::new(PostgresTrackRepository::new(pool));

    Arc::new(LibraryService::new(
        Arc::clone(&store) as Arc<dyn LibraryRead>,
        Arc::clone(&store) as Arc<dyn SourceLookup>,
        Arc::clone(&store) as Arc<dyn StateStore>,
    ))
}

async fn add_track(store: &PostgresTrackRepository, external_id: &str, title: &str) -> TrackId {
    let source = SourceIdentity::new(
        ProviderId::new("local".to_owned()).expect("valid provider"),
        SourceScope::Named("/music".to_owned()),
        external_id.to_owned(),
    )
    .expect("valid source");

    store
        .get_or_create(
            source,
            TrackMetadata::from_names(title.to_owned(), vec!["Artist".to_owned()], Some(1_000))
                .expect("valid metadata"),
        )
        .await
        .expect("a stored track")
        .id()
        .clone()
}

async fn seed(store: &PostgresTrackRepository, count: usize) {
    for index in 0..count {
        add_track(
            store,
            &format!("path-{index}"),
            &format!("Track {index:04}"),
        )
        .await;
    }
}

async fn add_album(
    store: &PostgresTrackRepository,
    title: &str,
    album_artist: Option<&str>,
) -> AlbumId {
    let mut metadata = AlbumMetadata::new(title.to_owned()).expect("valid album");
    if let Some(name) = album_artist {
        metadata =
            metadata.with_album_artist(ArtistCredit::new(name.to_owned(), None).expect("valid"));
    }

    store
        .resolve_or_create_album(&metadata)
        .await
        .expect("a stored album")
        .id()
        .clone()
}

fn query(title: Option<&str>, start: Option<u32>, end: Option<i64>) -> SongListRequest {
    let offset = start.unwrap_or(0);
    let end = u32::try_from(end.unwrap_or(0).max(0)).unwrap_or(u32::MAX);
    let span = end.saturating_sub(offset).min(MAX_PAGE_LIMIT);
    SongListRequest {
        filter: TrackListFilter::new().with_title(
            SelectorText::new(title.unwrap_or_default()).expect("valid title selector"),
        ),
        order: TrackOrder::Title,
        offset,
        limit: PageLimit::new(if span == 0 { MAX_PAGE_LIMIT } else { span })
            .expect("valid page limit"),
    }
}

async fn song_page(
    library: &Arc<LibraryService>,
    auth: &TestAuth,
    request: &SongListRequest,
) -> (Vec<Song>, u32) {
    song_list(
        library,
        &CompletionService::disabled(Arc::clone(library)),
        &auth.service,
        &auth.token,
        request.clone(),
        SourceTags::Off,
    )
    .await
    .expect("song list succeeds")
}

#[sqlx::test]
async fn auth_gates_the_song_list_and_detail(pool: sqlx::PgPool) {
    let library = library(pool.clone());
    let auth = auth();

    let result = song_list(
        &library,
        &CompletionService::disabled(Arc::clone(&library)),
        &auth.service,
        "forged",
        query(None, None, None),
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
    let result = song_list(
        &library,
        &CompletionService::disabled(Arc::clone(&library)),
        &auth.service,
        &dead,
        query(Some("song"), None, None),
        SourceTags::Off,
    )
    .await;
    assert!(matches!(result, Err(SongError::Unauthorized)));
}

#[sqlx::test]
async fn song_list_enumerates_the_library_in_title_order(pool: sqlx::PgPool) {
    let auth = auth();
    let library = library(pool.clone());
    let store = Arc::new(PostgresTrackRepository::new(pool));
    for (index, title) in ["Zebra", "Apple", "Mango"].iter().enumerate() {
        add_track(&store, &format!("path-{index}"), title).await;
    }

    let (songs, total) = song_page(&library, &auth, &query(None, None, None)).await;

    assert_eq!(total, 3);
    assert_eq!(
        songs
            .iter()
            .map(|song| song.title.as_str())
            .collect::<Vec<_>>(),
        ["Apple", "Mango", "Zebra"]
    );
}

#[sqlx::test]
async fn song_list_matches_a_normalised_title_and_treats_blank_as_absent(pool: sqlx::PgPool) {
    let auth = auth();
    let library = library(pool.clone());
    let store = Arc::new(PostgresTrackRepository::new(pool));
    add_track(&store, "path-a", "Instant Crush").await;
    add_track(&store, "path-b", "One More Time").await;

    let (songs, total) = song_page(
        &library,
        &auth,
        &query(Some("  INSTANT   crush  "), None, None),
    )
    .await;
    assert_eq!(total, 1);
    assert_eq!(songs.len(), 1);
    assert_eq!(songs[0].title, "Instant Crush");

    let (songs, total) = song_page(&library, &auth, &query(Some("missing"), None, None)).await;
    assert!(songs.is_empty());
    assert_eq!(total, 0);

    let (songs, total) = song_page(&library, &auth, &query(Some("   "), None, None)).await;
    assert_eq!(songs.len(), 2);
    assert_eq!(total, 2);
}

#[sqlx::test]
async fn song_list_window_is_end_exclusive_and_honours_deep_offsets(pool: sqlx::PgPool) {
    let auth = auth();
    let library = library(pool.clone());
    let store = Arc::new(PostgresTrackRepository::new(pool));
    seed(&store, 30).await;

    let (songs, total) = song_page(&library, &auth, &query(None, Some(0), Some(9))).await;
    assert_eq!(total, 30);
    assert_eq!(songs.len(), 9, "max(0, _end - _start) is the row count");
    assert_eq!(songs[0].title, "Track 0000");
    assert_eq!(songs[8].title, "Track 0008");

    let (songs, _) = song_page(&library, &auth, &query(None, Some(10), Some(19))).await;
    assert_eq!(songs.len(), 9);
    assert_eq!(songs[0].title, "Track 0010");

    let (songs, _) = song_page(&library, &auth, &query(None, Some(0), Some(99))).await;
    assert_eq!(
        songs.len(),
        30,
        "a bound past the collection returns the rows that exist"
    );

    let (songs, _) = song_page(&library, &auth, &query(None, None, None)).await;
    assert_eq!(
        songs.len(),
        30,
        "an absent bound is zero, which is no bound"
    );

    let (songs, _) = song_page(&library, &auth, &query(None, Some(0), Some(-1))).await;
    assert_eq!(
        songs.len(),
        30,
        "a negative bound is zero, which is no bound"
    );

    let (songs, total) = song_page(
        &library,
        &auth,
        &query(Some("track"), Some(1_001), Some(1_010)),
    )
    .await;
    assert!(songs.is_empty());
    assert_eq!(total, 30);

    let (songs, _) = song_page(&library, &auth, &query(None, Some(10), Some(9))).await;
    assert_eq!(songs.len(), 20, "an empty bound offsets without limiting");
    assert_eq!(songs[0].title, "Track 0010");
}

#[sqlx::test]
async fn song_list_maps_stored_tracks_into_songs(pool: sqlx::PgPool) {
    let auth = auth();
    let library = library(pool.clone());
    let store = Arc::new(PostgresTrackRepository::new(pool));
    let album = add_album(&store, "Discovery", Some("Daft Punk")).await;
    let placed = add_track(&store, "path-a", "Song One").await;
    store
        .set_album_track(
            &album,
            &placed,
            &AlbumTrack::new(Some(1), Some(2), 1).expect("valid membership"),
        )
        .await
        .expect("membership stored");

    let (songs, total) = song_page(&library, &auth, &query(Some("song one"), None, None)).await;

    assert_eq!(total, 1);
    assert_eq!(songs.len(), 1);
    assert_eq!(songs[0].title, "Song One");
    assert_eq!(songs[0].artist, "Artist");
    assert_eq!(songs[0].album.as_deref(), Some("Discovery"));
    assert_eq!(
        songs[0].album_id.as_deref(),
        Some(album.to_string().as_str())
    );
    assert_eq!(songs[0].album_artist.as_deref(), Some("Daft Punk"));
    assert_eq!(songs[0].track_number, Some(2));
    assert_eq!(songs[0].disc_number, Some(1));
    assert_eq!(songs[0].duration, Some(1.0));
    assert_eq!(
        songs[0].cover_art.as_deref(),
        Some(placed.to_string().as_str())
    );
    assert!(!songs[0].id.is_empty());
}

#[sqlx::test]
async fn detail_returns_songs_and_rejects_unknown_ids(pool: sqlx::PgPool) {
    let auth = auth();
    let library = library(pool.clone());
    let store = Arc::new(PostgresTrackRepository::new(pool));
    let stored = add_track(&store, "path-a", "Song One").await;

    let (songs, _) = song_page(&library, &auth, &query(Some("song one"), None, None)).await;
    let song = song_detail(
        &library,
        &auth.service,
        &songs[0].id,
        &auth.token,
        SourceTags::Off,
    )
    .await
    .expect("detail succeeds");
    assert_eq!(song.id, stored.to_string());
    assert_eq!(song.title, "Song One");
    assert_eq!(song.artist, "Artist");
    assert_eq!(song.duration, Some(1.0));

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

    let song = song_from_view(
        &TrackView::new(
            track,
            Some(album),
            None,
            None,
            crate::features::music_library::domain::LibraryState::default(),
            Vec::new(),
        ),
        SourceTags::Off,
    );

    assert_eq!(song.title, "title");
    assert_eq!(song.artist, "a, b");
    assert_eq!(song.album.as_deref(), Some("album"));
    assert_eq!(song.cover_art.as_deref(), Some(id.as_str()));
    assert_eq!(song.duration, Some(180.0));
    assert!(!song.id.is_empty());
}

#[test]
fn every_album_sort_name_maps_exactly_or_is_rejected() {
    for (foreign, expected) in [
        ("name", AlbumOrder::Name),
        ("album_artist", AlbumOrder::AlbumArtist),
        ("albumartist", AlbumOrder::AlbumArtist),
        ("artist", AlbumOrder::Artist),
        ("duration", AlbumOrder::Duration),
        ("songCount", AlbumOrder::SongCount),
        ("max_year", AlbumOrder::MaxYear),
        ("recently_added", AlbumOrder::DateAdded),
        ("recentlyadded", AlbumOrder::DateAdded),
        ("createdAt", AlbumOrder::DateAdded),
        ("starred_at", AlbumOrder::StarredAt),
        ("rating", AlbumOrder::Rating),
        ("play_count", AlbumOrder::PlayCount),
        ("play_date", AlbumOrder::PlayDate),
        ("random", AlbumOrder::Random),
    ] {
        assert!(
            matches!(album_order(Some(foreign)), Ok(order) if order == expected),
            "{foreign} must map to {expected:?}"
        );
    }

    assert!(matches!(album_order(None), Ok(AlbumOrder::Name)));

    for unknown in [
        "",
        "Name",
        "song_count",
        "albumCount",
        "year",
        "explicitStatus",
        "playCount",
        "recentlyAdded",
        "starred",
    ] {
        assert!(
            matches!(album_order(Some(unknown)), Err(SongError::InvalidSearch)),
            "{unknown} must not fall back to a sort"
        );
    }
}

#[test]
fn every_artist_sort_name_maps_exactly_or_is_rejected() {
    for (foreign, expected) in [
        ("name", ArtistSort::Name),
        ("albumCount", ArtistSort::AlbumCount),
        ("songCount", ArtistSort::SongCount),
        ("starred_at", ArtistSort::StarredAt),
        ("rating", ArtistSort::Rating),
        ("playCount", ArtistSort::PlayCount),
    ] {
        assert!(
            matches!(artist_sort(Some(foreign)), Ok(sort) if sort == expected),
            "{foreign} must map to {expected:?}"
        );
    }

    assert!(matches!(artist_sort(None), Ok(ArtistSort::Name)));

    for unknown in [
        "",
        "album_count",
        "song_count",
        "play_count",
        "Name",
        "random",
    ] {
        assert!(
            matches!(artist_sort(Some(unknown)), Err(SongError::InvalidSearch)),
            "{unknown} must not fall back to a sort"
        );
    }
}

#[test]
fn the_order_parameter_is_an_explicit_direction() {
    assert!(matches!(sort_direction(None), Ok(None)));
    assert!(matches!(
        sort_direction(Some("asc")),
        Ok(Some(SortDirection::Ascending))
    ));
    assert!(matches!(
        sort_direction(Some("ASC")),
        Ok(Some(SortDirection::Ascending))
    ));
    assert!(matches!(
        sort_direction(Some("desc")),
        Ok(Some(SortDirection::Descending))
    ));
    assert!(matches!(
        sort_direction(Some("DESC")),
        Ok(Some(SortDirection::Descending))
    ));

    for unknown in ["", "ascending", "down", "1"] {
        assert!(matches!(
            sort_direction(Some(unknown)),
            Err(SongError::InvalidSearch)
        ));
    }
}
