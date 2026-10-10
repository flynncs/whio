use crate::support;

use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

use axum::{
    body::{Body, to_bytes},
    http::{HeaderMap, Request, StatusCode},
};
use serde_json::{Value, json};
use sqlx::PgPool;
use tower::ServiceExt;
use uuid::Uuid;

use whio_core::features::music_library::{
    catalogue::{CatalogueCandidate, ProviderError as CatalogueProviderError},
    domain::{
        AlbumId, AlbumMetadata, AlbumTrack, ArtistCredit, ProviderId, ProviderKind, SourceIdentity,
        SourceScope, StateTarget, TrackId, TrackMetadata,
    },
    library::{LibraryRegistration, LibraryRegistry},
    test_support::{
        CatalogueWrite, LibraryRead, PostgresLibraryStore, PostgresTrackRepository,
        SourceFileFacts, StateStore,
    },
};
use whio_core::{
    app::AppState,
    features::playback::providers::ProviderError,
    features::playback::{PlaybackProvider, PlaybackSource, ResolvedPlayback},
    platform::request::RequestContext,
};

use support::{
    StorePorts, credentials::database_credential_service, router, router_with_completion, services,
};

const API: &str = "/compat/navidrome/api/song";

struct Fixture {
    app: axum::Router,
    token: String,
    secret: String,
    user: Uuid,
    store: Arc<PostgresTrackRepository>,
}

struct CountingPlayback {
    calls: Arc<AtomicUsize>,
}

#[async_trait::async_trait]
impl PlaybackProvider for CountingPlayback {
    fn kind(&self) -> ProviderKind {
        ProviderKind::YoutubeMusic
    }

    async fn resolve(
        &self,
        _source: &PlaybackSource,
        _user: Option<&Uuid>,
        _context: &RequestContext,
    ) -> Result<Option<ResolvedPlayback>, ProviderError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(None)
    }
}

async fn fixture(
    pool: PgPool,
    responses: Vec<Result<Vec<CatalogueCandidate>, CatalogueProviderError>>,
) -> Fixture {
    let (credential, secret) = database_credential_service(pool.clone()).await;
    let (search, library, playback) = services(pool.clone(), responses);
    let app = router(credential, search, library, playback);
    let store = Arc::new(PostgresTrackRepository::new(pool));
    let login = post_login(app.clone(), "flynn", &secret).await;
    let token = login["token"].as_str().expect("token").to_owned();
    let user = login["id"]
        .as_str()
        .expect("user id")
        .parse()
        .expect("uuid");

    Fixture {
        app,
        token,
        secret,
        user,
        store,
    }
}

async fn api_fixture(pool: PgPool) -> Fixture {
    fixture(pool, Vec::new()).await
}

async fn post_login(app: axum::Router, username: &str, password: &str) -> Value {
    let request = Request::builder()
        .method("POST")
        .uri("/compat/navidrome/auth/login")
        .header("x-request-id", support::TEST_REQUEST_ID)
        .header("content-type", "application/json")
        .body(Body::from(
            json!({"username": username, "password": password}).to_string(),
        ))
        .unwrap();
    let response = app.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), 8192).await.unwrap();

    serde_json::from_slice(&body).unwrap()
}

async fn get(app: axum::Router, token: &str, query: &str) -> (StatusCode, HeaderMap, Value) {
    let request = Request::builder()
        .method("GET")
        .uri(format!("{API}{query}"))
        .header("x-request-id", support::TEST_REQUEST_ID)
        .header("x-nd-authorization", format!("Bearer {token}"))
        .body(Body::empty())
        .unwrap();
    let response = app.oneshot(request).await.unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let body = to_bytes(response.into_body(), 8 * 1024 * 1024)
        .await
        .unwrap();

    (status, headers, serde_json::from_slice(&body).unwrap())
}

async fn get_plain(app: axum::Router, uri: &str) -> Value {
    let request = Request::builder()
        .method("GET")
        .uri(uri)
        .header("x-request-id", support::TEST_REQUEST_ID)
        .body(Body::empty())
        .unwrap();
    let response = app.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), 8192).await.unwrap();

    serde_json::from_slice(&body).unwrap()
}

fn titles(body: &Value) -> Vec<String> {
    body.as_array()
        .expect("a song list")
        .iter()
        .map(|song| song["title"].as_str().expect("a title").to_owned())
        .collect()
}

fn total(headers: &HeaderMap) -> String {
    headers["x-total-count"]
        .to_str()
        .expect("a count")
        .to_owned()
}

fn local_source(external_id: &str) -> SourceIdentity {
    SourceIdentity::new(
        ProviderId::new("local".to_owned()).unwrap(),
        SourceScope::Named("/music".to_owned()),
        external_id.to_owned(),
    )
    .unwrap()
}

