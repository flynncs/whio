use super::*;
use crate::features::music_library::test_support::{
    CatalogueWrite, LibraryRead, SourceLookup, StateStore, TrackRepositoryError,
};
use crate::{
    features::music_library::catalogue::ProviderError,
    features::music_library::catalogue::{CatalogueCandidate, CatalogueProvider, CatalogueSearch},
    features::music_library::{
        domain::{
            AlbumMetadata, AlbumTrack, ArtistCredit, CreditRole, ProviderId, Resource,
            SourceIdentity, SourceScope, TrackMetadata,
        },
        library::ScanService,
        test_support::PostgresTrackRepository,
    },
    platform::raw_query::RawQuery,
    platform::request::{RequestContext, RequestId},
};
use async_trait::async_trait;
use std::{
    collections::VecDeque,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU32, Ordering},
    },
};
use uuid::Uuid;

use super::super::{enumeration_request, search_window};

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

fn windows(count: Option<&str>, offset: Option<&str>) -> SearchWindows {
    SearchWindows::uniform(search_window(count, offset).expect("window parses"))
}

#[sqlx::test]
async fn search_maps_tracks_and_short_circuits_empty(pool: sqlx::PgPool) {
    let service = catalogue(pool.clone(), vec![Ok(vec![candidate()])]);
    let result = search(
        &service,
        &context(),
        None,
        "song",
        &windows(Some("10"), None),
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
        &windows(None, None),
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
    assert!(matches!(
        search_window(Some("many"), None),
        Err(SearchError::InvalidSearch)
    ));
    assert!(matches!(
        search_window(None, Some("1001")),
        Err(SearchError::InvalidSearch)
    ));

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
            &windows(Some(count), Some(offset)),
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
        &windows(Some("100"), None),
        SourceTags::Off,
    )
    .await
    .expect("search succeeds")
    .songs;
    assert_eq!(songs.len(), 25);
}

async fn seed_albums(repository: &PostgresTrackRepository, albums: &[&str]) {
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
}

async fn album_catalogue(pool: sqlx::PgPool, albums: &[&str]) -> LibraryService {
    let repository = Arc::new(PostgresTrackRepository::new(pool.clone()));
    seed_albums(&repository, albums).await;

    LibraryService::new(
        Arc::clone(&repository) as Arc<dyn LibraryRead>,
        Arc::clone(&repository) as Arc<dyn SourceLookup>,
        Arc::clone(&repository) as Arc<dyn StateStore>,
    )
}

struct CountingLibrary {
    inner: Arc<dyn LibraryRead>,
    track_calls: AtomicU32,
    album_calls: AtomicU32,
    artist_calls: AtomicU32,
}

impl CountingLibrary {
    fn tracks(&self) -> u32 {
        self.track_calls.load(Ordering::SeqCst)
    }

    fn albums(&self) -> u32 {
        self.album_calls.load(Ordering::SeqCst)
    }

