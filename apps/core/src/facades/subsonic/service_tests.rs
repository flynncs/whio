use super::*;
use crate::features::music_library::test_support::{
    CatalogueWrite, LibraryRead, ListenStore, SourceLookup, StateStore, TrackRepositoryError,
};
use crate::{
    features::identity::{
        api::native::AuthenticatedPrincipal,
        domain::{Principal, UserId, UserRole},
        secrets::CredentialKey,
    },
    features::metadata::capabilities::{
        CAPABILITY_TTL, CapabilityError, CapabilitySnapshot, CapabilitySource, Snapshot,
    },
    features::metadata::domain::{CandidateItem, FieldId, FieldValue, FieldWrite, MatchBasis},
    features::metadata::freshness::NoJitter,
    features::metadata::refresh::{DocumentError, DocumentOutcome, MetadataDocuments},
    features::metadata::selection::{FieldOrder, YOUTUBE_MUSIC},
    features::metadata::service::{MetadataProfile, MetadataService},
    features::metadata::{MetadataStore, PostgresMetadataStore},
    features::music_library::EnrichmentGuard,
    features::music_library::api::artist_info::{ArtistInfoState, related, top},
    features::music_library::catalogue::ProviderError,
    features::music_library::catalogue::{
        CatalogueCandidate, CatalogueProvider, CatalogueSearch, CompletionService, SearchService,
    },
    features::music_library::history::ListenService,
    features::music_library::{
        domain::{
            AlbumMetadata, AlbumTrack, ArtistCredit, CreditRole, ProviderId, Resource,
            SourceIdentity, SourceScope, TrackMetadata,
        },
        library::ScanService,
        test_support::PostgresTrackRepository,
    },
    features::playback::PlaybackActivityService,
    platform::raw_query::RawQuery,
    platform::request::{RequestContext, RequestId},
};
use async_trait::async_trait;
use axum::{
    Extension, Json,
    extract::{Path, State},
    response::IntoResponse,
};
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
    async fn search_tracks(
        &self,
        _search: &CatalogueSearch,
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
    async fn search_tracks(
        &self,
        search: &CatalogueSearch,
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

fn library_for_pool(pool: sqlx::PgPool) -> Arc<LibraryService> {
    let store = Arc::new(PostgresTrackRepository::new(pool));

    Arc::new(LibraryService::new(
        Arc::clone(&store) as Arc<dyn LibraryRead>,
        Arc::clone(&store) as Arc<dyn SourceLookup>,
        Arc::clone(&store) as Arc<dyn StateStore>,
    ))
}

fn windows(count: Option<&str>, offset: Option<&str>) -> SearchWindows {
    SearchWindows::uniform(
        search_window(count, offset)
            .expect("window parses")
            .expect("window present"),
    )
}

#[sqlx::test]
async fn search_maps_tracks_and_short_circuits_empty(pool: sqlx::PgPool) {
    let service = catalogue(pool.clone(), vec![Ok(vec![candidate()])]);
    let library = library_for_pool(pool.clone());
    let result = search(
        &service,
        &library,
        &context(),
        None,
        "song",
        &windows(Some("10"), None),
        PayloadContext::default(),
    )
    .await
    .expect("search succeeds");
    let songs = result.songs;
    assert_eq!(songs.len(), 1);
    assert_eq!(songs[0].title, "song");
    assert_eq!(songs[0].artist.as_deref(), Some("artist"));
    assert_eq!(songs[0].duration, Some(200));
    assert!(!songs[0].id.is_empty());
}

#[test]
fn search_window_treats_a_zero_count_as_suppressed() {
    assert!(
        search_window(Some("0"), None)
            .expect("window parses")
            .is_none()
    );
    assert!(
        search_window(Some("0"), Some("100"))
            .expect("window parses")
            .is_none()
    );
    assert!(
        search_window(Some("10"), Some("0"))
            .expect("window parses")
            .is_some()
    );
    assert!(matches!(
        search_window(Some("0"), Some("abc")),
        Err(SearchError::InvalidSearch)
    ));
    assert!(matches!(
        search_window(Some("0"), Some("1001")),
        Err(SearchError::InvalidSearch)
    ));
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
    let library = library_for_pool(pool.clone());
    async fn songs(
        catalogue: &SearchService,
        library: &LibraryService,
        context: &RequestContext,
        count: &str,
        offset: &str,
    ) -> Vec<whio_subsonic_api::models::Child> {
        search(
            catalogue,
            library,
            context,
            None,
            "song",
            &windows(Some(count), Some(offset)),
            PayloadContext::default(),
        )
        .await
        .expect("page succeeds")
        .songs
    }
    let context = context();
    let first = songs(&service, &library, &context, "10", "0").await;
    let second = songs(&service, &library, &context, "10", "10").await;
    let third = songs(&service, &library, &context, "10", "20").await;
    let bounded = songs(&service, &library, &context, "10", "25").await;
    assert_eq!(first.len(), 10);
    assert_eq!(first[0].title, "song-0");
    assert_eq!(second[0].title, "song-10");
    assert_eq!(third.len(), 5);
    assert!(bounded.is_empty());

    let service = paged_catalogue(pool.clone(), 25);
    let library = library_for_pool(pool.clone());
    let songs = search(
        &service,
        &library,
        &context,
        None,
        "song",
        &windows(Some("100"), None),
        PayloadContext::default(),
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
    log: Mutex<Vec<String>>,
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

    fn note(&self, entry: String) {
        self.log.lock().expect("the call log").push(entry);
    }

    fn clear_log(&self) {
        self.log.lock().expect("the call log").clear();
    }

    fn composition_log(&self) -> Vec<String> {
        self.log
            .lock()
            .expect("the call log")
            .iter()
            .filter(|entry| {
                entry.starts_with("page_tracks")
                    || entry.starts_with("track_views")
                    || entry.starts_with("tracks_credited_to")
            })
            .cloned()
            .collect()
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

    async fn find_artist(
        &self,
        artist_id: &ArtistId,
        user: Option<Uuid>,
    ) -> Result<Option<crate::features::music_library::domain::Artist>, TrackRepositoryError> {
        self.inner.find_artist(artist_id, user).await
    }

    async fn find_artist_by_exact_name(
        &self,
        name: &str,
    ) -> Result<Option<ArtistId>, TrackRepositoryError> {
        self.note("find_artist_by_exact_name".to_owned());

        self.inner.find_artist_by_exact_name(name).await
    }

    async fn tracks_credited_to(
        &self,
        artist_ids: &[ArtistId],
    ) -> Result<Vec<crate::features::music_library::domain::TrackId>, TrackRepositoryError> {
        self.note(format!("tracks_credited_to:{}", artist_ids.len()));

        self.inner.tracks_credited_to(artist_ids).await
    }

    async fn dedup_candidates(
        &self,
        keys: &[String],
        limit: u32,
    ) -> Result<Vec<crate::features::music_library::domain::TrackHit>, TrackRepositoryError> {
        self.note(format!("dedup_candidates:{}", keys.len()));

        self.inner.dedup_candidates(keys, limit).await
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
        self.note("page_tracks".to_owned());

        self.inner
            .page_tracks(offset, limit, order, filter, user)
            .await
    }

    async fn track_views(
        &self,
        track_ids: &[crate::features::music_library::domain::TrackId],
        user: Option<Uuid>,
    ) -> Result<Vec<crate::features::music_library::domain::TrackView>, TrackRepositoryError> {
        self.note(format!("track_views:{}", track_ids.len()));

        self.inner.track_views(track_ids, user).await
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
        log: Mutex::new(Vec::new()),
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
            library_id: None,
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
            library_id: None,
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
                library_id: None,
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
                library_id: None,
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
    let catalogue = disabled_completion(&repository);

    let detail = album_detail(
        &catalogue,
        &album.id().to_string(),
        None,
        PayloadContext::default(),
    )
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
        album_detail(&catalogue, "not-a-uuid", None, PayloadContext::default()).await,
        Err(BrowseError::AlbumNotFound)
    ));
    assert!(matches!(
        album_detail(
            &catalogue,
            &Uuid::now_v7().to_string(),
            None,
            PayloadContext::default()
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
fn enumeration_windows_accept_counts_above_the_page_limit_up_to_ten_thousand() {
    assert!(matches!(
        EnumerationWindow::new(1000, 1000),
        Ok(window) if window.count == 1000 && window.offset == 1000
    ));
    assert!(matches!(
        EnumerationWindow::new(10_000, 0),
        Ok(window) if window.count == 10_000
    ));
    assert!(matches!(
        EnumerationWindow::new(0, 5),
        Ok(window) if window.count == 0 && window.offset == 5
    ));
    assert!(matches!(
        EnumerationWindow::new(10_001, 0),
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

    let result = enumerate(&library, None, &request, PayloadContext::default())
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
async fn enumerate_fills_a_count_above_the_page_limit_from_several_pages(pool: sqlx::PgPool) {
    let names = (0..MAX_PAGE_LIMIT + 20)
        .map(|index| format!("Album {index:04}"))
        .collect::<Vec<_>>();
    let titles = names.iter().map(String::as_str).collect::<Vec<_>>();
    let (library, calls) = counting_catalogue(pool.clone(), &titles).await;
    let request = EnumerationRequest {
        songs: EnumerationWindow::new(0, 0).expect("valid window"),
        albums: EnumerationWindow::new(1000, 10).expect("valid window"),
        artists: EnumerationWindow::new(0, 0).expect("valid window"),
    };

    let result = enumerate(&library, None, &request, PayloadContext::default())
        .await
        .expect("the enumeration succeeds");

    assert_eq!(result.albums.len(), names.len() - 10);
    assert_eq!(result.albums[0].name, "Album 0010");
    assert_eq!(
        result.albums.last().map(|album| album.name.as_str()),
        names.last().map(String::as_str)
    );
    assert_eq!(
        calls.albums(),
        2,
        "a short second page ends the enumeration"
    );
}

#[sqlx::test]
async fn enumerate_zero_count_performs_no_work_for_that_kind(pool: sqlx::PgPool) {
    let (library, calls) = counting_catalogue(pool.clone(), &["Discovery", "Homework"]).await;
    let zeroed = EnumerationRequest {
        songs: EnumerationWindow::new(0, 0).expect("valid window"),
        albums: EnumerationWindow::new(0, 0).expect("valid window"),
        artists: EnumerationWindow::new(0, 0).expect("valid window"),
    };

    let result = enumerate(&library, None, &zeroed, PayloadContext::default())
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

    let result = enumerate(&library, None, &served, PayloadContext::default())
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

    let result = enumerate(&library, None, &request, PayloadContext::default())
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
        library_id: None,
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
        library_id: None,
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
            library_id: None,
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
        library_id: None,
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
        library_id: None,
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
        library_id: None,
    };

    assert!(album_list_order(&missing).is_err());

    let unparsable = AlbumListRequest {
        list_type: "byYear",
        size: None,
        offset: None,
        from_year: Some("many"),
        to_year: Some("1990"),
        genre: None,
        library_id: None,
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
        library_id: None,
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
        library_id: None,
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
        library_id: None,
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

fn search_over(repository: &Arc<PostgresTrackRepository>) -> Arc<SearchService> {
    Arc::new(SearchService::new(
        Arc::clone(repository) as Arc<dyn LibraryRead>,
        Arc::clone(repository) as Arc<dyn CatalogueWrite>,
        Arc::clone(repository) as Arc<dyn SourceLookup>,
        Arc::clone(repository) as Arc<dyn StateStore>,
    ))
}

fn disabled_completion(repository: &Arc<PostgresTrackRepository>) -> CompletionService {
    CompletionService::disabled(Arc::new(library_over(repository)), search_over(repository))
}

fn discovery_over(repository: &Arc<PostgresTrackRepository>) -> DiscoveryService {
    DiscoveryService::new(
        Arc::clone(repository) as Arc<dyn LibraryRead>,
        Arc::clone(repository) as Arc<dyn SourceLookup>,
        Arc::clone(repository) as Arc<dyn StateStore>,
        Arc::clone(repository) as Arc<dyn ListenStore>,
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
        library_id: None,
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
        PayloadContext::default(),
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
        PayloadContext::default(),
    )
    .await
    .expect("the offset pages the same membership");
    assert_eq!(songs_of(&second), ["Rock Two"]);

    let absent = songs_by_genre(
        &library,
        None,
        &stored(Some("Jazz"), None, None),
        PayloadContext::default(),
    )
    .await
    .expect("an unknown genre is an empty page");
    assert!(absent.is_empty());

    let before = calls.tracks();

    let missing = songs_by_genre(
        &library,
        None,
        &stored(None, None, None),
        PayloadContext::default(),
    )
    .await
    .expect("an absent genre is the reference's empty page, not an error");
    assert!(missing.is_empty());
    let blank = songs_by_genre(
        &library,
        None,
        &stored(Some("   "), None, None),
        PayloadContext::default(),
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
        library_id: None,
    };
    assert!(matches!(
        songs_by_genre(&library, None, &over_long, PayloadContext::default()).await,
        Err(BrowseError::InvalidRequest)
    ));
    assert!(matches!(
        songs_by_genre(
            &library,
            None,
            &stored(Some("Rock"), Some("many"), None),
            PayloadContext::default()
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
        library_id: None,
    };

    let whole = random_songs(
        &library,
        Some(&user),
        &request("40"),
        seed,
        PayloadContext::default(),
    )
    .await
    .expect("the random page succeeds");
    let again = random_songs(
        &library,
        Some(&user),
        &request("40"),
        seed,
        PayloadContext::default(),
    )
    .await
    .expect("the random page succeeds");
    let head = random_songs(
        &library,
        Some(&user),
        &request("10"),
        seed,
        PayloadContext::default(),
    )
    .await
    .expect("the random page succeeds");
    let elsewhere = random_songs(
        &library,
        Some(&user),
        &request("40"),
        other,
        PayloadContext::default(),
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

    let directory = music_directory(&library, &artist_id, None, PayloadContext::default())
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
    let directory = music_directory(&library, &album_id, None, PayloadContext::default())
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
        PayloadContext::default(),
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
        music_directory(
            &library,
            &Uuid::now_v7().to_string(),
            None,
            PayloadContext::default()
        )
        .await,
        Err(BrowseError::AlbumNotFound)
    ));
    assert!(matches!(
        music_directory(&library, "not-a-uuid", None, PayloadContext::default()).await,
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
    let result = enumerate(&library, None, &request, PayloadContext::default())
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
    let result = enumerate(&library, None, &only_songs, PayloadContext::default())
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
    let completion = disabled_completion(&repository);
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
        PayloadContext::default(),
    )
    .await
    .expect("the album detail succeeds");

    assert_eq!(detail.name, summary.album().title());
    assert_eq!(detail.song_count, summary.track_count() as i32);
    assert_eq!(detail.duration, (summary.duration_ms() / 1000) as i32);
    assert_eq!(detail.song.len(), summary.track_count() as usize);

    let detail = artist_detail(&completion, &artist.artist().id().to_string(), None)
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
            PayloadContext::default()
        )
        .await,
        Err(BrowseError::AlbumNotFound)
    ));
    assert!(matches!(
        artist_detail(&completion, &Uuid::now_v7().to_string(), None).await,
        Err(BrowseError::ArtistNotFound)
    ));
}

struct ReportingFixture {
    activity: Arc<PlaybackActivityService>,
    library: Arc<LibraryService>,
    credential: Arc<CredentialService>,
    principal: Principal,
    track: TrackId,
    pool: sqlx::PgPool,
}

fn reporting_key() -> CredentialKey {
    CredentialKey::from_base64("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA").expect("valid key")
}

async fn reporting_fixture(pool: sqlx::PgPool, duration_ms: Option<u64>) -> ReportingFixture {
    let repository = Arc::new(PostgresTrackRepository::new(pool.clone()));
    let track = repository
        .get_or_create(
            SourceIdentity::new(
                ProviderId::new("local".to_owned()).unwrap(),
                SourceScope::Named("/music".to_owned()),
                "reporting.flac".to_owned(),
            )
            .unwrap(),
            TrackMetadata::from_names(
                "Reporting".to_owned(),
                vec!["Daft Punk".to_owned()],
                duration_ms,
            )
            .unwrap(),
        )
        .await
        .unwrap()
        .id()
        .clone();

    let library = Arc::new(LibraryService::new(
        Arc::clone(&repository) as Arc<dyn LibraryRead>,
        Arc::clone(&repository) as Arc<dyn SourceLookup>,
        Arc::clone(&repository) as Arc<dyn StateStore>,
    ));
    let listens = Arc::new(ListenService::new(
        Arc::clone(&repository) as Arc<dyn ListenStore>,
        Arc::clone(&repository) as Arc<dyn LibraryRead>,
    ));
    let activity = Arc::new(PlaybackActivityService::new(listens, Arc::clone(&library)));
    let user = Uuid::now_v7();

    sqlx::query("INSERT INTO users (id, username, display_name) VALUES ($1, $2, $3)")
        .bind(user)
        .bind("flynn")
        .bind("Flynn")
        .execute(&pool)
        .await
        .unwrap();

    ReportingFixture {
        activity,
        library,
        credential: Arc::new(crate::features::identity::credentials(
            pool.clone(),
            reporting_key(),
            crate::features::identity::LoginThrottleConfig::default(),
        )),
        principal: Principal {
            user_id: UserId::from_uuid(user),
            role: UserRole::Member,
        },
        track,
        pool,
    }
}

fn at(millis: i64) -> DateTime<Utc> {
    DateTime::from_timestamp_millis(millis).expect("a representable instant")
}

async fn counted_plays(pool: &sqlx::PgPool, track: &TrackId) -> i64 {
    sqlx::query_scalar(
        "SELECT COALESCE(SUM(play_count), 0)::BIGINT FROM track_play_stats WHERE track_id = $1",
    )
    .bind(track.as_uuid())
    .fetch_one(pool)
    .await
    .unwrap()
}

async fn recorded_listens(
    pool: &sqlx::PgPool,
) -> Vec<(String, Option<i64>, Option<i64>, Option<bool>)> {
    sqlx::query_as(
        "SELECT origin, listened_ms, start_position_ms, qualified FROM listens \
         ORDER BY listen_at, origin",
    )
    .fetch_all(pool)
    .await
    .unwrap()
}

fn report_request(
    track: &TrackId,
    state: &str,
    position_ms: u64,
    extra: &str,
) -> PlaybackReportRequest {
    PlaybackReportRequest::parse(&RawQuery::parse(&format!(
        "mediaId={track}&mediaType=song&positionMs={position_ms}&state={state}{extra}"
    )))
    .expect("a valid report")
}

#[sqlx::test]
async fn a_submission_records_one_qualified_play_and_moves_the_counter(pool: sqlx::PgPool) {
    let fixture = reporting_fixture(pool.clone(), Some(240_000)).await;
    let ids = vec![fixture.track.to_string()];

    scrobble(
        &fixture.activity,
        fixture.principal.user_id.as_uuid(),
        "phone",
        &ids,
        &[],
        true,
        at(0),
    )
    .await
    .unwrap();

    assert_eq!(
        recorded_listens(&fixture.pool).await,
        [("subsonic_scrobble".to_owned(), None, None, None)],
        "a legacy submission carries no extent and is accepted unconditionally"
    );
    assert_eq!(counted_plays(&fixture.pool, &fixture.track).await, 1);
}

#[sqlx::test]
async fn a_scrobble_batch_records_every_entry(pool: sqlx::PgPool) {
    let fixture = reporting_fixture(pool.clone(), Some(240_000)).await;
    let second = fixture.track.to_string();
    let ids = vec![fixture.track.to_string(), second.clone()];
    let times = vec!["1000".to_owned(), "2000".to_owned()];

    scrobble(
        &fixture.activity,
        fixture.principal.user_id.as_uuid(),
        "phone",
        &ids,
        &times,
        true,
        at(0),
    )
    .await
    .unwrap();

    assert_eq!(recorded_listens(&fixture.pool).await.len(), 2);
    assert_eq!(counted_plays(&fixture.pool, &fixture.track).await, 2);
}

#[sqlx::test]
async fn a_now_playing_notification_records_nothing_and_shows_the_session(pool: sqlx::PgPool) {
    let fixture = reporting_fixture(pool.clone(), Some(240_000)).await;
    let ids = vec![fixture.track.to_string()];

    scrobble(
        &fixture.activity,
        fixture.principal.user_id.as_uuid(),
        "phone",
        &ids,
        &[],
        false,
        at(0),
    )
    .await
    .unwrap();

    assert!(recorded_listens(&fixture.pool).await.is_empty());
    assert_eq!(counted_plays(&fixture.pool, &fixture.track).await, 0);

    let live = fixture
        .activity
        .now_playing(fixture.principal.user_id.as_uuid(), at(0));
    assert_eq!(live.len(), 1);
    assert_eq!(live[0].track_id(), &fixture.track);
    assert_eq!(live[0].state(), PlaybackState::Playing);
    assert_eq!(live[0].client().as_str(), "phone");

    let entries = now_playing(
        &fixture.activity,
        &fixture.library,
        &fixture.credential,
        &fixture.principal,
        PayloadContext::default(),
        at(0),
    )
    .await
    .unwrap();

    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].id, fixture.track.to_string());
    assert_eq!(entries[0].title, "Reporting");
    assert_eq!(entries[0].username, "flynn");
    assert_eq!(entries[0].player_name.as_deref(), Some("phone"));
    assert_eq!(
        entries[0].state,
        Some(models::now_playing_entry::State::Playing)
    );
    assert_eq!(entries[0].minutes_ago, 0);
}

#[sqlx::test]
async fn a_report_sequence_crossing_the_threshold_records_exactly_one_play(pool: sqlx::PgPool) {
    let fixture = reporting_fixture(pool.clone(), Some(2_000)).await;
    let user = fixture.principal.user_id.as_uuid();

    for (state, position_ms, millis) in [
        ("starting", 0_u64, 0_i64),
        ("playing", 0, 0),
        ("paused", 600, 600),
        ("playing", 600, 600),
        ("stopped", 1_200, 1_200),
    ] {
        report_playback(
            &fixture.activity,
            user,
            "phone",
            &report_request(&fixture.track, state, position_ms, ""),
            at(millis),
        )
        .await
        .unwrap();
    }

    assert_eq!(
        recorded_listens(&fixture.pool).await,
        [(
            "report_playback".to_owned(),
            Some(1_200),
            Some(0),
            Some(true)
        )],
        "the session's accumulated listening is the extent of its one play"
    );
    assert_eq!(counted_plays(&fixture.pool, &fixture.track).await, 1);
}

#[sqlx::test]
async fn a_report_sequence_below_the_threshold_is_present_and_uncounted(pool: sqlx::PgPool) {
    let fixture = reporting_fixture(pool.clone(), Some(2_000)).await;
    let user = fixture.principal.user_id.as_uuid();

    for (state, position_ms, millis) in [
        ("starting", 0_u64, 0_i64),
        ("playing", 0, 0),
        ("paused", 300, 300),
        ("playing", 300, 300),
    ] {
        report_playback(
            &fixture.activity,
            user,
            "phone",
            &report_request(&fixture.track, state, position_ms, ""),
            at(millis),
        )
        .await
        .unwrap();

        let live = fixture.activity.now_playing(user, at(millis));

        assert_eq!(live.len(), 1);
        assert_eq!(live[0].listened_ms(), u64::try_from(millis).unwrap());
    }

    report_playback(
        &fixture.activity,
        user,
        "phone",
        &report_request(&fixture.track, "stopped", 600, ""),
        at(600),
    )
    .await
    .unwrap();

    assert!(recorded_listens(&fixture.pool).await.is_empty());
    assert_eq!(counted_plays(&fixture.pool, &fixture.track).await, 0);
    assert!(
        fixture.activity.now_playing(user, at(600)).is_empty(),
        "a stopped session is not live"
    );
}

#[sqlx::test]
async fn an_ignored_scrobble_records_nothing_across_the_sequence(pool: sqlx::PgPool) {
    let fixture = reporting_fixture(pool.clone(), Some(2_000)).await;
    let user = fixture.principal.user_id.as_uuid();

    for (state, position_ms, millis) in [
        ("starting", 0_u64, 0_i64),
        ("playing", 0, 0),
        ("paused", 600, 600),
        ("playing", 600, 600),
        ("stopped", 1_200, 1_200),
    ] {
        report_playback(
            &fixture.activity,
            user,
            "phone",
            &report_request(&fixture.track, state, position_ms, "&ignoreScrobble=true"),
            at(millis),
        )
        .await
        .unwrap();

        let live = fixture.activity.now_playing(user, at(millis));

        if state == "stopped" {
            assert!(live.is_empty(), "a stopped session is not live");
        } else {
            assert_eq!(
                live.len(),
                1,
                "an ignored sequence is still live now-playing state"
            );
        }
    }

    assert!(recorded_listens(&fixture.pool).await.is_empty());
    assert_eq!(counted_plays(&fixture.pool, &fixture.track).await, 0);
}

#[sqlx::test]
async fn a_repeated_scrobble_of_the_same_play_counts_once(pool: sqlx::PgPool) {
    let fixture = reporting_fixture(pool.clone(), Some(240_000)).await;
    let user = fixture.principal.user_id.as_uuid();
    let ids = vec![fixture.track.to_string()];
    let times = vec!["1000".to_owned()];

    for _ in 0..2 {
        scrobble(&fixture.activity, user, "phone", &ids, &times, true, at(0))
            .await
            .unwrap();
    }

    assert_eq!(recorded_listens(&fixture.pool).await.len(), 1);
    assert_eq!(counted_plays(&fixture.pool, &fixture.track).await, 1);

    scrobble(
        &fixture.activity,
        user,
        "phone",
        &ids,
        &["1001".to_owned()],
        true,
        at(0),
    )
    .await
    .unwrap();

    assert_eq!(
        recorded_listens(&fixture.pool).await.len(),
        2,
        "a distinct timestamp is a distinct listen"
    );
    assert_eq!(counted_plays(&fixture.pool, &fixture.track).await, 2);
}

#[sqlx::test]
async fn an_unknown_track_is_not_found(pool: sqlx::PgPool) {
    let fixture = reporting_fixture(pool.clone(), Some(240_000)).await;
    let user = fixture.principal.user_id.as_uuid();

    assert!(matches!(
        scrobble(
            &fixture.activity,
            user,
            "phone",
            &[Uuid::now_v7().to_string()],
            &[],
            true,
            at(0),
        )
        .await,
        Err(ReportingError::NotFound)
    ));
    assert!(matches!(
        report_playback(
            &fixture.activity,
            user,
            "phone",
            &PlaybackReportRequest {
                media_id: TrackId::from_uuid(Uuid::now_v7()),
                state: PlaybackState::Playing,
                position_ms: 0,
                playback_rate: PlaybackRate::default(),
                ignore_scrobble: false,
            },
            at(0),
        )
        .await,
        Err(ReportingError::NotFound)
    ));
    assert!(recorded_listens(&fixture.pool).await.is_empty());
}

#[test]
fn scrobble_rejects_an_invalid_batch_before_any_write() {
    let track = Uuid::now_v7().to_string();

    assert!(matches!(
        scrobble_entries(&[], &[], at(0)),
        Err(ReportingError::MissingParameter)
    ));
    assert!(matches!(
        scrobble_entries(&[track.clone(), track.clone()], &["1".to_owned()], at(0)),
        Err(ReportingError::InvalidRequest)
    ));
    assert!(matches!(
        scrobble_entries(&["not-a-track".to_owned()], &[], at(0)),
        Err(ReportingError::InvalidRequest)
    ));
    assert!(matches!(
        scrobble_entries(std::slice::from_ref(&track), &["later".to_owned()], at(0)),
        Err(ReportingError::InvalidRequest)
    ));
    assert!(matches!(
        scrobble_entries(std::slice::from_ref(&track), &["-1".to_owned()], at(0)),
        Err(ReportingError::InvalidRequest)
    ));
    assert_eq!(
        scrobble_entries(&[track], &["1000".to_owned()], at(0))
            .unwrap()
            .into_iter()
            .map(|entry| entry.listened_at)
            .collect::<Vec<_>>(),
        [at(1_000)]
    );
}

#[test]
fn scrobble_submission_defaults_true_and_rejects_an_unknown_value() {
    assert!(scrobble_submission(&RawQuery::parse("id=x")).unwrap());
    assert!(scrobble_submission(&RawQuery::parse("submission=true")).unwrap());
    assert!(scrobble_submission(&RawQuery::parse("submission=1")).unwrap());
    assert!(!scrobble_submission(&RawQuery::parse("submission=false")).unwrap());
    assert!(!scrobble_submission(&RawQuery::parse("submission=0")).unwrap());
    assert!(matches!(
        scrobble_submission(&RawQuery::parse("submission=maybe")),
        Err(ReportingError::InvalidRequest)
    ));
}

#[test]
fn report_playback_rejects_unknown_parameters_states_and_values() {
    let track = Uuid::now_v7().to_string();
    let valid = format!("mediaId={track}&mediaType=song&positionMs=0&state=playing");

    assert!(
        PlaybackReportRequest::parse(&RawQuery::parse(&format!("{valid}&position=5"))).is_err()
    );
    assert!(
        PlaybackReportRequest::parse(&RawQuery::parse(&format!(
            "mediaId={track}&mediaType=song&positionMs=0&state=stalled"
        )))
        .is_err()
    );
    assert!(
        PlaybackReportRequest::parse(&RawQuery::parse(&format!(
            "mediaId={track}&mediaType=podcast&positionMs=0&state=playing"
        )))
        .is_err()
    );
    assert!(
        PlaybackReportRequest::parse(&RawQuery::parse(&format!(
            "mediaId={track}&mediaType=song&positionMs=-1&state=playing"
        )))
        .is_err()
    );
    assert!(
        PlaybackReportRequest::parse(&RawQuery::parse(&format!(
            "mediaId={track}&mediaType=song&positionMs=0&state=playing&playbackRate=0"
        )))
        .is_err()
    );
    assert!(
        PlaybackReportRequest::parse(&RawQuery::parse(&format!(
            "mediaId={track}&mediaType=song&positionMs=0&state=playing&playbackRate=fast"
        )))
        .is_err()
    );
    assert!(
        PlaybackReportRequest::parse(&RawQuery::parse(&format!(
            "mediaId={track}&mediaType=song&positionMs=0&state=playing&ignoreScrobble=maybe"
        )))
        .is_err()
    );
    assert!(matches!(
        PlaybackReportRequest::parse(&RawQuery::parse(&format!(
            "mediaId={track}&mediaType=song&state=playing"
        ))),
        Err(ReportingError::MissingParameter)
    ));
    assert!(matches!(
        PlaybackReportRequest::parse(&RawQuery::parse(&format!(
            "mediaId={track}&positionMs=0&state=playing"
        ))),
        Err(ReportingError::MissingParameter)
    ));

    let request = PlaybackReportRequest::parse(&RawQuery::parse(&valid)).unwrap();

    assert_eq!(request.media_id.to_string(), track);
    assert_eq!(request.state, PlaybackState::Playing);
    assert_eq!(request.position_ms, 0);
    assert_eq!(request.playback_rate, PlaybackRate::default());
    assert!(!request.ignore_scrobble);
}

#[test]
fn artist_info_count_defaults_caps_and_rejects() {
    let over = (MAX_BROWSE_SIZE + 1).to_string();

    assert_eq!(artist_info_count(None).unwrap(), 20);
    assert_eq!(artist_info_count(Some("10")).unwrap(), 10);
    assert_eq!(artist_info_count(Some("0")).unwrap(), 0);
    assert_eq!(
        artist_info_count(Some(over.as_str())).unwrap(),
        MAX_BROWSE_SIZE
    );
    assert!(matches!(
        artist_info_count(Some("-1")),
        Err(BrowseError::InvalidRequest)
    ));
    assert!(matches!(
        artist_info_count(Some("many")),
        Err(BrowseError::InvalidRequest)
    ));
}

#[test]
fn include_not_present_defaults_false_and_rejects_an_unknown_value() {
    assert!(!include_not_present(None).unwrap());
    assert!(include_not_present(Some("true")).unwrap());
    assert!(include_not_present(Some("1")).unwrap());
    assert!(!include_not_present(Some("false")).unwrap());
    assert!(!include_not_present(Some("0")).unwrap());
    assert!(matches!(
        include_not_present(Some("maybe")),
        Err(BrowseError::InvalidRequest)
    ));
}

#[sqlx::test]
async fn subsonic_artist_info_reports_a_disabled_metadata_feature(pool: sqlx::PgPool) {
    let repository = Arc::new(PostgresTrackRepository::new(pool.clone()));
    let artist = repository
        .resolve_or_create_artist(
            &ArtistCredit::new("Daft Punk".to_owned(), Some("mbid-daft-punk".to_owned())).unwrap(),
        )
        .await
        .unwrap();
    let library = library_over(&repository);
    let discovery = discovery_over(&repository);

    let error = artist_info2(
        &library,
        Some(&discovery),
        &artist.id().to_string(),
        None,
        20,
    )
    .await
    .expect_err("no metadata feature is configured");

    assert!(matches!(
        error,
        BrowseError::Catalogue(CatalogueError::Provider(ProviderError::Disabled))
    ));
    assert!(
        artist_detail(
            &disabled_completion(&repository),
            &artist.id().to_string(),
            None,
        )
        .await
        .is_ok(),
        "the artist read stays usable without the metadata feature"
    );
    assert!(
        artists(&library, None).await.is_ok(),
        "the artist index stays usable without the metadata feature"
    );
}

#[sqlx::test]
async fn subsonic_artist_info_distinguishes_a_bad_id_from_an_unknown_one(pool: sqlx::PgPool) {
    let repository = Arc::new(PostgresTrackRepository::new(pool.clone()));
    let library = library_over(&repository);
    let discovery = discovery_over(&repository);

    assert!(matches!(
        artist_info2(&library, Some(&discovery), "not-a-uuid", None, 20).await,
        Err(BrowseError::ArtistNotFound)
    ));

    let unknown = ArtistId::new().to_string();

    assert!(matches!(
        artist_info2(&library, Some(&discovery), &unknown, None, 20).await,
        Err(BrowseError::ArtistNotFound)
    ));
    assert!(matches!(
        artist_info(&library, &unknown, None).await,
        Err(BrowseError::ArtistNotFound)
    ));
}

const MAX_METADATA_BYTES: u64 = 4 * 1024 * 1024;

const SEED_MBID: &str = "mbid-discovery-seed";
const PEER_MBID: &str = "mbid-discovery-peer";
const SECOND_PEER_MBID: &str = "mbid-discovery-second-peer";
const QUIET_MBID: &str = "mbid-discovery-quiet";
const EMPTY_MBID: &str = "mbid-discovery-empty";
const TWIN_ONE_MBID: &str = "mbid-discovery-twin-one";
const TWIN_TWO_MBID: &str = "mbid-discovery-twin-two";
const PROVIDER_ONLY_MBID: &str = "mbid-discovery-provider-only";

fn mbid_credit(name: &str, mbid: &str) -> ArtistCredit {
    ArtistCredit::new(name.to_owned(), Some(mbid.to_owned())).expect("a credit")
}

fn mbid_role(name: &str, mbid: &str, role: CreditRole) -> ArtistCredit {
    mbid_credit(name, mbid).with_role(role)
}

fn musicbrainz_identity(mbid: &str) -> SourceIdentity {
    SourceIdentity::new(
        ProviderId::new("musicbrainz".to_owned()).expect("a provider"),
        SourceScope::Global,
        mbid.to_owned(),
    )
    .expect("a source identity")
}

async fn named_artist(repository: &PostgresTrackRepository, name: &str, mbid: &str) -> ArtistId {
    let artist = repository
        .resolve_or_create_artist(&mbid_credit(name, mbid))
        .await
        .expect("an artist");
    repository
        .set_artist_source(artist.id(), &musicbrainz_identity(mbid))
        .await
        .expect("the artist source");

    artist.id().clone()
}

struct SeededDiscovery {
    seed: ArtistId,
    seed_album: crate::features::music_library::domain::AlbumId,
    peer: ArtistId,
    second_peer: ArtistId,
    quiet: ArtistId,
    empty: ArtistId,
    seed_solo: TrackId,
    seed_track: TrackId,
    bridge: TrackId,
    peer_track: TrackId,
    quiet_track: TrackId,
}

async fn seeded_discovery(repository: &PostgresTrackRepository) -> SeededDiscovery {
    let seed = named_artist(repository, "Seed Artist", SEED_MBID).await;
    let peer = named_artist(repository, "Peer Artist", PEER_MBID).await;
    let second_peer = named_artist(repository, "Second Peer", SECOND_PEER_MBID).await;
    let quiet = named_artist(repository, "Quiet Artist", QUIET_MBID).await;
    let empty = named_artist(repository, "Empty Artist", EMPTY_MBID).await;
    named_artist(repository, "Twin Artist", TWIN_ONE_MBID).await;
    named_artist(repository, "Twin Artist", TWIN_TWO_MBID).await;

    let seed_album = repository
        .resolve_or_create_album(
            &AlbumMetadata::new("Seed Album".to_owned())
                .expect("album metadata")
                .with_album_artist(mbid_credit("Seed Artist", SEED_MBID)),
        )
        .await
        .expect("the seed album");
    let peer_album = repository
        .resolve_or_create_album(
            &AlbumMetadata::new("Peer Album".to_owned())
                .expect("album metadata")
                .with_album_artist(mbid_credit("Peer Artist", PEER_MBID)),
        )
        .await
        .expect("the peer album");

    let seed_track = repository
        .get_or_create(
            local_source("discovery-seed.flac"),
            TrackMetadata::new(
                "Zeta Seed".to_owned(),
                vec![mbid_credit("Seed Artist", SEED_MBID)],
                Some(1_000),
            )
            .expect("track metadata")
            .with_credits(vec![
                mbid_role("Seed Artist", SEED_MBID, CreditRole::Composer),
                mbid_role("Peer Artist", PEER_MBID, CreditRole::Composer),
                mbid_role("Second Peer", SECOND_PEER_MBID, CreditRole::Composer),
            ]),
        )
        .await
        .expect("the seed track")
        .id()
        .clone();
    let seed_solo = repository
        .get_or_create(
            local_source("discovery-solo.flac"),
            TrackMetadata::new(
                "Alpha Solo".to_owned(),
                vec![mbid_credit("Seed Artist", SEED_MBID)],
                Some(2_000),
            )
            .expect("track metadata"),
        )
        .await
        .expect("the solo track")
        .id()
        .clone();
    let bridge = repository
        .get_or_create(
            local_source("discovery-bridge.flac"),
            TrackMetadata::new(
                "Mid Bridge".to_owned(),
                vec![mbid_credit("Seed Artist", SEED_MBID)],
                Some(3_000),
            )
            .expect("track metadata")
            .with_credits(vec![mbid_role(
                "Peer Artist",
                PEER_MBID,
                CreditRole::Composer,
            )]),
        )
        .await
        .expect("the bridge track")
        .id()
        .clone();
    let peer_track = repository
        .get_or_create(
            local_source("discovery-peer.flac"),
            TrackMetadata::new(
                "Peer Song".to_owned(),
                vec![mbid_credit("Peer Artist", PEER_MBID)],
                Some(4_000),
            )
            .expect("track metadata"),
        )
        .await
        .expect("the peer track")
        .id()
        .clone();
    let quiet_track = repository
        .get_or_create(
            local_source("discovery-quiet.flac"),
            TrackMetadata::new(
                "Quiet Song".to_owned(),
                vec![mbid_credit("Quiet Artist", QUIET_MBID)],
                Some(5_000),
            )
            .expect("track metadata")
            .with_credits(vec![mbid_role(
                "Quiet Artist",
                QUIET_MBID,
                CreditRole::Composer,
            )]),
        )
        .await
        .expect("the quiet track")
        .id()
        .clone();

    for (album, track, position) in [
        (seed_album.id(), &seed_track, 0u32),
        (seed_album.id(), &seed_solo, 1),
        (seed_album.id(), &bridge, 2),
        (peer_album.id(), &peer_track, 0),
    ] {
        repository
            .set_album_track(
                album,
                track,
                &AlbumTrack::new(Some(1), Some((position + 1) as u16), position)
                    .expect("a membership"),
            )
            .await
            .expect("the album membership");
    }

    SeededDiscovery {
        seed,
        seed_album: seed_album.id().clone(),
        peer,
        second_peer,
        quiet,
        empty,
        seed_solo,
        seed_track,
        bridge,
        peer_track,
        quiet_track,
    }
}

fn song_ids(songs: &models::TopSongs) -> Vec<String> {
    songs
        .song
        .as_ref()
        .expect("the declared song list")
        .iter()
        .map(|song| song.id.clone())
        .collect()
}

#[sqlx::test]
async fn top_songs_resolves_an_exact_name_and_serves_the_local_tracks_capped_at_count(
    pool: sqlx::PgPool,
) {
    let repository = Arc::new(PostgresTrackRepository::new(pool.clone()));
    let data = seeded_discovery(&repository).await;
    let library = library_over(&repository);
    let discovery = discovery_over(&repository);

    let songs = top_songs(
        &library,
        Some(&discovery),
        "Seed Artist",
        None,
        50,
        PayloadContext::default(),
    )
    .await
    .expect("the local top songs");
    let mut ids = song_ids(&songs);
    ids.sort();

    let mut expected = vec![
        data.seed_solo.to_string(),
        data.bridge.to_string(),
        data.seed_track.to_string(),
    ];
    expected.sort();
    assert_eq!(ids, expected, "the local relationship basis");

    let capped = top_songs(
        &library,
        Some(&discovery),
        "Seed Artist",
        None,
        1,
        PayloadContext::default(),
    )
    .await
    .expect("the capped local top songs");
    let capped_ids = song_ids(&capped);

    assert_eq!(capped_ids.len(), 1, "the cap is honoured");
    assert!(expected.contains(&capped_ids[0]));
}

#[sqlx::test]
async fn top_songs_reports_not_found_for_zero_and_for_several_exact_matches(pool: sqlx::PgPool) {
    let repository = Arc::new(PostgresTrackRepository::new(pool.clone()));
    let data = seeded_discovery(&repository).await;
    let library = library_over(&repository);
    let discovery = discovery_over(&repository);

    assert_eq!(
        library
            .artist_by_exact_name("Seed Artist")
            .await
            .expect("the exact read"),
        Some(data.seed.clone())
    );
    assert_eq!(
        library
            .artist_by_exact_name("Twin Artist")
            .await
            .expect("the exact read"),
        None,
        "several canonical artists share the name, so no artist is picked"
    );
    assert!(matches!(
        top_songs(
            &library,
            Some(&discovery),
            "Nobody",
            None,
            50,
            PayloadContext::default()
        )
        .await,
        Err(BrowseError::ArtistNotFound)
    ));
    assert!(matches!(
        top_songs(
            &library,
            Some(&discovery),
            "Twin Artist",
            None,
            50,
            PayloadContext::default()
        )
        .await,
        Err(BrowseError::ArtistNotFound)
    ));
}

#[sqlx::test]
async fn similar_songs_exclude_the_seed_and_honour_count(pool: sqlx::PgPool) {
    let repository = Arc::new(PostgresTrackRepository::new(pool.clone()));
    let data = seeded_discovery(&repository).await;
    let library = library_over(&repository);
    let discovery = discovery_over(&repository);

    let songs = similar_songs(
        &library,
        Some(&discovery),
        &data.seed_track.to_string(),
        None,
        50,
        PayloadContext::default(),
    )
    .await
    .expect("the similar songs");
    let mut ids = song_ids_from(&songs);
    ids.sort();

    let mut expected = vec![data.bridge.to_string(), data.peer_track.to_string()];
    expected.sort();
    assert_eq!(ids, expected);
    assert!(
        !ids.contains(&data.seed_track.to_string()),
        "the seed track is excluded even though a related artist is credited on it"
    );
}

fn song_ids_from(songs: &models::SimilarSongs) -> Vec<String> {
    song_ids_of(songs.song.as_ref())
}

fn song_ids_of(songs: Option<&Vec<models::Child>>) -> Vec<String> {
    songs
        .expect("the declared song list")
        .iter()
        .map(|song| song.id.clone())
        .collect()
}

#[sqlx::test]
async fn similar_songs2_serves_the_related_artists_tracks_in_rank_order(pool: sqlx::PgPool) {
    let repository = Arc::new(PostgresTrackRepository::new(pool.clone()));
    let data = seeded_discovery(&repository).await;
    let library = library_over(&repository);
    let discovery = discovery_over(&repository);

    let songs = similar_songs2(
        &library,
        Some(&discovery),
        &data.seed.to_string(),
        None,
        50,
        PayloadContext::default(),
    )
    .await
    .expect("the similar songs");
    let mut ids = song_ids_of(songs.song.as_ref());
    ids.sort();

    let mut expected = vec![
        data.bridge.to_string(),
        data.peer_track.to_string(),
        data.seed_track.to_string(),
    ];
    expected.sort();
    assert_eq!(ids, expected);

    let capped = similar_songs2(
        &library,
        Some(&discovery),
        &data.seed.to_string(),
        None,
        1,
        PayloadContext::default(),
    )
    .await
    .expect("the capped similar songs");

    assert_eq!(
        song_ids_of(capped.song.as_ref()).len(),
        1,
        "the cap is honoured without a second related artist's tracks"
    );
}

#[sqlx::test]
async fn similar_songs_by_album_id_seed_the_album_artist(pool: sqlx::PgPool) {
    let repository = Arc::new(PostgresTrackRepository::new(pool.clone()));
    let data = seeded_discovery(&repository).await;
    let library = library_over(&repository);
    let discovery = discovery_over(&repository);
    let songs = similar_songs(
        &library,
        Some(&discovery),
        &data.seed_album.to_string(),
        None,
        50,
        PayloadContext::default(),
    )
    .await
    .expect("album radio");
    let mut ids = song_ids_from(&songs);
    ids.sort();

    let mut expected = vec![
        data.bridge.to_string(),
        data.peer_track.to_string(),
        data.seed_track.to_string(),
    ];
    expected.sort();
    assert_eq!(ids, expected, "album radio is anchored on the album artist");
}

#[sqlx::test]
async fn similar_songs_and_top_songs_declare_an_empty_list_when_nothing_is_available(
    pool: sqlx::PgPool,
) {
    let repository = Arc::new(PostgresTrackRepository::new(pool.clone()));
    let data = seeded_discovery(&repository).await;
    let library = library_over(&repository);
    let discovery = discovery_over(&repository);

    let songs = similar_songs(
        &library,
        Some(&discovery),
        &data.quiet_track.to_string(),
        None,
        50,
        PayloadContext::default(),
    )
    .await
    .expect("the declared absence");

    assert_eq!(songs.song, Some(Vec::new()));

    let songs = similar_songs2(
        &library,
        Some(&discovery),
        &data.quiet.to_string(),
        None,
        50,
        PayloadContext::default(),
    )
    .await
    .expect("the declared absence");

    assert_eq!(songs.song, Some(Vec::new()));

    let songs = top_songs(
        &library,
        Some(&discovery),
        "Empty Artist",
        None,
        50,
        PayloadContext::default(),
    )
    .await
    .expect("the declared absence");

    assert_eq!(songs.song, Some(Vec::new()));
}

#[sqlx::test]
async fn similar_artists_serve_only_canonical_matches_capped_at_count(pool: sqlx::PgPool) {
    let repository = Arc::new(PostgresTrackRepository::new(pool.clone()));
    let data = seeded_discovery(&repository).await;
    let library = library_over(&repository);
    let discovery = discovery_over(&repository);

    let artists = similar_artists(&library, &discovery, &data.seed, None, 20)
        .await
        .expect("the related artists");

    let ids = artists
        .iter()
        .map(|artist| artist.id.clone())
        .collect::<Vec<_>>();
    assert_eq!(
        ids,
        vec![data.peer.to_string(), data.second_peer.to_string()]
    );
    assert_eq!(artists[0].name, "Peer Artist");
    assert_eq!(artists[0].album_count, Some(1));
    assert_eq!(artists[0].cover_art, Some(format!("ar-{}", data.peer)));
    assert_eq!(artists[0].starred, None);
    assert_eq!(artists[1].name, "Second Peer");

    let capped = similar_artists(&library, &discovery, &data.seed, None, 1)
        .await
        .expect("the capped related artists");

    assert_eq!(
        capped
            .iter()
            .map(|artist| artist.id.clone())
            .collect::<Vec<_>>(),
        vec![data.peer.to_string()]
    );
}

#[sqlx::test]
async fn similar_artists_and_top_songs_report_not_found_for_an_unmatched_name_or_id(
    pool: sqlx::PgPool,
) {
    let repository = Arc::new(PostgresTrackRepository::new(pool.clone()));
    let data = seeded_discovery(&repository).await;
    let library = library_over(&repository);
    let discovery = discovery_over(&repository);

    assert!(matches!(
        top_songs(
            &library,
            Some(&discovery),
            "Nobody",
            None,
            50,
            PayloadContext::default()
        )
        .await,
        Err(BrowseError::ArtistNotFound)
    ));
    assert!(matches!(
        top_songs(
            &library,
            Some(&discovery),
            "Twin Artist",
            None,
            50,
            PayloadContext::default()
        )
        .await,
        Err(BrowseError::ArtistNotFound)
    ));
    assert!(matches!(
        similar_songs2(
            &library,
            Some(&discovery),
            &ArtistId::new().to_string(),
            None,
            50,
            PayloadContext::default()
        )
        .await,
        Err(BrowseError::ArtistNotFound)
    ));
    assert!(matches!(
        similar_songs2(
            &library,
            Some(&discovery),
            "not-a-uuid",
            None,
            50,
            PayloadContext::default()
        )
        .await,
        Err(BrowseError::ArtistNotFound)
    ));
    assert!(
        similar_artists(&library, &discovery, &data.empty, None, 20)
            .await
            .expect("a served absence")
            .is_empty()
    );
}

struct FixedCapabilities {
    providers: Vec<String>,
}

#[async_trait]
impl CapabilitySource for FixedCapabilities {
    async fn fetch(&self) -> Result<Snapshot, CapabilityError> {
        Ok(Snapshot::new(self.providers.clone()))
    }
}

struct NoDocuments;

#[async_trait]
impl MetadataDocuments for NoDocuments {
    async fn fetch(
        &self,
        _source: &SourceIdentity,
        _profile: &MetadataProfile,
        _language: &str,
        _fields: &[FieldId],
    ) -> Result<DocumentOutcome, DocumentError> {
        Err(DocumentError::Unsupported)
    }
}

fn provider_write(field: FieldId, provider_id: &str, external_id: &str) -> FieldWrite {
    FieldWrite {
        field,
        source_provider_id: provider_id.to_owned(),
        source_external_id: format!("{provider_id}-{external_id}"),
        adapter_version: "v1".to_owned(),
        language: "en".to_owned(),
        value: Some(FieldValue::Candidates(vec![CandidateItem {
            name: "Provider Candidate".to_owned(),
            external_id: Some("ytm-channel-1".to_owned()),
            url: None,
            rank: Some(1),
            score: Some(0.9),
            artists: vec!["Provider Only".to_owned()],
            release_name: None,
            duration_ms: None,
            isrc: None,
            recording_id: None,
            ranking_scope: Some("ytm.similar.v1".to_owned()),
        }])),
        licence_id: Some(provider_id.to_owned()),
        licence_revision: Some(1),
        attribution: None,
        source_url: None,
        match_basis: MatchBasis::Identifier,
        operation: "artist.discovery.v1".to_owned(),
        terms_basis: None,
        uncertainty: false,
        image: None,
        metric: None,
        rank: Some(1),
        score: Some(0.9),
        fetched_at: Utc::now(),
    }
}

fn discovery_field_order() -> FieldOrder {
    FieldOrder::from_entries([
        (FieldId::Similar, vec![YOUTUBE_MUSIC.to_owned()]),
        (FieldId::TopTracks, vec![YOUTUBE_MUSIC.to_owned()]),
    ])
}

#[sqlx::test]
async fn a_provider_only_ranking_is_not_served_through_the_subsonic_discovery_lists(
    pool: sqlx::PgPool,
) {
    let repository = Arc::new(PostgresTrackRepository::new(pool.clone()));
    let artist = named_artist(&repository, "Provider Only", PROVIDER_ONLY_MBID).await;
    let library = library_over(&repository);
    let store = Arc::new(PostgresMetadataStore::new(
        pool.clone(),
        Arc::clone(&repository) as Arc<dyn EnrichmentGuard>,
        MAX_METADATA_BYTES,
    ));
    let snapshot = Arc::new(CapabilitySnapshot::new(
        Arc::new(FixedCapabilities {
            providers: vec![YOUTUBE_MUSIC.to_owned()],
        }),
        CAPABILITY_TTL,
    ));
    snapshot
        .current(Utc::now())
        .await
        .expect("the capability snapshot warms");
    let subject = library
        .enrichment_subject(&artist)
        .await
        .expect("the enrichment subject")
        .expect("the canonical artist");
    let external_id = subject
        .musicbrainz_artist_id()
        .unwrap_or_default()
        .to_owned();
    store
        .commit_field(
            &subject,
            &provider_write(FieldId::Similar, YOUTUBE_MUSIC, &external_id),
        )
        .await
        .expect("the provider fact");
    let metadata = Arc::new(MetadataService::new(
        pool.clone(),
        Arc::clone(&store) as Arc<dyn MetadataStore>,
        Arc::new(NoDocuments) as Arc<dyn MetadataDocuments>,
        Arc::clone(&snapshot),
        discovery_field_order(),
        "v1".to_owned(),
        Arc::new(NoJitter),
    ));
    let discovery = discovery_over(&repository).with_metadata(Arc::clone(&metadata));

    let external = discovery
        .related(RelatedRequest {
            seed: DiscoverySeed::Artist(artist.clone()),
            basis: DiscoveryBasis::External,
            window: None,
            limit: PageLimit::new(20).expect("the page limit"),
            user: None,
        })
        .await
        .expect("the provider basis");
    assert!(
        !external.items.is_empty(),
        "the provider ranking is available on the native external basis"
    );
    assert!(
        external.items.iter().all(|item| item.canonical.is_none()),
        "no provider candidate is a verified match"
    );

    let songs = similar_songs2(
        &library,
        Some(&discovery),
        &artist.to_string(),
        None,
        50,
        PayloadContext::default(),
    )
    .await
    .expect("the declared absence");

    assert_eq!(
        songs.song,
        Some(Vec::new()),
        "an unattributable provider ranking is not served through the facade"
    );
    assert!(
        similar_artists(&library, &discovery, &artist, None, 20)
            .await
            .expect("the declared absence")
            .is_empty()
    );
    assert_eq!(
        top_songs(
            &library,
            Some(&discovery),
            "Provider Only",
            None,
            50,
            PayloadContext::default()
        )
        .await
        .expect("the declared absence")
        .song,
        Some(Vec::new())
    );
}

async fn shared_composition_catalogue(
    pool: sqlx::PgPool,
) -> (
    Arc<LibraryService>,
    Arc<DiscoveryService>,
    Arc<CountingLibrary>,
    SeededDiscovery,
) {
    let repository = Arc::new(PostgresTrackRepository::new(pool.clone()));
    let data = seeded_discovery(&repository).await;
    let counting = Arc::new(CountingLibrary {
        inner: Arc::clone(&repository) as Arc<dyn LibraryRead>,
        track_calls: AtomicU32::new(0),
        album_calls: AtomicU32::new(0),
        artist_calls: AtomicU32::new(0),
        log: Mutex::new(Vec::new()),
    });
    let library = Arc::new(LibraryService::new(
        Arc::clone(&repository) as Arc<dyn LibraryRead>,
        Arc::clone(&repository) as Arc<dyn SourceLookup>,
        Arc::clone(&repository) as Arc<dyn StateStore>,
    ));
    let discovery = Arc::new(DiscoveryService::new(
        Arc::clone(&counting) as Arc<dyn LibraryRead>,
        Arc::clone(&repository) as Arc<dyn SourceLookup>,
        Arc::clone(&repository) as Arc<dyn StateStore>,
        Arc::clone(&repository) as Arc<dyn ListenStore>,
    ));

    (library, discovery, counting, data)
}

fn native_json<T, E>(result: Result<Json<T>, E>) -> serde_json::Value
where
    T: serde::Serialize,
    E: IntoResponse,
{
    match result {
        Ok(Json(body)) => serde_json::to_value(body).expect("the native body"),
        Err(_) => panic!("the native handler failed"),
    }
}

fn native_item_ids(body: &serde_json::Value, member: &str) -> Vec<String> {
    body["items"]
        .as_array()
        .expect("the item list")
        .iter()
        .map(|item| {
            item[member]
                .as_str()
                .unwrap_or_else(|| panic!("the `{member}` member of {item}"))
                .to_owned()
        })
        .collect()
}

#[sqlx::test]
async fn the_native_route_and_the_subsonic_facade_observe_the_same_composed_result(
    pool: sqlx::PgPool,
) {
    let (library, discovery, counting, data) = shared_composition_catalogue(pool.clone()).await;
    let state = ArtistInfoState::with_defaults(
        Arc::clone(&library),
        Arc::new(crate::features::identity::credentials(
            pool.clone(),
            reporting_key(),
            crate::features::identity::LoginThrottleConfig::default(),
        )),
        MetadataProfile::artist_display(),
        "en".to_owned(),
        Some(Arc::clone(&discovery)),
    );
    let user = Uuid::now_v7();
    let principal = Principal {
        user_id: UserId::from_uuid(user),
        role: UserRole::Member,
    };

    counting.clear_log();
    let native_top = native_json(
        top(
            State(state.clone()),
            Extension(context()),
            AuthenticatedPrincipal(principal.clone()),
            Path(data.seed.to_string()),
            RawQuery::parse(""),
        )
        .await,
    );
    let native_top_log = counting.composition_log();
    counting.clear_log();
    let facade_top = top_songs(
        &library,
        Some(&discovery),
        "Seed Artist",
        Some(&user),
        50,
        PayloadContext::default(),
    )
    .await
    .expect("the facade top songs");
    let facade_top_log = counting.composition_log();
    let mut facade_top_ids = song_ids(&facade_top);
    let mut native_top_ids = native_item_ids(&native_top, "track_id");
    facade_top_ids.sort();
    native_top_ids.sort();

    assert_eq!(
        native_top_ids, facade_top_ids,
        "the same artist must produce the same composed set through both adapters"
    );
    assert!(!native_top_ids.is_empty());
    assert_eq!(native_top_log, facade_top_log);
    assert!(!native_top_log.is_empty(), "the shared operation ran");

    counting.clear_log();
    let native_related = native_json(
        related(
            State(state),
            Extension(context()),
            AuthenticatedPrincipal(principal),
            Path(data.seed.to_string()),
            RawQuery::parse(""),
        )
        .await,
    );
    let native_related_log = counting.composition_log();
    counting.clear_log();
    let facade_related = similar_artists(&library, &discovery, &data.seed, Some(&user), 20)
        .await
        .expect("the facade related artists");
    let facade_related_log = counting.composition_log();

    assert_eq!(
        native_item_ids(&native_related, "id"),
        facade_related
            .iter()
            .map(|artist| artist.id.clone())
            .collect::<Vec<_>>(),
        "the same artist must produce the same composed order through both adapters"
    );
    assert_eq!(native_related_log, facade_related_log);
    assert!(!native_related_log.is_empty(), "the shared operation ran");
}
