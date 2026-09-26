use super::*;
use crate::features::music_library::test_support::{
    CatalogueWrite, LibraryRead, SourceLookup, StateStore,
};
use crate::{
    features::music_library::catalogue::ProviderError,
    features::music_library::catalogue::{CatalogueCandidate, CatalogueProvider},
    features::music_library::{
        domain::{
            AlbumMetadata, AlbumTrack, ArtistCredit, ProviderId, SourceIdentity, SourceScope,
            TrackMetadata,
        },
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

fn catalogue(
    pool: sqlx::PgPool,
    responses: Vec<Result<Vec<CatalogueCandidate>, ProviderError>>,
) -> SearchService {
    let stub = Arc::new(StubCatalogue {
        responses: Mutex::new(responses.into()),
    });
    let store = Arc::new(PostgresTrackRepository::new(pool.clone()));
    SearchService::with_providers(
        Arc::clone(&store) as Arc<dyn LibraryRead>,
        Arc::clone(&store) as Arc<dyn CatalogueWrite>,
        Arc::clone(&store) as Arc<dyn SourceLookup>,
        Arc::clone(&store) as Arc<dyn StateStore>,
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

fn context() -> RequestContext {
    RequestContext::new(RequestId::generate())
}

fn source(external_id: &str) -> SourceIdentity {
    SourceIdentity::new(
        ProviderId::new("youtube_music".to_owned()).expect("valid provider"),
        SourceScope::Global,
        external_id.to_owned(),
    )
    .expect("valid source")
}

fn candidate() -> CatalogueCandidate {
    CatalogueCandidate::new(
        SourceIdentity::new(
            ProviderId::new("youtube".to_owned()).expect("valid provider"),
            SourceScope::Global,
            "vid-1".to_owned(),
        )
        .expect("valid source"),
        TrackMetadata::from_names("song".to_owned(), vec!["artist".to_owned()], Some(200_000))
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
            Some(200_000),
        )
        .expect("valid metadata"),
    )
}

#[sqlx::test]
async fn search_maps_tracks_and_short_circuits_empty(pool: sqlx::PgPool) {
    let service = catalogue(pool.clone(), vec![Ok(vec![candidate()])]);
    let result = search(
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
    let songs = result.songs;
    assert_eq!(songs.len(), 1);
    assert_eq!(songs[0].title, "song");
    assert_eq!(songs[0].artist.as_deref(), Some("artist"));
    assert_eq!(songs[0].duration, Some(200));
    assert!(!songs[0].id.is_empty());

    let result = search(
        &catalogue(pool.clone(), vec![]),
        &context(),
        None,
        "",
        None,
        None,
        SourceTags::Off,
    )
    .await
    .expect("search succeeds");
    assert!(result.songs.is_empty());
    assert!(result.albums.is_empty());
    assert!(result.artists.is_empty());
}

#[sqlx::test]
async fn paging_caps_slices_and_rejects(pool: sqlx::PgPool) {
    let result = search(
        &catalogue(pool.clone(), vec![]),
        &context(),
        None,
        "song",
        Some("many"),
        None,
        SourceTags::Off,
    )
    .await;
    assert!(matches!(result, Err(SearchError::InvalidSearch)));

    let result = search(
        &catalogue(pool.clone(), vec![]),
        &context(),
        None,
        "song",
        None,
        Some("1001"),
        SourceTags::Off,
    )
    .await;
    assert!(matches!(result, Err(SearchError::InvalidSearch)));

    let service = paged_catalogue(pool.clone(), 25);
    async fn songs(
        catalogue: &SearchService,
        context: &RequestContext,
        count: &str,
        offset: &str,
    ) -> Vec<whio_subsonic_api::models::Child> {
        search(
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
        .songs
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

    let service = paged_catalogue(pool.clone(), 25);
    let songs = search(
        &service,
        &context,
        None,
        "song",
        Some("100"),
        None,
        SourceTags::Off,
    )
    .await
    .expect("search succeeds")
    .songs;
    assert_eq!(songs.len(), 25);
}

async fn album_catalogue(pool: sqlx::PgPool, albums: &[&str]) -> LibraryService {
    let repository = Arc::new(PostgresTrackRepository::new(pool.clone()));

    for title in albums {
        let metadata = AlbumMetadata::new((*title).to_owned())
            .unwrap()
            .with_album_artist(ArtistCredit::new("Daft Punk".to_owned(), None).unwrap());
        let album = repository.resolve_or_create_album(&metadata).await.unwrap();
        let track = repository
            .get_or_create(
                SourceIdentity::new(
                    ProviderId::new("local".to_owned()).unwrap(),
                    SourceScope::Named("/music".to_owned()),
                    format!("{title}.flac"),
                )
                .unwrap(),
                TrackMetadata::from_names(
                    format!("{title} track"),
                    vec!["Daft Punk".to_owned()],
                    Some(1_000),
                )
                .unwrap(),
            )
            .await
            .unwrap();
        repository
            .set_album_track(album.id(), track.id(), &AlbumTrack::unplaced())
            .await
            .unwrap();
    }

    LibraryService::new(
        Arc::clone(&repository) as Arc<dyn LibraryRead>,
        Arc::clone(&repository) as Arc<dyn SourceLookup>,
        Arc::clone(&repository) as Arc<dyn StateStore>,
    )
}

#[sqlx::test]
async fn album_list_maps_types_and_rejects_unknown(pool: sqlx::PgPool) {
    let catalogue = album_catalogue(pool.clone(), &["Discovery", "Homework"]).await;

    let albums = album_list(
        &catalogue,
        &AlbumListRequest {
            list_type: "alphabeticalByName",
            size: None,
            offset: None,
            from_year: None,
            to_year: None,
            genre: None,
        },
        None,
    )
    .await
    .expect("the list succeeds");
    let names = albums
        .iter()
        .map(|album| album.name.clone())
        .collect::<Vec<_>>();
    assert_eq!(names, ["Discovery", "Homework"]);

    let recent = album_list(
        &catalogue,
        &AlbumListRequest {
            list_type: "newest",
            size: Some("1"),
            offset: Some("0"),
            from_year: None,
            to_year: None,
            genre: None,
        },
        None,
    )
    .await
    .expect("the list succeeds");
    assert_eq!(recent.len(), 1);
    assert_eq!(recent[0].name, "Homework");

    assert!(
        album_list(
            &catalogue,
            &AlbumListRequest {
                list_type: "not-a-type",
                size: None,
                offset: None,
                from_year: None,
                to_year: None,
                genre: None,
            },
            None,
        )
        .await
        .is_err()
    );
    assert!(matches!(
        album_list(
            &catalogue,
            &AlbumListRequest {
                list_type: "alphabeticalByName",
                size: Some("many"),
                offset: None,
                from_year: None,
                to_year: None,
                genre: None,
            },
            None,
        )
        .await,
        Err(BrowseError::InvalidRequest)
    ));
}

#[sqlx::test]
async fn album_detail_lists_its_tracks_in_order(pool: sqlx::PgPool) {
    let repository = Arc::new(PostgresTrackRepository::new(pool.clone()));
    let album = repository
        .resolve_or_create_album(&AlbumMetadata::new("Discovery".to_owned()).unwrap())
        .await
        .unwrap();
    let second = repository
        .get_or_create(
            source("track-2"),
            TrackMetadata::from_names(
                "Second".to_owned(),
                vec!["Daft Punk".to_owned()],
                Some(2_000),
            )
            .unwrap(),
        )
        .await
        .unwrap();
    let first = repository
        .get_or_create(
            source("track-1"),
            TrackMetadata::from_names(
                "First".to_owned(),
                vec!["Daft Punk".to_owned()],
                Some(1_000),
            )
            .unwrap(),
        )
        .await
        .unwrap();
    repository
        .set_album_track(
            album.id(),
            second.id(),
            &AlbumTrack::new(Some(1), Some(2), 1).unwrap(),
        )
        .await
        .unwrap();
    repository
        .set_album_track(
            album.id(),
            first.id(),
            &AlbumTrack::new(Some(1), Some(1), 0).unwrap(),
        )
        .await
        .unwrap();
    let catalogue = CompletionService::disabled(Arc::new(LibraryService::new(
        Arc::clone(&repository) as Arc<dyn LibraryRead>,
        Arc::clone(&repository) as Arc<dyn SourceLookup>,
        Arc::clone(&repository) as Arc<dyn StateStore>,
    )));

    let detail = album_detail(&catalogue, &album.id().to_string(), None, SourceTags::Off)
        .await
        .expect("the album is found");

    assert_eq!(detail.song_count, 2);
    assert_eq!(detail.duration, 3);
    let titles = detail
        .song
        .iter()
        .map(|song| song.title.clone())
        .collect::<Vec<_>>();
    assert_eq!(titles, ["First", "Second"]);
    assert_eq!(detail.song[0].track, Some(1));
    assert_eq!(detail.song[0].disc_number, Some(1));

    assert!(matches!(
        album_detail(&catalogue, "not-a-uuid", None, SourceTags::Off).await,
        Err(BrowseError::AlbumNotFound)
    ));
    assert!(matches!(
        album_detail(
            &catalogue,
            &Uuid::now_v7().to_string(),
            None,
            SourceTags::Off
        )
        .await,
        Err(BrowseError::AlbumNotFound)
    ));
}

#[sqlx::test]
async fn artists_group_into_indexes(pool: sqlx::PgPool) {
    let repository = Arc::new(PostgresTrackRepository::new(pool.clone()));
    for name in ["Daft Punk", "99 Posse"] {
        repository
            .get_or_create(
                SourceIdentity::new(
                    ProviderId::new("local".to_owned()).unwrap(),
                    SourceScope::Named("/music".to_owned()),
                    format!("{name}.flac"),
                )
                .unwrap(),
                TrackMetadata::from_names(
                    format!("{name} song"),
                    vec![name.to_owned()],
                    Some(1_000),
                )
                .unwrap(),
            )
            .await
            .unwrap();
    }
    let catalogue = LibraryService::new(
        Arc::clone(&repository) as Arc<dyn LibraryRead>,
        Arc::clone(&repository) as Arc<dyn SourceLookup>,
        Arc::clone(&repository) as Arc<dyn StateStore>,
    );

    let artists = artists(&catalogue, None).await.expect("the list succeeds");
    let indexes = artist_indexes(artists);
    let names = indexes
        .iter()
        .map(|index| index.name.clone())
        .collect::<Vec<_>>();

    assert_eq!(names, ["#", "D"]);
    assert_eq!(indexes[1].artist.as_ref().unwrap()[0].name, "Daft Punk");
}