async fn add_track(store: &PostgresTrackRepository, external_id: &str, title: &str) -> TrackId {
    store
        .get_or_create(
            local_source(external_id),
            TrackMetadata::from_names(title.to_owned(), vec!["Artist".to_owned()], Some(1_000))
                .unwrap(),
        )
        .await
        .unwrap()
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
    let mut metadata = AlbumMetadata::new(title.to_owned()).unwrap();
    if let Some(name) = album_artist {
        metadata = metadata.with_album_artist(ArtistCredit::new(name.to_owned(), None).unwrap());
    }

    store
        .resolve_or_create_album(&metadata)
        .await
        .unwrap()
        .id()
        .clone()
}

fn selected_facts() -> SourceFileFacts {
    SourceFileFacts {
        path: Some("/music/song.ogg".to_owned()),
        size_bytes: 0,
        modified_at: None,
        missing_at: None,
        content_type: Some("audio/ogg".to_owned()),
        suffix: Some("ogg".to_owned()),
        container: Some("ogg".to_owned()),
        codec: Some("vorbis".to_owned()),
        bitrate_kbps: Some(900),
        sample_rate_hz: Some(48_000),
        bit_depth: Some(24),
        channels: Some(2),
        facts_version: 1,
        acoustid_fingerprint: None,
    }
}

#[sqlx::test]
async fn navidrome_song_surfaces_share_representation_facts(pool: PgPool) {
    let fixture = api_fixture(pool).await;
    let album = add_album(&fixture.store, "Discovery", Some("Daft Punk")).await;
    let source = local_source("song.ogg");
    let track = fixture
        .store
        .get_or_create(
            source.clone(),
            TrackMetadata::from_names(
                "Selected".to_owned(),
                vec!["Daft Punk".to_owned()],
                Some(200_000),
            )
            .unwrap(),
        )
        .await
        .unwrap();
    fixture
        .store
        .set_source_file_facts(&source, &selected_facts())
        .await
        .unwrap();
    fixture
        .store
        .set_album_track(
            &album,
            track.id(),
            &AlbumTrack::new(Some(1), Some(2), 0).unwrap(),
        )
        .await
        .unwrap();
    let id = track.id().to_string();
    let expected = [
        ("suffix", json!("ogg")),
        ("codec", json!("vorbis")),
        ("size", json!(0)),
        ("bitRate", json!(900)),
        ("sampleRate", json!(48_000)),
        ("bitDepth", json!(24)),
        ("channels", json!(2)),
    ];

    let (_, _, list) = get(fixture.app.clone(), &fixture.token, "?title=Selected").await;
    let (_, _, detail) = get(fixture.app.clone(), &fixture.token, &format!("/{id}")).await;
    let (_, _, contextual) = get(fixture.app, &fixture.token, &format!("?album_id={album}")).await;
    for song in [&list[0], &detail, &contextual[0]] {
        for (field, value) in &expected {
            assert_eq!(song[*field], *value, "{field}");
        }
    }
}

#[sqlx::test]
async fn navidrome_provider_only_song_omits_unknown_media_measurements(pool: PgPool) {
    let (credential, secret) = database_credential_service(pool.clone()).await;
    let ports = StorePorts::new(pool);
    let (search, library) = ports.bare_catalogue();
    let calls = Arc::new(AtomicUsize::new(0));
    let playback = ports.streaming(vec![Arc::new(CountingPlayback {
        calls: Arc::clone(&calls),
    }) as Arc<dyn PlaybackProvider>]);
    let app = router(credential, search, library, playback);
    let login = post_login(app.clone(), "flynn", &secret).await;
    let token = login["token"].as_str().unwrap().to_owned();
    let store = Arc::clone(&ports.store);
    let album = add_album(&store, "Discovery", Some("Daft Punk")).await;
    let source = SourceIdentity::new(
        ProviderId::new("youtube_music".to_owned()).unwrap(),
        SourceScope::Global,
        "provider-only".to_owned(),
    )
    .unwrap();
    let track = store
        .get_or_create(
            source,
            TrackMetadata::from_names(
                "Provider Only".to_owned(),
                vec!["Daft Punk".to_owned()],
                Some(200_000),
            )
            .unwrap(),
        )
        .await
        .unwrap();
    store
        .set_album_track(&album, track.id(), &AlbumTrack::unplaced())
        .await
        .unwrap();
    let id = track.id().to_string();

    let (_, _, detail) = get(app.clone(), &token, &format!("/{id}")).await;
    let (_, _, contextual) = get(app, &token, &format!("?album_id={album}")).await;
    for song in [&detail, &contextual[0]] {
        for field in [
            "suffix",
            "codec",
            "size",
            "bitRate",
            "sampleRate",
            "bitDepth",
            "channels",
        ] {
            assert!(song.get(field).is_none(), "{field} stays absent");
        }
        assert_eq!(song["title"], json!("Provider Only"));
        assert_eq!(song["id"], json!(id));
    }
    assert_eq!(
        calls.load(Ordering::SeqCst),
        0,
        "song reads do not resolve media"
    );
}