    fn artists(&self) -> u32 {
        self.artist_calls.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl LibraryRead for CountingLibrary {
    async fn find_track(
        &self,
        track_id: &crate::features::music_library::domain::TrackId,
    ) -> Result<Option<crate::features::music_library::domain::Track>, TrackRepositoryError> {
        self.inner.find_track(track_id).await
    }

    async fn list_albums(
        &self,
        offset: u32,
        limit: u32,
        order: AlbumOrder,
        filter: AlbumListFilter,
        user: Option<Uuid>,
    ) -> Result<Vec<crate::features::music_library::domain::AlbumSummary>, TrackRepositoryError>
    {
        self.album_calls.fetch_add(1, Ordering::SeqCst);

        self.inner
            .list_albums(offset, limit, order, filter, user)
            .await
    }

    async fn list_artists(
        &self,
        offset: u32,
        limit: u32,
        sort: ArtistSort,
        filter: &ArtistListFilter,
        user: Option<Uuid>,
    ) -> Result<Vec<crate::features::music_library::domain::ArtistSummary>, TrackRepositoryError>
    {
        self.artist_calls.fetch_add(1, Ordering::SeqCst);

        self.inner
            .list_artists(offset, limit, sort, filter, user)
            .await
    }

    async fn page_tracks(
        &self,
        offset: u32,
        limit: u32,
        order: TrackOrder,
        filter: &TrackListFilter,
        user: Option<Uuid>,
    ) -> Result<crate::features::music_library::domain::TrackPage, TrackRepositoryError> {
        self.track_calls.fetch_add(1, Ordering::SeqCst);

        self.inner
            .page_tracks(offset, limit, order, filter, user)
            .await
    }
}

async fn counting_catalogue(
    pool: sqlx::PgPool,
    albums: &[&str],
) -> (LibraryService, Arc<CountingLibrary>) {
    let repository = Arc::new(PostgresTrackRepository::new(pool.clone()));
    seed_albums(&repository, albums).await;
    let counting = Arc::new(CountingLibrary {
        inner: Arc::clone(&repository) as Arc<dyn LibraryRead>,
        track_calls: AtomicU32::new(0),
        album_calls: AtomicU32::new(0),
        artist_calls: AtomicU32::new(0),
    });

    let library = LibraryService::new(
        Arc::clone(&counting) as Arc<dyn LibraryRead>,
        Arc::clone(&repository) as Arc<dyn SourceLookup>,
        Arc::clone(&repository) as Arc<dyn StateStore>,
    );

    (library, counting)
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
        let from_names = |title: &str, artist: &str| {
            TrackMetadata::from_names(title.to_owned(), vec![artist.to_owned()], Some(1_000))
                .unwrap()
        };
        let album = repository
            .resolve_or_create_album(
                &AlbumMetadata::new(format!("{name} album"))
                    .unwrap()
                    .with_album_artist(ArtistCredit::new(name.to_owned(), None).unwrap()),
            )
            .await
            .unwrap();
        let track = repository
            .get_or_create(
                SourceIdentity::new(
                    ProviderId::new("local".to_owned()).unwrap(),
                    SourceScope::Named("/music".to_owned()),
                    format!("{name}.flac"),
                )
                .unwrap(),
                from_names(&format!("{name} song"), name),
            )
            .await
            .unwrap();
        repository
            .set_album_track(album.id(), track.id(), &AlbumTrack::unplaced())
            .await
            .unwrap();
    }
    repository
        .get_or_create(
            SourceIdentity::new(
                ProviderId::new("local".to_owned()).unwrap(),
                SourceScope::Named("/music".to_owned()),
                "session.flac".to_owned(),
            )
            .unwrap(),
            TrackMetadata::from_names(
                "session song".to_owned(),
                vec!["Soloist".to_owned()],
                Some(1_000),
            )
            .unwrap()
            .with_credits(vec![
                ArtistCredit::new("Session Composer".to_owned(), None)
                    .unwrap()
                    .with_role(CreditRole::Composer),
            ]),
        )
        .await
        .unwrap();
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

    let all_roles = catalogue
        .artist_list(0, 50, ArtistSort::Name, &ArtistListFilter::new(), None)
        .await
        .expect("the unfiltered list succeeds")
        .into_iter()
        .map(|summary| summary.artist().name().to_owned())
        .collect::<Vec<_>>();

    assert_eq!(
        all_roles,
        ["99 Posse", "Daft Punk", "Session Composer", "Soloist"],
        "the scoped index drops the credit-only artists the unfiltered list keeps"
    );
}

#[test]
fn enumeration_windows_reject_counts_above_the_page_limit() {
    assert!(matches!(
        EnumerationWindow::new(MAX_PAGE_LIMIT, 0),
        Ok(window) if window.count == MAX_PAGE_LIMIT
    ));
    assert!(matches!(
        EnumerationWindow::new(0, 5),
        Ok(window) if window.count == 0 && window.offset == 5
    ));
    assert!(matches!(
        EnumerationWindow::new(MAX_PAGE_LIMIT + 1, 0),
        Err(SearchError::InvalidSearch)
    ));
}

#[sqlx::test]
async fn enumerate_pages_each_kind_independently(pool: sqlx::PgPool) {
    let library = album_catalogue(
        pool.clone(),
        &["Discovery", "Homework", "Random Access Memories"],
    )
    .await;
    let request = EnumerationRequest {
        songs: EnumerationWindow::new(1, 1).expect("valid window"),
        albums: EnumerationWindow::new(1, 2).expect("valid window"),
        artists: EnumerationWindow::new(1, 0).expect("valid window"),
    };

    let result = enumerate(&library, None, &request, SourceTags::Off)
        .await
        .expect("the enumeration succeeds");

    assert_eq!(
        result
            .songs
            .iter()
            .map(|song| song.title.clone())
            .collect::<Vec<_>>(),
        ["Homework track"]
    );
    assert_eq!(result.albums.len(), 1);
    assert_eq!(result.albums[0].name, "Random Access Memories");
    assert_eq!(result.artists.len(), 1);
    assert_eq!(result.artists[0].name, "Daft Punk");
}

#[sqlx::test]
async fn enumerate_zero_count_performs_no_work_for_that_kind(pool: sqlx::PgPool) {
    let (library, calls) = counting_catalogue(pool.clone(), &["Discovery", "Homework"]).await;
    let zeroed = EnumerationRequest {
        songs: EnumerationWindow::new(0, 0).expect("valid window"),
        albums: EnumerationWindow::new(0, 0).expect("valid window"),
        artists: EnumerationWindow::new(0, 0).expect("valid window"),
    };

    let result = enumerate(&library, None, &zeroed, SourceTags::Off)
        .await
        .expect("the enumeration succeeds");

    assert!(result.songs.is_empty());
    assert!(result.albums.is_empty());
    assert!(result.artists.is_empty());
    assert_eq!(
        (calls.tracks(), calls.albums(), calls.artists()),
        (0, 0, 0),
        "a zero window must not reach the library"
    );

    let served = EnumerationRequest {
        songs: EnumerationWindow::new(1, 0).expect("valid window"),
        albums: EnumerationWindow::new(1, 0).expect("valid window"),
        artists: EnumerationWindow::new(1, 0).expect("valid window"),
    };

    let result = enumerate(&library, None, &served, SourceTags::Off)
        .await
        .expect("the enumeration succeeds");

    assert_eq!(result.songs.len(), 1);
    assert_eq!(result.albums.len(), 1);
    assert_eq!(result.artists.len(), 1);
    assert_eq!(
        (calls.tracks(), calls.albums(), calls.artists()),
        (1, 1, 1),
        "a non-zero window must reach the library exactly once per kind"
    );
}

#[sqlx::test]
async fn enumerate_skips_kinds_with_a_zero_count(pool: sqlx::PgPool) {
    let library = album_catalogue(pool.clone(), &["Discovery", "Homework"]).await;
    let request = EnumerationRequest {
        songs: EnumerationWindow::new(0, 0).expect("valid window"),
        albums: EnumerationWindow::new(2, 0).expect("valid window"),
        artists: EnumerationWindow::new(0, 0).expect("valid window"),
    };

    let result = enumerate(&library, None, &request, SourceTags::Off)
        .await
        .expect("the enumeration succeeds");

    assert!(result.songs.is_empty());
    assert!(result.artists.is_empty());
    assert_eq!(result.albums.len(), 2);
}

#[test]
fn newest_carries_the_owned_scope_in_the_filter() {
    let newest = AlbumListRequest {
        list_type: "newest",
        size: None,
        offset: None,
        from_year: None,
        to_year: None,
        genre: None,
    };
    let (order, filter) = album_list_order(&newest).expect("the type is supported");

    assert_eq!(order, AlbumOrder::RecentlyAdded);
    assert_eq!(
        filter.scope(),
        AlbumListScope::Owned,
        "newest means added to the library, so the scope travels in the filter"
    );
    assert_eq!(
        filter.resolved_direction(order),
        SortDirection::Descending,
        "and the order keeps its natural direction"
    );

    let frequent = AlbumListRequest {
        list_type: "frequent",
        size: None,
        offset: None,
        from_year: None,
        to_year: None,
        genre: None,
    };
    let (order, filter) = album_list_order(&frequent).expect("the type is supported");

    assert_eq!(order, AlbumOrder::PlayCount);
    assert_eq!(
        filter.scope(),
        AlbumListScope::Engaged,
        "another order is engaged, so the scope is not a property of the order value"
    );
}

#[test]
fn state_types_carry_their_membership_in_the_filter() {
    for (list_type, order) in [
        ("starred", AlbumOrder::StarredAt),
        ("frequent", AlbumOrder::PlayCount),
        ("recent", AlbumOrder::PlayDate),
        ("highest", AlbumOrder::Rating),
    ] {
        let request = AlbumListRequest {
            list_type,
            size: None,
            offset: None,
            from_year: None,
            to_year: None,
            genre: None,
        };
        let (observed, filter) = album_list_order(&request).expect("the type is supported");

        assert_eq!(observed, order, "{list_type}");

        let membership = match list_type {
            "starred" => filter.starred(),
            "recent" => filter.recently_played(),
            "highest" => filter.has_rating(),
            _ => None,
        };

        if list_type == "frequent" {
            assert_eq!(
                filter.play_count(),
                Some(Range::new(Some(1u32), None).unwrap()),
                "frequent means a positive album play count"
            );
        } else {
            assert_eq!(membership, Some(true), "{list_type} filters on its state");
        }

        assert_eq!(
            filter.resolved_direction(observed),
            SortDirection::Descending,
            "{list_type} keeps its natural direction"
        );
    }
}

#[test]
fn by_year_carries_its_range_and_direction() {
    let ascending = AlbumListRequest {
        list_type: "byYear",
        size: None,
        offset: None,
        from_year: Some("1990"),
        to_year: Some("2000"),
        genre: None,
    };
    let (order, filter) = album_list_order(&ascending).expect("the type is supported");

    assert_eq!(order, AlbumOrder::MaxYear);
    assert_eq!(filter.resolved_direction(order), SortDirection::Ascending);

    let year = filter.year().expect("the year range is present");

    assert_eq!((year.from(), year.to()), (Some(1990), Some(2000)));

    let inverted = AlbumListRequest {
        list_type: "byYear",
        size: None,
        offset: None,
        from_year: Some("2000"),
        to_year: Some("1990"),
        genre: None,
    };
    let (order, filter) = album_list_order(&inverted).expect("the type is supported");

    assert_eq!(order, AlbumOrder::MaxYear);
    assert_eq!(filter.resolved_direction(order), SortDirection::Descending);

    let year = filter.year().expect("the year range is present");

    assert_eq!(
        (year.from(), year.to()),
        (Some(1990), Some(2000)),
        "the inverted pair is normalised into a valid range"
    );

    let missing = AlbumListRequest {
        list_type: "byYear",
        size: None,
        offset: None,
        from_year: None,
        to_year: Some("1990"),
        genre: None,
    };

    assert!(album_list_order(&missing).is_err());

    let unparsable = AlbumListRequest {
        list_type: "byYear",
        size: None,
        offset: None,
        from_year: Some("many"),
        to_year: Some("1990"),
        genre: None,
    };

    assert!(matches!(
        album_list_order(&unparsable),
        Err(BrowseError::InvalidRequest)
    ));
}

#[test]
fn by_genre_carries_one_normalised_name() {
    let rock = AlbumListRequest {
        list_type: "byGenre",
        size: None,
        offset: None,
        from_year: None,
        to_year: None,
        genre: Some("  Rock "),
    };
    let (order, filter) = album_list_order(&rock).expect("the type is supported");

    assert_eq!(order, AlbumOrder::Name);
    assert_eq!(
        filter.genre().map(|genre| genre.len()),
        Some(1),
        "the foreign single name becomes a one-value set"
    );

    let blank = AlbumListRequest {
        list_type: "byGenre",
        size: None,
        offset: None,
        from_year: None,
        to_year: None,
        genre: Some("   "),
    };

    assert!(matches!(
        album_list_order(&blank),
        Err(BrowseError::MissingParameter)
    ));

    let long = "a".repeat(501);
    let over_long = AlbumListRequest {
        list_type: "byGenre",
        size: None,
        offset: None,
        from_year: None,
        to_year: None,
        genre: Some(&long),
    };

    assert!(matches!(
        album_list_order(&over_long),
        Err(BrowseError::InvalidRequest)
    ));
}

fn library_over(repository: &Arc<PostgresTrackRepository>) -> LibraryService {
    LibraryService::new(
        Arc::clone(repository) as Arc<dyn LibraryRead>,
        Arc::clone(repository) as Arc<dyn SourceLookup>,
        Arc::clone(repository) as Arc<dyn StateStore>,
    )
}

fn local_source(external_id: &str) -> SourceIdentity {
    SourceIdentity::new(
        ProviderId::new("local".to_owned()).unwrap(),
        SourceScope::Named("/music".to_owned()),
        external_id.to_owned(),
    )
    .unwrap()
}

async fn seed_three_kinds(pool: &sqlx::PgPool) {
    let repository = PostgresTrackRepository::new(pool.clone());

    for (album_title, artist, first) in [
        ("Album A", "Artist One", 1u32),
        ("Album B", "Artist Two", 3),
        ("Album C", "Artist Three", 5),
    ] {
        let album = repository
            .resolve_or_create_album(
                &AlbumMetadata::new(album_title.to_owned())
                    .unwrap()
                    .with_album_artist(ArtistCredit::new(artist.to_owned(), None).unwrap()),
            )
            .await
            .unwrap();

        for number in [first, first + 1] {
            let track = repository
                .get_or_create(
                    local_source(&format!("path-{number:02}")),
                    TrackMetadata::from_names(
                        format!("song-{number:02}"),
                        vec![artist.to_owned()],
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
    }
}

fn songs_of(children: &[whio_subsonic_api::models::Child]) -> Vec<String> {
    children.iter().map(|child| child.title.clone()).collect()
}

#[sqlx::test]
async fn subsonic_songs_by_genre_uses_the_stored_set(pool: sqlx::PgPool) {
    let repository = Arc::new(PostgresTrackRepository::new(pool.clone()));

    for title in ["Rock One", "Rock Two", "Rock Three"] {
        repository
            .get_or_create(
                local_source(&format!("{title}.flac")),
                TrackMetadata::from_names(
                    title.to_owned(),
                    vec!["Daft Punk".to_owned()],
                    Some(1_000),
                )
                .unwrap()
                .with_genres(vec!["Pop".to_owned(), "Rock".to_owned()]),
            )
            .await
            .unwrap();
    }

    let (library, calls) = counting_catalogue(pool.clone(), &[]).await;
    let stored = |genre: Option<&'static str>, count, offset| SongsByGenreRequest {
        genre,
        count,
        offset,
    };

    let projection: String = sqlx::query_scalar("SELECT genre FROM tracks ORDER BY title LIMIT 1")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(
        projection, "Pop",
        "the projection holds the first genre only"
    );

    let first = songs_by_genre(
        &library,
        None,
        &stored(Some("Rock"), Some("2"), Some("0")),
        SourceTags::Off,
    )
    .await
    .expect("the stored genre finds the songs");
    assert_eq!(songs_of(&first), ["Rock One", "Rock Three"]);
    assert_eq!(
        calls.tracks(),
        1,
        "the genre window reaches the library once"
    );

    let second = songs_by_genre(
        &library,
        None,
        &stored(Some("Rock"), Some("2"), Some("2")),
        SourceTags::Off,
    )
    .await
    .expect("the offset pages the same membership");
    assert_eq!(songs_of(&second), ["Rock Two"]);

    let absent = songs_by_genre(
        &library,
        None,
        &stored(Some("Jazz"), None, None),
        SourceTags::Off,
    )
    .await
    .expect("an unknown genre is an empty page");
    assert!(absent.is_empty());

    let before = calls.tracks();

    let missing = songs_by_genre(&library, None, &stored(None, None, None), SourceTags::Off)
        .await
        .expect("an absent genre is the reference's empty page, not an error");
    assert!(missing.is_empty());
    let blank = songs_by_genre(
        &library,
        None,
        &stored(Some("   "), None, None),
        SourceTags::Off,
    )
    .await
    .expect("a blank genre is the same empty page");
    assert!(blank.is_empty());
    assert_eq!(
        calls.tracks(),
        before,
        "an absent or blank genre never reaches the library"
    );

    let long = "a".repeat(501);
    let over_long = SongsByGenreRequest {
        genre: Some(&long),
        count: None,
        offset: None,
    };
    assert!(matches!(
        songs_by_genre(&library, None, &over_long, SourceTags::Off).await,
        Err(BrowseError::InvalidRequest)
    ));
    assert!(matches!(
        songs_by_genre(
            &library,
            None,
            &stored(Some("Rock"), Some("many"), None),
            SourceTags::Off
        )
        .await,
        Err(BrowseError::InvalidRequest)
    ));
}

#[sqlx::test]
async fn subsonic_random_songs_is_one_permutation(pool: sqlx::PgPool) {
    let repository = Arc::new(PostgresTrackRepository::new(pool.clone()));

    for index in 0..40u32 {
        repository
            .get_or_create(
                local_source(&format!("random-{index:03}.flac")),
                TrackMetadata::from_names(
                    format!("random {index:03}"),
                    vec!["Narrator".to_owned()],
                    Some(1_000),
                )
                .unwrap(),
            )
            .await
            .unwrap();
    }

    let library = library_over(&repository);
    let user = Uuid::now_v7();
    let day = chrono::Utc::now().date_naive();
    let seed = RandomSeed::for_request(Some(user), Resource::Track, day);
    let other = RandomSeed::for_request(Some(Uuid::now_v7()), Resource::Track, day);
    let request = |size: &'static str| RandomSongsRequest {
        size: Some(size),
        genre: None,
        from_year: None,
        to_year: None,
    };

    let whole = random_songs(&library, Some(&user), &request("40"), seed, SourceTags::Off)
        .await
        .expect("the random page succeeds");
    let again = random_songs(&library, Some(&user), &request("40"), seed, SourceTags::Off)
        .await
        .expect("the random page succeeds");
    let head = random_songs(&library, Some(&user), &request("10"), seed, SourceTags::Off)
        .await
        .expect("the random page succeeds");
    let elsewhere = random_songs(
        &library,
        Some(&user),
        &request("40"),
        other,
        SourceTags::Off,
    )
    .await
    .expect("the random page succeeds");

    assert_eq!(whole.len(), 40);
    assert_eq!(
        songs_of(&whole),
        songs_of(&again),
        "the same derived seed repeats the order"
    );
    assert_eq!(
        songs_of(&head),
        songs_of(&whole)[..10],
        "a smaller window is the head of one permutation"
    );
    assert_ne!(
        songs_of(&whole),
        songs_of(&elsewhere),
        "a different seed is a different permutation"
    );
}

#[sqlx::test]
async fn subsonic_indexes_is_the_artist_index(pool: sqlx::PgPool) {
    let repository = Arc::new(PostgresTrackRepository::new(pool.clone()));

    for name in ["Daft Punk", "The Beatles", "99 Posse", "Las Vegas"] {
        let album = repository
            .resolve_or_create_album(
                &AlbumMetadata::new(format!("{name} album"))
                    .unwrap()
                    .with_album_artist(ArtistCredit::new(name.to_owned(), None).unwrap()),
            )
            .await
            .unwrap();
        let track = repository
            .get_or_create(
                local_source(&format!("{name}.flac")),
                TrackMetadata::from_names(
                    format!("{name} song"),
                    vec![name.to_owned()],
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

    let library = library_over(&repository);
    let indexes = indexes(&library, &ScanService::disabled(), None)
        .await
        .expect("the index succeeds");

    assert_eq!(
        indexes.ignored_articles, "The El La Los Las Le Les Os As O A",
        "the wire string is the reference default, not whatever the producer holds"
    );
    assert_eq!(
        indexes.last_modified,
        i32::MAX,
        "ms since epoch overflow the generated i32 field, so the value saturates"
    );

    let index = indexes.index.expect("the index list is present");
    let letters = index
        .iter()
        .map(|index| index.name.as_str())
        .collect::<Vec<_>>();
    assert_eq!(letters, ["#", "B", "D", "V"]);

    let filed = index
        .iter()
        .flat_map(|index| index.artist.clone().unwrap_or_default())
        .map(|artist| artist.name)
        .collect::<Vec<_>>();
    let listed = artists(&library, None)
        .await
        .expect("the artist list succeeds")
        .iter()
        .map(|artist| artist.name.clone())
        .collect::<Vec<_>>();

    assert_eq!(
        filed.len(),
        listed.len(),
        "the grouping keeps the membership"
    );
    for name in listed {
        assert!(
            filed.contains(&name),
            "{name} is filed under an index letter"
        );
    }
    assert!(
        index[1]
            .artist
            .as_ref()
            .unwrap()
            .iter()
            .any(|artist| artist.name == "The Beatles"),
        "the article is ignored when the letter is chosen"
    );
}

#[sqlx::test]
async fn subsonic_music_directory_walks_entities(pool: sqlx::PgPool) {
    let repository = Arc::new(PostgresTrackRepository::new(pool.clone()));
    let album = repository
        .resolve_or_create_album(
            &AlbumMetadata::new("Discovery".to_owned())
                .unwrap()
                .with_album_artist(ArtistCredit::new("Daft Punk".to_owned(), None).unwrap()),
        )
        .await
        .unwrap();
    let mut songs = Vec::new();

    for (number, title) in [(1u32, "First"), (2, "Second")] {
        let track = repository
            .get_or_create(
                local_source(&format!("path-{number}")),
                TrackMetadata::from_names(
                    title.to_owned(),
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
                track.id(),
                &AlbumTrack::new(Some(1), Some(number as u16), number - 1).unwrap(),
            )
            .await
            .unwrap();
        songs.push(track.id().to_string());
    }

    let library = library_over(&repository);
    let artist = artists(&library, None)
        .await
        .expect("the artist list succeeds")
        .remove(0);
    let artist_id = artist.id.clone();

    let directory = music_directory(&library, &artist_id, None, SourceTags::Off)
        .await
        .expect("the artist directory resolves");
    assert_eq!(directory.id, artist_id);
    assert_eq!(directory.name, "Daft Punk");

    let children = directory.child.expect("the artist has children");
    assert_eq!(children.len(), 1);
    assert!(children[0].is_dir);
    assert_eq!(children[0].title, "Discovery");
    assert_eq!(children[0].id, album.id().to_string());
    assert_eq!(children[0].artist.as_deref(), Some("Daft Punk"));
    assert!(
        children.iter().all(|child| child.path.is_none()),
        "no filesystem path is exposed"
    );

    let album_id = children[0].id.clone();
    let directory = music_directory(&library, &album_id, None, SourceTags::Off)
        .await
        .expect("the album directory resolves");
    assert_eq!(directory.id, album_id);
    assert_eq!(directory.parent.as_deref(), Some(artist_id.as_str()));
    assert_eq!(directory.name, "Discovery");

    let children = directory.child.expect("the album has children");
    assert_eq!(songs_of(&children), ["First", "Second"]);
    assert_eq!(
        children
            .iter()
            .map(|child| child.id.clone())
            .collect::<Vec<_>>(),
        songs,
        "the child ids are the canonical track ids"
    );
    assert!(children.iter().all(|child| !child.is_dir));
    assert!(
        children.iter().all(|child| child.path.is_none()),
        "no filesystem path is exposed"
    );

    let provider_album = repository
        .resolve_or_create_album(&AlbumMetadata::new("Provider Only".to_owned()).unwrap())
        .await
        .unwrap();
    let provider_track = repository
        .get_or_create(
            source("vid-1"),
            TrackMetadata::from_names(
                "Provider Song".to_owned(),
                vec!["Daft Punk".to_owned()],
                Some(2_000),
            )
            .unwrap(),
        )
        .await
        .unwrap();
    repository
        .set_album_track(
            provider_album.id(),
            provider_track.id(),
            &AlbumTrack::unplaced(),
        )
        .await
        .unwrap();

    let detail = library
        .album_detail(provider_album.id(), None)
        .await
        .expect("the album read succeeds")
        .expect("the album is known");
    let directory = music_directory(
        &library,
        &provider_album.id().to_string(),
        None,
        SourceTags::Off,
    )
    .await
    .expect("the provider album resolves");

    assert_eq!(
        directory
            .child
            .unwrap_or_default()
            .iter()
            .map(|child| child.id.clone())
            .collect::<Vec<_>>(),
        detail
            .entries
            .iter()
            .map(|entry| entry.track().id().to_string())
            .collect::<Vec<_>>(),
        "a provider-only entity is reported as the library reports it"
    );
    assert_eq!(
        detail
            .entries
            .iter()
            .map(|entry| entry.track().id().to_string())
            .collect::<Vec<_>>(),
        [provider_track.id().to_string()],
        "the library reports the provider track"
    );

    assert!(matches!(
        music_directory(&library, &Uuid::now_v7().to_string(), None, SourceTags::Off).await,
        Err(BrowseError::AlbumNotFound)
    ));
    assert!(matches!(
        music_directory(&library, "not-a-uuid", None, SourceTags::Off).await,
        Err(BrowseError::ArtistNotFound)
    ));
}

#[sqlx::test]
async fn subsonic_search3_windows_are_per_type(pool: sqlx::PgPool) {
    seed_three_kinds(&pool).await;
    let library = library_over(&Arc::new(PostgresTrackRepository::new(pool.clone())));

    let request = enumeration_request(&RawQuery::parse(
        "songCount=2&songOffset=1&albumCount=1&albumOffset=1&artistCount=1&artistOffset=2",
    ))
    .expect("the windows parse");
    let result = enumerate(&library, None, &request, SourceTags::Off)
        .await
        .expect("the enumeration succeeds");

    assert_eq!(songs_of(&result.songs), ["song-02", "song-03"]);
    assert_eq!(
        result
            .albums
            .iter()
            .map(|album| album.name.clone())
            .collect::<Vec<_>>(),
        ["Album B"]
    );
    assert_eq!(
        result
            .artists
            .iter()
            .map(|artist| artist.name.clone())
            .collect::<Vec<_>>(),
        ["Artist Two"]
    );

    let only_songs =
        enumeration_request(&RawQuery::parse("songCount=1")).expect("the window parses");
    let result = enumerate(&library, None, &only_songs, SourceTags::Off)
        .await
        .expect("the enumeration succeeds");

    assert_eq!(songs_of(&result.songs), ["song-01"]);
    assert_eq!(
        result.albums.len(),
        3,
        "a request with only songCount does not truncate the albums"
    );
    assert_eq!(
        result.artists.len(),
        3,
        "a request with only songCount does not truncate the artists"
    );
}

#[sqlx::test]
async fn subsonic_album_and_artist_details_use_the_shared_operations(pool: sqlx::PgPool) {
    let repository = Arc::new(PostgresTrackRepository::new(pool.clone()));
    let album = repository
        .resolve_or_create_album(
            &AlbumMetadata::new("Discovery".to_owned())
                .unwrap()
                .with_album_artist(ArtistCredit::new("Daft Punk".to_owned(), None).unwrap()),
        )
        .await
        .unwrap();
    let credited = repository
        .resolve_or_create_album(
            &AlbumMetadata::new("Guest".to_owned())
                .unwrap()
                .with_album_artist(ArtistCredit::new("Another Artist".to_owned(), None).unwrap()),
        )
        .await
        .unwrap();

    for (album, number, title) in [
        (album.id().clone(), 1u16, "First"),
        (album.id().clone(), 2, "Second"),
        (credited.id().clone(), 1, "Guest Song"),
    ] {
        let track = repository
            .get_or_create(
                local_source(&format!("{title}.flac")),
                TrackMetadata::from_names(
                    title.to_owned(),
                    vec!["Daft Punk".to_owned()],
                    Some(1_000),
                )
                .unwrap(),
            )
            .await
            .unwrap();
        repository
            .set_album_track(
                &album,
                track.id(),
                &AlbumTrack::new(Some(1), Some(number), u32::from(number) - 1).unwrap(),
            )
            .await
            .unwrap();
    }

    let library = library_over(&repository);
    let completion = CompletionService::disabled(Arc::new(library_over(&repository)));
    let albums = library
        .album_list(0, u32::MAX, AlbumOrder::Name, AlbumListFilter::new(), None)
        .await
        .expect("the album list succeeds");
    let artist = library
        .artist_list(
            0,
            u32::MAX,
            ArtistSort::Name,
            &ArtistListFilter::default(),
            None,
        )
        .await
        .expect("the artist list succeeds")
        .into_iter()
        .find(|artist| artist.artist().name() == "Daft Punk")
        .expect("the credited artist is listed");

    let target = artist.artist().id().clone();
    let album_artist_membership = albums
        .iter()
        .filter(|summary| {
            summary.album_artist().map(|album_artist| album_artist.id()) == Some(&target)
        })
        .map(|summary| summary.album().id().to_string())
        .collect::<Vec<_>>();
    let participated = library
        .track_page(
            0,
            PageLimit::new(50).expect("valid page"),
            TrackOrder::Title,
            &TrackListFilter::new()
                .with_artists_id(Some(IdSet::new([target.clone()]).expect("valid id set"))),
            None,
        )
        .await
        .expect("the credited tracks are read");
    let mut participation_membership = participated
        .items()
        .iter()
        .filter_map(|view| view.album().map(|album| album.id().to_string()))
        .collect::<Vec<_>>();

    participation_membership.sort();
    participation_membership.dedup();

    assert_eq!(
        (
            album_artist_membership.len(),
            participation_membership.len()
        ),
        (1, 2),
        "the fixture separates the album-artist membership from the participation set"
    );

    let summary = albums
        .iter()
        .find(|summary| summary.album().title() == "Discovery")
        .expect("the album is listed");
    let detail = album_detail(
        &completion,
        &summary.album().id().to_string(),
        None,
        SourceTags::Off,
    )
    .await
    .expect("the album detail succeeds");

    assert_eq!(detail.name, summary.album().title());
    assert_eq!(detail.song_count, summary.track_count() as i32);
    assert_eq!(detail.duration, (summary.duration_ms() / 1000) as i32);
    assert_eq!(detail.song.len(), summary.track_count() as usize);

    let detail = artist_detail(&library, &artist.artist().id().to_string(), None)
        .await
        .expect("the artist detail succeeds");
    let files = detail
        .album
        .iter()
        .map(|album| album.id.clone())
        .collect::<Vec<_>>();

    assert_eq!(detail.name, artist.artist().name());
    assert_eq!(
        files, album_artist_membership,
        "the album-artist membership, not the wider participation set a facade recount would fold in"
    );
    assert_eq!(
        detail.album_count,
        Some(album_artist_membership.len() as i32)
    );

    assert!(matches!(
        album_detail(
            &completion,
            &Uuid::now_v7().to_string(),
            None,
            SourceTags::Off
        )
        .await,
        Err(BrowseError::AlbumNotFound)
    ));
    assert!(matches!(
        artist_detail(&library, &Uuid::now_v7().to_string(), None).await,
        Err(BrowseError::ArtistNotFound)
    ));
}