#[sqlx::test]
async fn navidrome_album_song_uses_requested_membership_and_art_target(pool: PgPool) {
    let fixture = api_fixture(pool).await;
    let earlier = add_album(&fixture.store, "Aardvark", Some("Earlier Artist")).await;
    let requested = add_album(&fixture.store, "Discovery", Some("Requested Artist")).await;
    let track = add_track(&fixture.store, "multi-album", "Shared Track").await;
    fixture
        .store
        .set_album_track(
            &earlier,
            &track,
            &AlbumTrack::new(Some(1), Some(1), 0).unwrap(),
        )
        .await
        .unwrap();
    fixture
        .store
        .set_album_track(
            &requested,
            &track,
            &AlbumTrack::new(Some(2), Some(7), 6).unwrap(),
        )
        .await
        .unwrap();

    let (_, _, songs) = get(
        fixture.app,
        &fixture.token,
        &format!("?album_id={requested}"),
    )
    .await;

    assert_eq!(songs.as_array().unwrap().len(), 1);
    assert_eq!(songs[0]["albumId"], json!(requested.to_string()));
    assert_eq!(songs[0]["albumArtist"], json!("Requested Artist"));
    assert_eq!(songs[0]["discNumber"], json!(2));
    assert_eq!(songs[0]["trackNumber"], json!(7));
    assert_eq!(songs[0]["coverArt"], json!(track.to_string()));
}

#[sqlx::test]
async fn songs_without_a_title_enumerate_the_library(pool: PgPool) {
    let fixture = api_fixture(pool).await;
    for (index, title) in ["Zebra", "Apple", "Mango"].iter().enumerate() {
        add_track(&fixture.store, &format!("path-{index}"), title).await;
    }

    let (status, headers, body) = get(fixture.app.clone(), &fixture.token, "").await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(titles(&body), ["Apple", "Mango", "Zebra"]);
    assert_eq!(total(&headers), "3");

    let (status, headers, body) = get(fixture.app, &fixture.token, "?title=").await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(titles(&body), ["Apple", "Mango", "Zebra"]);
    assert_eq!(total(&headers), "3");
}

#[sqlx::test]
async fn title_filter_matches_song_and_album_text(pool: PgPool) {
    let fixture = api_fixture(pool).await;
    let album = add_album(&fixture.store, "Glue Song", Some("Album Artist")).await;
    let titled = add_track(&fixture.store, "glue-title", "Glue Song").await;
    let album_member = add_track(&fixture.store, "glue-album", "Another Track").await;
    for track in [&titled, &album_member] {
        fixture
            .store
            .set_album_track(&album, track, &AlbumTrack::unplaced())
            .await
            .unwrap();
    }

    let (status, headers, body) = get(fixture.app, &fixture.token, "?title=glue").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(titles(&body), ["Another Track", "Glue Song"]);
    assert_eq!(total(&headers), "2");
}

#[sqlx::test]
async fn title_filter_matches_normalised_text(pool: PgPool) {
    let fixture = api_fixture(pool).await;
    for (index, title) in [
        "Instant Crush",
        "One More Time",
        "100% Pure",
        "100 Songs",
        "under_score",
        "underXscore",
    ]
    .iter()
    .enumerate()
    {
        add_track(&fixture.store, &format!("path-{index}"), title).await;
    }

    let (status, headers, body) = get(
        fixture.app.clone(),
        &fixture.token,
        "?title=%20%20INSTANT%20%20%20crush%20",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(titles(&body), ["Instant Crush"]);
    assert_eq!(total(&headers), "1");

    let (status, _, body) = get(fixture.app.clone(), &fixture.token, "?title=missing").await;
    assert_eq!(status, StatusCode::OK);
    assert!(titles(&body).is_empty());

    let (status, headers, body) = get(fixture.app.clone(), &fixture.token, "?title=%20%20").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(titles(&body).len(), 6);
    assert_eq!(total(&headers), "6");

    let (status, headers, body) = get(fixture.app.clone(), &fixture.token, "?title=100%25").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(titles(&body), ["100% Pure"]);
    assert_eq!(total(&headers), "1");

    let (status, headers, body) = get(fixture.app, &fixture.token, "?title=under_score").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(titles(&body), ["under_score"]);
    assert_eq!(total(&headers), "1");
}

#[sqlx::test]
async fn paging_past_25_is_honoured(pool: PgPool) {
    let fixture = api_fixture(pool).await;
    seed(&fixture.store, 520).await;

    let (status, headers, body) =
        get(fixture.app.clone(), &fixture.token, "?_start=0&_end=99").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body.as_array().unwrap().len(),
        99,
        "the react-admin bound is exclusive: max(0, _end - _start) rows"
    );
    assert_eq!(titles(&body)[0], "Track 0000");
    assert_eq!(titles(&body)[98], "Track 0098");
    assert_eq!(total(&headers), "520");

    let (status, headers, body) =
        get(fixture.app.clone(), &fixture.token, "?_start=0&_end=100000").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body.as_array().unwrap().len(),
        500,
        "the requested bound is capped at the page ceiling"
    );
    assert_eq!(total(&headers), "520");

    let (status, headers, body) =
        get(fixture.app.clone(), &fixture.token, "?_start=10&_end=9").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        titles(&body)[0],
        "Track 0010",
        "max(0, _end - _start) is zero, so the collection is unbounded from the offset"
    );
    assert_eq!(body.as_array().unwrap().len(), 500);
    assert_eq!(total(&headers), "520");

    let (status, _, body) = get(fixture.app, &fixture.token, "?_start=ten").await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body, json!({"error": "Invalid search parameters."}));
}

#[sqlx::test]
async fn total_is_the_page_total(pool: PgPool) {
    let fixture = api_fixture(pool).await;
    seed(&fixture.store, 5).await;

    let (status, headers, body) =
        get(fixture.app.clone(), &fixture.token, "?_start=1&_end=2").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        titles(&body),
        ["Track 0001"],
        "the exclusive bound leaves exactly one row for max(0, _end - _start)"
    );
    assert_eq!(total(&headers), "5");

    let (status, headers, body) = get(fixture.app, &fixture.token, "?_start=4&_end=4").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        titles(&body),
        ["Track 0004"],
        "max(0, _end - _start) is zero, so the page is unbounded and the fixture leaves one row from the offset"
    );
    assert_eq!(total(&headers), "5");
}

#[sqlx::test]
async fn deep_page_uses_the_shared_operation(pool: PgPool) {
    let fixture = api_fixture(pool).await;
    seed(&fixture.store, 1_003).await;

    let (status, headers, body) = get(
        fixture.app.clone(),
        &fixture.token,
        "?_start=1000&_end=1024",
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(titles(&body), ["Track 1000", "Track 1001", "Track 1002"]);
    assert_eq!(total(&headers), "1003");
}

#[sqlx::test]
async fn album_id_keeps_the_collection_read(pool: PgPool) {
    let fixture = api_fixture(pool).await;
    let album = add_album(&fixture.store, "Discovery", Some("Daft Punk")).await;
    let second = add_track(&fixture.store, "path-b", "Second").await;
    let first = add_track(&fixture.store, "path-a", "First").await;
    fixture
        .store
        .set_album_track(
            &album,
            &second,
            &AlbumTrack::new(Some(1), Some(2), 1).unwrap(),
        )
        .await
        .unwrap();
    fixture
        .store
        .set_album_track(
            &album,
            &first,
            &AlbumTrack::new(Some(1), Some(1), 0).unwrap(),
        )
        .await
        .unwrap();

    let (status, headers, body) = get(
        fixture.app,
        &fixture.token,
        &format!("?album_id={album}&_start=0&_end=0"),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(titles(&body), ["First", "Second"]);
    assert_eq!(total(&headers), "2");
    assert_eq!(body[0]["trackNumber"], json!(1));
    assert_eq!(body[0]["albumId"], json!(album.to_string()));
}

#[sqlx::test]
async fn song_query_rejects_unknown_selectors_malformed_values_and_duplicate_scalars(pool: PgPool) {
    let fixture = api_fixture(pool).await;
    add_track(&fixture.store, "path-a", "First").await;

    for query in [
        "?unknown=1",
        "?title[]=First",
        "?title[0]=First",
        "?_sort=bpm",
        "?_sort=comment",
        "?_sort=explicitStatus",
        "?_sort=unknown",
        "?album_id=",
        "?album_id=not-a-uuid",
        "?album_artist_id=not-a-uuid",
        "?artists_id=not-a-uuid",
        "?genre_id=",
        "?library_id=invalid",
        "?year=not-a-year",
        "?starred=1",
        "?has_rating=yes",
        "?missing=yes",
        "?compilation=yes",
        "?recently_played=yes",
        "?role=unknown",
        "?role=&role=artist",
        "?_start=0&_start=1",
        "?_end=1&_end=2",
        "?_sort=title&_sort=album",
        "?_order=asc&_order=desc",
        "?title=First&title=First",
        "?year=2000&year=2001",
        "?starred=true&starred=false",
        "?has_rating=true&has_rating=false",
        "?missing=true&missing=false",
        "?compilation=true&compilation=false",
        "?recently_played=true&recently_played=false",
    ] {
        let (status, _, body) = get(fixture.app.clone(), &fixture.token, query).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{query}");
        assert_eq!(
            body,
            json!({"error": "Invalid search parameters."}),
            "{query}"
        );
    }
}

#[sqlx::test]
async fn song_list_applies_registered_library_selector(pool: PgPool) {
    let (credential, secret) = database_credential_service(pool.clone()).await;
    let (search, library, playback) = services(pool.clone(), Vec::new());
    let registry = Arc::new(LibraryRegistry::new(Arc::new(PostgresLibraryStore::new(
        pool.clone(),
    ))));
    registry
        .register(&[
            LibraryRegistration::new("music".to_owned(), vec![PathBuf::from("/music")]),
            LibraryRegistration::new("elsewhere".to_owned(), vec![PathBuf::from("/elsewhere")]),
        ])
        .await
        .unwrap();
    let app = whio_core::app::router(
        AppState::new(credential, search, library, playback).with_library_registry(registry),
    );
    let login = post_login(app.clone(), "flynn", &secret).await;
    let token = login["token"].as_str().unwrap().to_owned();
    let store = PostgresTrackRepository::new(pool);
    for (root, external_id, title) in [
        ("/music", "main", "Main Track"),
        ("/elsewhere", "other", "Other Track"),
    ] {
        let source = SourceIdentity::new(
            ProviderId::new("local".to_owned()).unwrap(),
            SourceScope::Named(root.to_owned()),
            external_id.to_owned(),
        )
        .unwrap();
        store
            .get_or_create(
                source,
                TrackMetadata::from_names(title.to_owned(), vec!["Artist".to_owned()], Some(1_000))
                    .unwrap(),
            )
            .await
            .unwrap();
    }

    for (query, expected) in [
        ("?library_id=1", "Main Track"),
        ("?library_id=2", "Other Track"),
    ] {
        let (status, headers, body) = get(app.clone(), &token, query).await;
        assert_eq!(status, StatusCode::OK, "{query}");
        assert_eq!(total(&headers), "1", "{query}");
        assert_eq!(titles(&body), [expected], "{query}");
    }
}

#[sqlx::test]
async fn invalid_song_queries_do_not_complete_provider_albums(pool: PgPool) {
    use whio_core::features::music_library::{
        availability::ProviderRegistry,
        catalogue::{
            CatalogueProvider, CatalogueSearch, CompletionService, FetchedAlbum, LibraryService,
            SearchService,
        },
    };

    struct CountingAlbumProvider {
        calls: Arc<AtomicUsize>,
    }

    #[async_trait::async_trait]
    impl CatalogueProvider for CountingAlbumProvider {
        async fn search_tracks(
            &self,
            _search: &CatalogueSearch,
            _context: &RequestContext,
        ) -> Result<Vec<CatalogueCandidate>, CatalogueProviderError> {
            Ok(Vec::new())
        }

        async fn fetch_album(
            &self,
            _source: &SourceIdentity,
            _context: &RequestContext,
        ) -> Result<FetchedAlbum, CatalogueProviderError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(FetchedAlbum::new(Vec::new(), Some(2)))
        }
    }

    let (credential, secret) = database_credential_service(pool.clone()).await;
    let ports = StorePorts::new(pool.clone());
    let library = Arc::new(LibraryService::new(
        ports.library(),
        ports.sources(),
        ports.state(),
    ));
    let search = Arc::new(SearchService::new(
        ports.library(),
        ports.catalogue(),
        ports.sources(),
        ports.state(),
    ));
    let album_source = SourceIdentity::new(
        ProviderId::new("youtube_music".to_owned()).unwrap(),
        SourceScope::Global,
        "provider-album".to_owned(),
    )
    .unwrap();
    let album = ports
        .store
        .resolve_or_create_album(
            &AlbumMetadata::new("Provider Album".to_owned())
                .unwrap()
                .with_provider(Some(album_source.clone())),
        )
        .await
        .unwrap();
    let partial = ports
        .store
        .get_or_create(
            SourceIdentity::new(
                ProviderId::new("youtube_music".to_owned()).unwrap(),
                SourceScope::Global,
                "partial-track".to_owned(),
            )
            .unwrap(),
            TrackMetadata::from_names("Partial".to_owned(), vec!["Artist".to_owned()], Some(1_000))
                .unwrap(),
        )
        .await
        .unwrap();
    ports
        .store
        .set_album_source(album.id(), &album_source)
        .await
        .unwrap();
    ports
        .store
        .set_album_track(album.id(), partial.id(), &AlbumTrack::unplaced())
        .await
        .unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let completion = Arc::new(
        CompletionService::new(
            Arc::clone(&library),
            ports.catalogue(),
            ports.sources(),
            vec![Arc::new(CountingAlbumProvider {
                calls: Arc::clone(&calls),
            })],
            Arc::clone(&search),
        )
        .with_registry(ProviderRegistry::with_enabled(["youtube_music"])),
    );
    let app = router_with_completion(
        credential,
        search,
        library,
        completion,
        ports.streaming(Vec::new()),
    );
    let login = post_login(app.clone(), "flynn", &secret).await;
    let token = login["token"].as_str().unwrap();

    for query in [
        format!("?album_id={}&_sort=unknown", album.id()),
        format!("?album_id={}&unknown=1", album.id()),
    ] {
        let (status, _, body) = get(app.clone(), token, &query).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{query}");
        assert_eq!(body, json!({"error": "Invalid search parameters."}));
    }
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[sqlx::test]
async fn song_sort_aliases_match_their_canonical_orders(pool: PgPool) {
    let fixture = api_fixture(pool).await;
    for (index, album_title, album_artist, title) in [
        ("a", "Zulu Album", "Beta Artist", "Track A"),
        ("b", "Alpha Album", "Zulu Artist", "Track B"),
        ("c", "Beta Album", "Alpha Artist", "Track C"),
    ] {
        let album = add_album(&fixture.store, album_title, Some(album_artist)).await;
        let track = add_track(&fixture.store, &format!("path-{index}"), title).await;
        fixture
            .store
            .set_album_track(&album, &track, &AlbumTrack::unplaced())
            .await
            .unwrap();
    }

    for (alias, canonical) in [
        ("order_album_artist_name", "album_artist"),
        ("releaseYear", "release_year"),
        ("createdAt", "date_added"),
        ("playCount", "play_count"),
        ("playDate", "play_date"),
    ] {
        let (_, _, aliased) = get(
            fixture.app.clone(),
            &fixture.token,
            &format!("?_sort={alias}"),
        )
        .await;
        let (_, _, canonical) = get(
            fixture.app.clone(),
            &fixture.token,
            &format!("?_sort={canonical}"),
        )
        .await;
        assert_eq!(titles(&aliased), titles(&canonical), "{alias}");
    }

    let (_, _, descending) = get(
        fixture.app.clone(),
        &fixture.token,
        "?_sort=album&_order=desc",
    )
    .await;
    let (_, _, ascending) = get(fixture.app, &fixture.token, "?_sort=album&_order=asc").await;
    assert_ne!(titles(&descending), titles(&ascending));
}

#[sqlx::test]
async fn song_list_applies_available_library_selectors_and_combinations(pool: PgPool) {
    use whio_core::features::music_library::library_state::test_support::seed_play;

    let fixture = api_fixture(pool.clone()).await;
    let first_metadata = AlbumMetadata::new("First Album".to_owned())
        .unwrap()
        .with_album_artist(ArtistCredit::new("First Artist".to_owned(), None).unwrap())
        .with_year(Some(1994))
        .unwrap()
        .with_compilation(Some(true));
    let second_metadata = AlbumMetadata::new("Second Album".to_owned())
        .unwrap()
        .with_album_artist(ArtistCredit::new("Second Artist".to_owned(), None).unwrap())
        .with_year(Some(2000))
        .unwrap()
        .with_compilation(Some(false));
    let first_album = fixture
        .store
        .resolve_or_create_album(&first_metadata)
        .await
        .unwrap();
    let second_album = fixture
        .store
        .resolve_or_create_album(&second_metadata)
        .await
        .unwrap();
    let first_source = local_source("first");
    let second_source = local_source("second");
    let first = fixture
        .store
        .get_or_create(
            first_source.clone(),
            TrackMetadata::from_names(
                "First".to_owned(),
                vec!["First Artist".to_owned()],
                Some(1_000),
            )
            .unwrap()
            .with_genres(vec!["Rock".to_owned()]),
        )
        .await
        .unwrap();
    let second = fixture
        .store
        .get_or_create(
            second_source.clone(),
            TrackMetadata::from_names(
                "Second".to_owned(),
                vec!["Second Artist".to_owned()],
                Some(1_000),
            )
            .unwrap()
            .with_genres(vec!["Pop".to_owned()]),
        )
        .await
        .unwrap();
    fixture
        .store
        .set_album_track(first_album.id(), first.id(), &AlbumTrack::unplaced())
        .await
        .unwrap();
    fixture
        .store
        .set_album_track(second_album.id(), second.id(), &AlbumTrack::unplaced())
        .await
        .unwrap();
    fixture
        .store
        .set_starred(fixture.user, StateTarget::Track(first.id()), true)
        .await
        .unwrap();
    fixture
        .store
        .set_rating(fixture.user, StateTarget::Track(first.id()), Some(5))
        .await
        .unwrap();
    seed_play(&pool, fixture.user, first.id(), chrono::Utc::now()).await;

    let first_artist = fixture
        .store
        .find_artist_by_exact_name("First Artist")
        .await
        .unwrap()
        .unwrap();
    let second_artist = fixture
        .store
        .find_artist_by_exact_name("Second Artist")
        .await
        .unwrap()
        .unwrap();

    for (query, expected) in [
        ("?genre_id=Rock", vec!["First"]),
        ("?year=1994", vec!["First"]),
        ("?compilation=true", vec!["First"]),
        ("?compilation=false", vec!["Second"]),
        ("?starred=true", vec!["First"]),
        ("?starred=false", vec!["Second"]),
        ("?has_rating=true", vec!["First"]),
        ("?has_rating=false", vec!["Second"]),
        ("?recently_played=true", vec!["First"]),
        ("?recently_played=false", vec!["Second"]),
        ("?missing=false&genre_id=Rock", vec!["First"]),
        ("?role=artist&role=artist", vec!["First", "Second"]),
        (
            "?genre_id=Rock&year=1994&starred=true&has_rating=true&compilation=true",
            vec!["First"],
        ),
    ] {
        let (status, headers, body) = get(fixture.app.clone(), &fixture.token, query).await;
        assert_eq!(status, StatusCode::OK, "{query}");
        assert_eq!(
            total(&headers).parse::<usize>().unwrap(),
            expected.len(),
            "{query}"
        );
        assert_eq!(titles(&body), expected, "{query}");
    }

    for (query, expected) in [
        (format!("?album_artist_id={first_artist}"), vec!["First"]),
        (format!("?artists_id={first_artist}"), vec!["First"]),
        (format!("?artists_id={second_artist}"), vec!["Second"]),
    ] {
        let (status, headers, body) = get(fixture.app.clone(), &fixture.token, &query).await;
        assert_eq!(status, StatusCode::OK, "{query}");
        assert_eq!(total(&headers), expected.len().to_string(), "{query}");
        assert_eq!(titles(&body), expected, "{query}");
    }

    let (status, _, body) = get(fixture.app, &fixture.token, "?library_id=1").await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body, json!({"error": "Invalid search parameters."}));
}

#[sqlx::test]
async fn album_song_query_keeps_context_filters_window_and_page_total(pool: PgPool) {
    let fixture = api_fixture(pool).await;
    let album = add_album(&fixture.store, "Discovery", Some("Daft Punk")).await;
    let first = add_track(&fixture.store, "path-a", "First").await;
    let second = add_track(&fixture.store, "path-b", "Second").await;
    for (position, track) in [(1, &first), (2, &second)] {
        fixture
            .store
            .set_album_track(
                &album,
                track,
                &AlbumTrack::new(Some(1), Some(position), u32::from(position - 1)).unwrap(),
            )
            .await
            .unwrap();
    }

    let (status, headers, body) = get(
        fixture.app,
        &fixture.token,
        &format!("?album_id={album}&missing=false&_start=1&_end=2"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(total(&headers), "2");
    assert_eq!(titles(&body), ["Second"]);
    assert_eq!(body[0]["albumId"], json!(album.to_string()));
    assert_eq!(body[0]["trackNumber"], json!(2));
}

#[sqlx::test]
async fn repeated_album_selectors_dedupe_and_bound_the_id_set(pool: PgPool) {
    let fixture = api_fixture(pool).await;
    let first_album = add_album(&fixture.store, "First Album", Some("Artist")).await;
    let second_album = add_album(&fixture.store, "Second Album", Some("Artist")).await;
    let first = add_track(&fixture.store, "path-a", "First").await;
    let second = add_track(&fixture.store, "path-b", "Second").await;
    for (album, track) in [(&first_album, &first), (&second_album, &second)] {
        fixture
            .store
            .set_album_track(album, track, &AlbumTrack::unplaced())
            .await
            .unwrap();
    }

    let repeated =
        format!("?album_id={first_album}&album_id={second_album}&album_id={first_album}");
    let comma = format!("?album_id={first_album},{second_album}");
    let (_, repeated_headers, repeated_body) =
        get(fixture.app.clone(), &fixture.token, &repeated).await;
    let (_, comma_headers, comma_body) = get(fixture.app.clone(), &fixture.token, &comma).await;
    assert_eq!(total(&repeated_headers), "2");
    assert_eq!(total(&comma_headers), "2");
    assert_eq!(titles(&repeated_body), ["First", "Second"]);
    assert_eq!(titles(&repeated_body), titles(&comma_body));

    let too_many = (0..=100)
        .map(|_| format!("album_id={}", Uuid::now_v7()))
        .collect::<Vec<_>>()
        .join("&");
    let (status, _, body) = get(fixture.app, &fixture.token, &format!("?{too_many}")).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body, json!({"error": "Invalid search parameters."}));
}

#[sqlx::test]
async fn unrated_song_omits_rating(pool: PgPool) {
    let fixture = api_fixture(pool).await;
    let rated = add_track(&fixture.store, "path-a", "Rated").await;
    add_track(&fixture.store, "path-b", "Unrated").await;
    fixture
        .store
        .set_rating(fixture.user, StateTarget::Track(&rated), Some(4))
        .await
        .unwrap();

    let (status, _, body) = get(fixture.app, &fixture.token, "").await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body[0]["title"], json!("Rated"));
    assert_eq!(body[0]["rating"], json!(4));
    assert_eq!(body[1]["title"], json!("Unrated"));
    assert!(body[1].get("rating").is_none(), "unrated omits rating");
}

#[sqlx::test]
async fn membership_facts_reach_the_dto(pool: PgPool) {
    let fixture = api_fixture(pool).await;
    let album = add_album(&fixture.store, "Discovery", Some("Daft Punk")).await;
    let placed = add_track(&fixture.store, "path-a", "Placed").await;
    add_track(&fixture.store, "path-b", "Loose").await;
    fixture
        .store
        .set_album_track(
            &album,
            &placed,
            &AlbumTrack::new(Some(1), Some(3), 2).unwrap(),
        )
        .await
        .unwrap();

    let (status, _, body) = get(fixture.app, &fixture.token, "").await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body[0]["title"], json!("Loose"));
    assert!(body[0].get("trackNumber").is_none(), "unplaced omits track");
    assert!(body[0].get("discNumber").is_none(), "unplaced omits disc");
    assert_eq!(body[1]["title"], json!("Placed"));
    assert_eq!(body[1]["trackNumber"], json!(3));
    assert_eq!(body[1]["discNumber"], json!(1));
}

#[sqlx::test]
async fn album_artist_is_honest(pool: PgPool) {
    let fixture = api_fixture(pool).await;
    let discovery = add_album(&fixture.store, "Discovery", Some("Daft Punk")).await;
    let bootleg = add_album(&fixture.store, "Bootleg", None).await;
    let credited = add_track(&fixture.store, "path-a", "Credited").await;
    let plain = add_track(&fixture.store, "path-b", "Plain").await;
    fixture
        .store
        .set_album_track(
            &discovery,
            &credited,
            &AlbumTrack::new(Some(1), Some(1), 0).unwrap(),
        )
        .await
        .unwrap();
    fixture
        .store
        .set_album_track(
            &bootleg,
            &plain,
            &AlbumTrack::new(Some(1), Some(1), 0).unwrap(),
        )
        .await
        .unwrap();

    let (status, _, body) = get(fixture.app, &fixture.token, "").await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body[0]["title"], json!("Credited"));
    assert_eq!(body[0]["albumArtist"], json!("Daft Punk"));
    assert_eq!(body[0]["artist"], json!("Artist"));
    assert_eq!(body[1]["title"], json!("Plain"));
    assert_eq!(body[1]["artist"], json!("Artist"));
    assert!(
        body[1].get("albumArtist").is_none(),
        "an unknown album artist is absent, not the track artist"
    );
}

#[sqlx::test]
async fn search_service_is_untouched(pool: PgPool) {
    let fixture = api_fixture(pool).await;
    add_track(&fixture.store, "path-a", "Stored Song").await;

    let (status, headers, body) = get(
        fixture.app.clone(),
        &fixture.token,
        "?title=stored%20song&_start=0&_end=9",
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(titles(&body), ["Stored Song"]);
    assert_eq!(total(&headers), "1");

    let (status, headers, body) = get(fixture.app, &fixture.token, "?title=absent").await;

    assert_eq!(status, StatusCode::OK);
    assert!(titles(&body).is_empty());
    assert_eq!(total(&headers), "0");
}

#[sqlx::test]
async fn text_song_list_discovers_provider_tracks_without_engagement(pool: PgPool) {
    let candidate = CatalogueCandidate::new(
        SourceIdentity::new(
            ProviderId::new("youtube_music".to_owned()).unwrap(),
            SourceScope::Global,
            "vid-1".to_owned(),
        )
        .unwrap(),
        TrackMetadata::from_names(
            "Provider Song".to_owned(),
            vec!["Artist".to_owned()],
            Some(1_000),
        )
        .unwrap(),
    );
    let fixture = fixture(
        pool,
        vec![
            Ok(vec![candidate.clone()]),
            Ok(vec![candidate.clone()]),
            Ok(vec![candidate]),
        ],
    )
    .await;

    let found = get_plain(
        fixture.app.clone(),
        &format!(
            "/compat/subsonic/rest/search3.view?u=flynn&v=1.16.1&c=test-client&p={}&query=provider",
            fixture.secret
        ),
    )
    .await;
    let songs = found["subsonic-response"]["searchResult3"]["song"]
        .as_array()
        .expect("provider results");
    assert_eq!(songs.len(), 1);
    let track_id: TrackId = songs[0]["id"].as_str().unwrap().parse().unwrap();

    let (status, headers, body) = get(
        fixture.app.clone(),
        &fixture.token,
        "?title=provider%20song",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(titles(&body), ["Provider Song"]);
    assert_eq!(total(&headers), "1");

    fixture
        .store
        .set_rating(fixture.user, StateTarget::Track(&track_id), Some(5))
        .await
        .unwrap();

    let (status, headers, body) = get(fixture.app, &fixture.token, "?title=provider%20song").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(titles(&body), ["Provider Song"]);
    assert_eq!(total(&headers), "1");
    assert_eq!(body[0]["id"], json!(track_id.to_string()));
}
