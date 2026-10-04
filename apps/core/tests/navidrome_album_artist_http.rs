#![cfg(feature = "postgres-integration")]

mod support;

use std::{path::PathBuf, sync::Arc};

use axum::{
    body::{Body, to_bytes},
    http::{HeaderMap, Request, StatusCode},
};
use serde_json::{Value, json};
use sqlx::PgPool;
use tower::ServiceExt;
use uuid::Uuid;

use whio_core::{
    app::AppState,
    features::identity::service::CredentialService,
    features::music_library::{
        domain::{
            AlbumId, AlbumMetadata, AlbumTrack, ArtistCredit, ArtistId, ProviderId, SourceIdentity,
            SourceScope, StateTarget, TrackId, TrackMetadata,
        },
        library::{Library, LibraryId, LibraryRegistration, LibraryRegistry},
        library_state::test_support::seed_play,
        test_support::{
            CatalogueWrite, LibraryStore, PostgresLibraryStore, PostgresTrackRepository,
            StateStore, TrackRepositoryError,
        },
    },
};

use support::{credentials::database_credential_service, services};

const API: &str = "/compat/navidrome/api";
const MUSIC_ROOT: &str = "/music";
const ELSEWHERE_ROOT: &str = "/elsewhere";

struct Fixture {
    app: axum::Router,
    token: String,
    user: Uuid,
    store: Arc<PostgresTrackRepository>,
    pool: PgPool,
    music: Library,
    elsewhere: Library,
}

async fn fixture(pool: PgPool) -> Fixture {
    let (credential, secret) = database_credential_service(pool.clone()).await;
    let (search, library, playback) = services(pool.clone(), Vec::new());
    let registry = Arc::new(LibraryRegistry::new(Arc::new(PostgresLibraryStore::new(
        pool.clone(),
    ))));
    let registered = registry
        .register(&[
            LibraryRegistration::new("music".to_owned(), vec![PathBuf::from(MUSIC_ROOT)]),
            LibraryRegistration::new("elsewhere".to_owned(), vec![PathBuf::from(ELSEWHERE_ROOT)]),
        ])
        .await
        .expect("registered libraries");
    let app = whio_core::app::router(
        AppState::new(credential, search, library, playback).with_library_registry(registry),
    );
    let login = post_login(app.clone(), "flynn", &secret).await;
    let token = login["token"].as_str().expect("token").to_owned();
    let user = login["id"]
        .as_str()
        .expect("user id")
        .parse()
        .expect("uuid");
    let library_named = |label: &str| {
        registered
            .iter()
            .find(|library| library.label() == label)
            .expect("a registered library")
            .clone()
    };

    Fixture {
        app,
        token,
        user,
        store: Arc::new(PostgresTrackRepository::new(pool.clone())),
        pool,
        music: library_named("music"),
        elsewhere: library_named("elsewhere"),
    }
}

async fn app_with_libraries(
    pool: PgPool,
    credential: Arc<CredentialService>,
    registry: Option<Arc<LibraryRegistry>>,
) -> axum::Router {
    let (search, library, playback) = services(pool, Vec::new());
    let state = AppState::new(credential, search, library, playback);

    whio_core::app::router(match registry {
        Some(registry) => state.with_library_registry(registry),
        None => state,
    })
}

struct FailingLibraryStore;

#[async_trait::async_trait]
impl LibraryStore for FailingLibraryStore {
    async fn find_by_label(&self, _label: &str) -> Result<Option<Library>, TrackRepositoryError> {
        Err(TrackRepositoryError::Unavailable)
    }

    async fn insert_library(
        &self,
        _label: &str,
        _roots: &[PathBuf],
    ) -> Result<Library, TrackRepositoryError> {
        Err(TrackRepositoryError::Unavailable)
    }

    async fn replace_roots(
        &self,
        _library_id: &LibraryId,
        _roots: &[PathBuf],
    ) -> Result<(), TrackRepositoryError> {
        Err(TrackRepositoryError::Unavailable)
    }

    async fn list_libraries(&self) -> Result<Vec<Library>, TrackRepositoryError> {
        Err(TrackRepositoryError::Unavailable)
    }

    async fn scopes_for_ids(
        &self,
        _ids: &[LibraryId],
    ) -> Result<Vec<String>, TrackRepositoryError> {
        Err(TrackRepositoryError::Unavailable)
    }
}

async fn post_login(app: axum::Router, username: &str, password: &str) -> Value {
    let request = Request::builder()
        .method("POST")
        .uri("/compat/navidrome/auth/login")
        .header("x-request-id", "request-123")
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

async fn request(app: &axum::Router, token: &str, path: &str) -> (StatusCode, HeaderMap, Value) {
    let request = Request::builder()
        .method("GET")
        .uri(format!("{API}{path}"))
        .header("x-request-id", "request-123")
        .header("x-nd-authorization", format!("Bearer {token}"))
        .body(Body::empty())
        .unwrap();
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let body = to_bytes(response.into_body(), 8 * 1024 * 1024)
        .await
        .unwrap();

    (status, headers, serde_json::from_slice(&body).unwrap())
}

async fn get(fixture: &Fixture, path: &str) -> (StatusCode, HeaderMap, Value) {
    request(&fixture.app, &fixture.token, path).await
}

async fn page(fixture: &Fixture, resource: &str, query: &str) -> (u32, Vec<String>) {
    let (status, headers, body) = get(fixture, &format!("/{resource}{query}")).await;
    assert_eq!(status, StatusCode::OK, "{resource} query {query}");
    assert!(body.is_array(), "{resource} query {query} is a list");

    (count(&headers), names(&body))
}

async fn album_page(fixture: &Fixture, query: &str) -> (u32, Vec<String>) {
    page(fixture, "album", query).await
}

async fn artist_page(fixture: &Fixture, query: &str) -> (u32, Vec<String>) {
    page(fixture, "artist", query).await
}

async fn rejected(fixture: &Fixture, path: &str) {
    let (status, _, body) = get(fixture, path).await;

    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "rejected: {path}");
    assert_eq!(body, json!({"error": "Invalid search parameters."}));
}

fn names(body: &Value) -> Vec<String> {
    body.as_array()
        .expect("a list")
        .iter()
        .map(|item| item["name"].as_str().expect("a name").to_owned())
        .collect()
}

fn count(headers: &HeaderMap) -> u32 {
    headers["x-total-count"]
        .to_str()
        .expect("a count")
        .parse()
        .expect("a number")
}

fn listing(total: u32, names: &[String], expected_total: u32, expected: &[&str]) {
    assert_eq!(total, expected_total, "total for {expected:?}");
    let expected = expected
        .iter()
        .map(|name| (*name).to_owned())
        .collect::<Vec<String>>();
    assert_eq!(names, expected.as_slice(), "items");
}

fn absent(body: &Value, field: &str) {
    assert!(body.get(field).is_none(), "{field} must be absent");
}

fn local_source(root: &str, external_id: &str) -> SourceIdentity {
    SourceIdentity::new(
        ProviderId::new("local".to_owned()).expect("valid provider"),
        SourceScope::Named(root.to_owned()),
        external_id.to_owned(),
    )
    .expect("valid source")
}

async fn store_track(
    fixture: &Fixture,
    root: &str,
    external_id: &str,
    metadata: TrackMetadata,
) -> TrackId {
    fixture
        .store
        .get_or_create(local_source(root, external_id), metadata)
        .await
        .expect("a stored track")
        .id()
        .clone()
}

async fn add_track(
    fixture: &Fixture,
    root: &str,
    external_id: &str,
    title: &str,
    artist: &str,
    genres: &[&str],
) -> TrackId {
    store_track(
        fixture,
        root,
        external_id,
        TrackMetadata::from_names(title.to_owned(), vec![artist.to_owned()], Some(1_000))
            .expect("valid metadata")
            .with_genres(genres.iter().map(|genre| (*genre).to_owned()).collect()),
    )
    .await
}

async fn add_album(
    fixture: &Fixture,
    title: &str,
    album_artist: Option<(&str, Option<&str>)>,
    year: Option<i32>,
    compilation: Option<bool>,
    root: &str,
    external_id: &str,
) -> AlbumId {
    let mut metadata = AlbumMetadata::new(title.to_owned()).expect("valid album");
    if let Some((name, musicbrainz_id)) = album_artist {
        metadata = metadata.with_album_artist(
            ArtistCredit::new(name.to_owned(), musicbrainz_id.map(str::to_owned))
                .expect("valid credit"),
        );
    }
    if let Some(year) = year {
        metadata = metadata.with_year(Some(year)).expect("valid year");
    }
    let album = fixture
        .store
        .resolve_or_create_album(&metadata.with_compilation(compilation))
        .await
        .expect("a stored album");
    fixture
        .store
        .set_album_source(album.id(), &local_source(root, external_id))
        .await
        .expect("an album source");

    album.id().clone()
}

async fn place(fixture: &Fixture, album: &AlbumId, track: &TrackId, position: u16) {
    fixture
        .store
        .set_album_track(
            album,
            track,
            &AlbumTrack::new(Some(1), Some(position), u32::from(position) - 1)
                .expect("valid membership"),
        )
        .await
        .expect("membership stored");
}

async fn album_artist_of(fixture: &Fixture, album: &AlbumId) -> ArtistId {
    let id: Uuid = sqlx::query_scalar("SELECT album_artist_id FROM albums WHERE id = $1")
        .bind(album.as_uuid())
        .fetch_one(&fixture.pool)
        .await
        .expect("an album artist");

    ArtistId::from_uuid(id)
}

async fn set_missing(fixture: &Fixture, root: &str, external_id: &str) {
    sqlx::query(
        "UPDATE track_sources SET missing_at = CURRENT_TIMESTAMP \
         WHERE provider_id = 'local' AND source_scope = $1 AND external_id = $2",
    )
    .bind(format!("named:{root}"))
    .bind(external_id)
    .execute(&fixture.pool)
    .await
    .expect("a missing source");
}

async fn age_album_source(fixture: &Fixture, album: &AlbumId, days: i64) {
    sqlx::query("UPDATE album_sources SET created_at = $2 WHERE album_id = $1")
        .bind(album.as_uuid())
        .bind(chrono::Utc::now() + chrono::Duration::days(days))
        .execute(&fixture.pool)
        .await
        .expect("an aged album source");
}

struct Contrast {
    kept: AlbumId,
    daft_punk: ArtistId,
    justice: ArtistId,
}

async fn contrast(fixture: &Fixture) -> Contrast {
    let kept = add_album(
        fixture,
        "Kept",
        Some(("Daft Punk", Some("mbz-daft-punk"))),
        Some(1994),
        Some(true),
        MUSIC_ROOT,
        "kept.flac",
    )
    .await;
    let other = add_album(
        fixture,
        "Other",
        Some(("Justice", Some("mbz-justice"))),
        Some(2001),
        Some(false),
        ELSEWHERE_ROOT,
        "other.flac",
    )
    .await;
    let kept_track = add_track(
        fixture,
        MUSIC_ROOT,
        "kept-track.flac",
        "Kept Track",
        "Guest One",
        &["Rock"],
    )
    .await;
    let other_track = add_track(
        fixture,
        ELSEWHERE_ROOT,
        "other-track.flac",
        "Other Track",
        "Guest Two",
        &["Pop"],
    )
    .await;
    place(fixture, &kept, &kept_track, 1).await;
    place(fixture, &other, &other_track, 1).await;
    fixture
        .store
        .set_starred(fixture.user, StateTarget::Album(&kept), true)
        .await
        .unwrap();
    fixture
        .store
        .set_rating(fixture.user, StateTarget::Album(&kept), Some(4))
        .await
        .unwrap();
    seed_play(&fixture.pool, fixture.user, &kept_track, chrono::Utc::now()).await;
    set_missing(fixture, ELSEWHERE_ROOT, "other-track.flac").await;

    let daft_punk = album_artist_of(fixture, &kept).await;
    let justice = album_artist_of(fixture, &other).await;
    fixture
        .store
        .set_artist_source(&daft_punk, &local_source(MUSIC_ROOT, "daft-punk"))
        .await
        .unwrap();
    fixture
        .store
        .set_artist_source(&justice, &local_source(ELSEWHERE_ROOT, "justice"))
        .await
        .unwrap();

    Contrast {
        kept,
        daft_punk,
        justice,
    }
}

#[sqlx::test]
async fn navidrome_album_list_translates_every_client_field(pool: PgPool) {
    let fixture = fixture(pool).await;
    let seed = contrast(&fixture).await;
    let daft_punk = seed.daft_punk.to_string();
    let justice = seed.justice.to_string();

    let (total, all) = album_page(&fixture, "").await;
    listing(total, &all, 2, &["Kept", "Other"]);

    let (total, kept) = album_page(&fixture, &format!("?artist_id={daft_punk}")).await;
    listing(total, &kept, 1, &["Kept"]);
    let (total, both) = album_page(
        &fixture,
        &format!("?artist_id={daft_punk}&artist_id={justice}"),
    )
    .await;
    assert_eq!((total, both.len()), (2, 2), "repeated keys are an array");
    let (total, both) = album_page(&fixture, &format!("?artist_id={daft_punk},{justice}")).await;
    assert_eq!(
        (total, both.len()),
        (2, 2),
        "a comma-separated value is an array"
    );

    let (total, kept) = album_page(&fixture, "?genre_id=Rock").await;
    listing(total, &kept, 1, &["Kept"]);
    let (total, both) = album_page(&fixture, "?genre_id=Rock&genre_id=Pop").await;
    assert_eq!((total, both.len()), (2, 2));

    let (total, kept) = album_page(&fixture, "?compilation=true").await;
    listing(total, &kept, 1, &["Kept"]);
    let (total, other) = album_page(&fixture, "?compilation=false").await;
    listing(total, &other, 1, &["Other"]);

    let (total, kept) = album_page(&fixture, "?has_rating=true").await;
    listing(total, &kept, 1, &["Kept"]);
    let (total, other) = album_page(&fixture, "?has_rating=false").await;
    listing(total, &other, 1, &["Other"]);

    let (total, kept) = album_page(&fixture, "?recently_played=true").await;
    listing(total, &kept, 1, &["Kept"]);
    let (total, other) = album_page(&fixture, "?recently_played=false").await;
    listing(total, &other, 1, &["Other"]);

    let (total, starred) = album_page(&fixture, "?starred=true").await;
    listing(total, &starred, 1, &["Kept"]);
    let (total, unstarred) = album_page(&fixture, "?starred=false").await;
    listing(total, &unstarred, 1, &["Other"]);
    let (total, either) = album_page(&fixture, "").await;
    assert_eq!((total, either.len()), (2, 2));
    assert_ne!(
        unstarred, either,
        "a present starred=false is not the absent starred"
    );

    let (total, kept) = album_page(&fixture, "?name=kept").await;
    listing(total, &kept, 1, &["Kept"]);

    let (total, kept) = album_page(&fixture, "?year=1994").await;
    listing(total, &kept, 1, &["Kept"]);
    let (total, none) = album_page(&fixture, "?year=1995").await;
    assert_eq!((total, none.len()), (0, 0));

    let music = fixture.music.numeric_id();
    let elsewhere = fixture.elsewhere.numeric_id();
    let (total, kept) = album_page(&fixture, &format!("?library_id={music}")).await;
    listing(total, &kept, 1, &["Kept"]);
    let (total, other) = album_page(&fixture, &format!("?library_id={elsewhere}")).await;
    listing(total, &other, 1, &["Other"]);
    let (total, both) = album_page(
        &fixture,
        &format!("?library_id={music}&library_id={elsewhere}"),
    )
    .await;
    assert_eq!((total, both.len()), (2, 2));

    let (total, kept) = album_page(&fixture, "?missing=false").await;
    listing(total, &kept, 1, &["Kept"]);
    let (total, other) = album_page(&fixture, "?missing=true").await;
    listing(total, &other, 1, &["Other"]);

    let (status, headers, body) = get(&fixture, "/album?_start=0&_end=1").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(count(&headers), 2);
    assert_eq!(names(&body), ["Kept"]);

    let (status, headers, body) = get(&fixture, "/album?_start=1000&_end=1010").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(count(&headers), 2);
    assert!(body.as_array().unwrap().is_empty());

    rejected(&fixture, "/album?_start=ten").await;
    rejected(&fixture, "/album?_end=abc").await;
    rejected(&fixture, "/album?_start=0&_end=100000").await;
    rejected(&fixture, "/album?_start=10&_end=9&year=abc").await;
}

#[sqlx::test]
async fn navidrome_album_list_rejects_an_unknown_field(pool: PgPool) {
    let fixture = fixture(pool).await;
    contrast(&fixture).await;

    let (status, _, _) = get(&fixture, "/album").await;
    assert_eq!(status, StatusCode::OK);

    rejected(&fixture, "/album?genre=Rock").await;
    rejected(&fixture, "/album?bogus=1").await;
    rejected(&fixture, "/album?_filters=%7B%7D").await;
    rejected(&fixture, "/album?title=kept").await;
    rejected(&fixture, "/album?role=albumartist").await;
    rejected(&fixture, "/artist?genre_id=Rock").await;
    rejected(
        &fixture,
        "/artist?artist_id=01a07482-0f2d-7511-a4da-ac170d6f97bd",
    )
    .await;
    rejected(&fixture, "/artist?bogus=1").await;

    let (status, _, _) = get(&fixture, "/artist").await;
    assert_eq!(status, StatusCode::OK);
    let (status, _, _) = get(&fixture, "/artist?role=").await;
    assert_eq!(status, StatusCode::OK);
    let (status, _, _) = get(&fixture, "/artist?role=artist").await;
    assert_eq!(status, StatusCode::OK);
}

#[sqlx::test]
async fn navidrome_album_order_comes_from_the_request(pool: PgPool) {
    let fixture = fixture(pool).await;
    let mut albums = Vec::new();
    for (title, artist, tracks) in [
        ("Alpha", "Alpha Artist", 1),
        ("Alpha Two", "Alpha Artist", 2),
        ("Beta", "Beta Artist", 4),
    ] {
        let album = add_album(
            &fixture,
            title,
            Some((artist, None)),
            None,
            None,
            MUSIC_ROOT,
            &format!("{title}.flac"),
        )
        .await;
        for position in 1..=tracks {
            let track = add_track(
                &fixture,
                MUSIC_ROOT,
                &format!("{title}-{position}.flac"),
                &format!("{title} {position}"),
                artist,
                &[],
            )
            .await;
            place(&fixture, &album, &track, position).await;
        }
        albums.push(album);
    }
    age_album_source(&fixture, &albums[0], -10).await;
    age_album_source(&fixture, &albums[1], -30).await;

    let (total, natural) = album_page(&fixture, "?_sort=name").await;
    listing(total, &natural, 3, &["Alpha", "Alpha Two", "Beta"]);
    let (total, explicit) = album_page(&fixture, "?_sort=name&_order=desc").await;
    listing(total, &explicit, 3, &["Beta", "Alpha Two", "Alpha"]);

    let (total, natural) = album_page(&fixture, "?_sort=songCount").await;
    listing(total, &natural, 3, &["Beta", "Alpha Two", "Alpha"]);
    let (total, explicit) = album_page(&fixture, "?_sort=songCount&_order=asc").await;
    listing(total, &explicit, 3, &["Alpha", "Alpha Two", "Beta"]);
    let (total, client_spelling) = album_page(&fixture, "?_sort=songCount&_order=DESC").await;
    listing(total, &client_spelling, 3, &["Beta", "Alpha Two", "Alpha"]);

    let (total, alias) = album_page(&fixture, "?_sort=recentlyadded").await;
    listing(total, &alias, 3, &["Beta", "Alpha", "Alpha Two"]);
    let (total, alias) = album_page(&fixture, "?_sort=createdAt").await;
    listing(total, &alias, 3, &["Beta", "Alpha", "Alpha Two"]);
    let (total, alias) = album_page(&fixture, "?_sort=createdAt&_order=asc").await;
    listing(total, &alias, 3, &["Alpha Two", "Alpha", "Beta"]);

    rejected(&fixture, "/album?_sort=explicitStatus").await;
    rejected(&fixture, "/album?_sort=song_count").await;
    rejected(&fixture, "/album?_sort=Name").await;
    rejected(&fixture, "/album?_sort=bogus").await;
    rejected(&fixture, "/album?_sort=name&_order=sideways").await;

    let (total, natural) = artist_page(&fixture, "?_sort=songCount").await;
    listing(total, &natural, 2, &["Beta Artist", "Alpha Artist"]);
    let (total, explicit) = artist_page(&fixture, "?_sort=songCount&_order=asc").await;
    listing(total, &explicit, 2, &["Alpha Artist", "Beta Artist"]);
    let (total, natural) = artist_page(&fixture, "?_sort=albumCount").await;
    listing(total, &natural, 2, &["Alpha Artist", "Beta Artist"]);
    let (total, explicit) = artist_page(&fixture, "?_sort=albumCount&_order=asc").await;
    listing(total, &explicit, 2, &["Beta Artist", "Alpha Artist"]);
    rejected(&fixture, "/artist?_sort=song_count").await;
    rejected(&fixture, "/artist?_sort=Beta").await;
}

#[sqlx::test]
async fn navidrome_album_artist_filter_agrees_with_its_total(pool: PgPool) {
    let fixture = fixture(pool).await;
    let seed = contrast(&fixture).await;
    let daft_punk = seed.daft_punk.to_string();
    let justice = seed.justice.to_string();

    let (status, headers, body) = get(
        &fixture,
        &format!("/album?artist_id={daft_punk}&_start=0&_end=10"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(count(&headers), 1);
    assert_eq!(names(&body), ["Kept"]);

    let (status, headers, body) = get(
        &fixture,
        &format!("/album?artist_id={daft_punk}&starred=true&_start=0&_end=10"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!((count(&headers), names(&body).len()), (1, 1));

    let (status, headers, body) = get(
        &fixture,
        &format!("/album?artist_id={daft_punk}&artist_id={justice}&_start=0&_end=1"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        (count(&headers), names(&body).len()),
        (2, 1),
        "the count is the two-artist filter's total, not the first artist's album list"
    );

    let (status, headers, body) = get(
        &fixture,
        &format!("/album?artist_id={daft_punk}&starred=false&_start=0&_end=10"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!((count(&headers), names(&body).len()), (0, 0));

    let (status, headers, body) = get(
        &fixture,
        &format!("/album?artist_id={justice}&year=2001&_start=0&_end=10"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        (count(&headers), names(&body)),
        (1, vec!["Other".to_owned()])
    );

    for (index, expected) in [(0, "Kept"), (1, "Other")] {
        let (status, headers, body) = get(
            &fixture,
            &format!(
                "/album?artist_id={daft_punk},{justice}&_start={index}&_end={}",
                index + 1
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(count(&headers), 2);
        assert_eq!(names(&body), [expected]);
    }
}

#[sqlx::test]
async fn navidrome_artist_list_is_role_scoped(pool: PgPool) {
    let fixture = fixture(pool).await;
    let seed = contrast(&fixture).await;
    let daft_punk = seed.daft_punk.clone();

    let (total, all_roles) = artist_page(&fixture, "").await;
    assert_eq!((total, all_roles.len()), (4, 4));
    assert_eq!(
        all_roles,
        ["Daft Punk", "Guest One", "Guest Two", "Justice"]
    );

    let (total, album_artists) = artist_page(&fixture, "?role=albumartist").await;
    listing(total, &album_artists, 2, &["Daft Punk", "Justice"]);
    assert_ne!(
        all_roles, album_artists,
        "role=albumartist is not every role"
    );

    let (total, primary) = artist_page(&fixture, "?role=artist").await;
    listing(total, &primary, 2, &["Guest One", "Guest Two"]);

    let (total, blank) = artist_page(&fixture, "?role=").await;
    assert_eq!((total, blank.len()), (4, 4), "a blank role is every role");

    let (total, named) = artist_page(&fixture, "?name=daft").await;
    listing(total, &named, 1, &["Daft Punk"]);
    let (total, named) = artist_page(&fixture, "?name=%20%20").await;
    assert_eq!(
        (total, named.len()),
        (4, 4),
        "a blank name is the whole list"
    );

    fixture
        .store
        .set_starred(fixture.user, StateTarget::Artist(&daft_punk), true)
        .await
        .unwrap();
    let (total, starred) = artist_page(&fixture, "?starred=true").await;
    listing(total, &starred, 1, &["Daft Punk"]);
    let (total, unstarred) = artist_page(&fixture, "?starred=false").await;
    assert_eq!((total, unstarred.len()), (3, 3));

    let music = fixture.music.numeric_id();
    let elsewhere = fixture.elsewhere.numeric_id();
    let (total, owned) = artist_page(&fixture, &format!("?library_id={music}")).await;
    listing(total, &owned, 2, &["Daft Punk", "Guest One"]);
    let (total, owned) = artist_page(&fixture, &format!("?library_id={elsewhere}")).await;
    listing(total, &owned, 2, &["Guest Two", "Justice"]);

    let (total, present) = artist_page(&fixture, "?missing=false").await;
    listing(total, &present, 3, &["Daft Punk", "Guest One", "Justice"]);
    let (total, missing) = artist_page(&fixture, "?missing=true").await;
    listing(total, &missing, 1, &["Guest Two"]);

    let (total, scoped) = artist_page(&fixture, "?role=albumartist&missing=false").await;
    listing(total, &scoped, 2, &["Daft Punk", "Justice"]);

    let (total, sorted) = artist_page(&fixture, "?role=albumartist&_sort=albumCount").await;
    assert_eq!((total, sorted.len()), (2, 2));
    let (total, sorted) =
        artist_page(&fixture, "?role=albumartist&_sort=songCount&_order=asc").await;
    assert_eq!((total, sorted.len()), (2, 2));

    let (status, headers, body) = get(&fixture, "/artist?role=albumartist&_start=0&_end=1").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(count(&headers), 2);
    assert_eq!(names(&body), ["Daft Punk"]);

    rejected(&fixture, "/artist?role=composerx").await;
    rejected(&fixture, "/artist?library_id=999").await;
    rejected(&fixture, "/album?library_id=999").await;
}

#[sqlx::test]
async fn navidrome_album_and_artist_facts_are_sourced(pool: PgPool) {
    let fixture = fixture(pool).await;
    let seed = contrast(&fixture).await;
    let extra = add_track(
        &fixture,
        MUSIC_ROOT,
        "kept-extra.flac",
        "Kept Extra",
        "Guest One",
        &["Rock"],
    )
    .await;
    place(&fixture, &seed.kept, &extra, 2).await;
    let created = chrono::DateTime::parse_from_rfc3339("2025-01-02T03:04:05.123456Z")
        .expect("a timestamp")
        .with_timezone(&chrono::Utc);
    sqlx::query("UPDATE albums SET created_at = $2 WHERE id = $1")
        .bind(seed.kept.as_uuid())
        .bind(created)
        .execute(&fixture.pool)
        .await
        .unwrap();
    sqlx::query("UPDATE artists SET sort_name = $2 WHERE id = $1")
        .bind(seed.daft_punk.as_uuid())
        .bind("Punk, Daft")
        .execute(&fixture.pool)
        .await
        .unwrap();

    let (status, _, body) = get(&fixture, "/album?name=kept").await;
    assert_eq!(status, StatusCode::OK);
    let album = &body[0];
    assert_eq!(album["name"], json!("Kept"));
    assert_eq!(
        album["songCount"],
        json!(2),
        "the album's member track count"
    );
    assert_eq!(album["duration"], json!(2.0), "the album's summed duration");
    assert_eq!(album["year"], json!(1994));
    assert_eq!(album["rating"], json!(4));
    assert_eq!(album["playCount"], json!(1));
    assert_eq!(album["starred"], json!(true));
    assert!(album["starredAt"].as_str().is_some(), "a real star time");
    assert_eq!(album["albumArtist"], json!("Daft Punk"));
    assert_eq!(album["albumArtistId"], json!(seed.daft_punk.to_string()));
    assert_eq!(album["coverArtId"], json!(format!("al-{}", seed.kept)));
    assert_eq!(
        album["createdAt"],
        json!(created.to_rfc3339()),
        "createdAt is the album's stored creation time"
    );
    absent(album, "size");
    absent(album, "minYear");
    absent(album, "maxYear");
    absent(album, "compilation");
    absent(album, "missing");
    absent(album, "genres");
    absent(album, "libraryId");
    absent(album, "libraryName");
    absent(album, "libraryPath");
    absent(album, "orderAlbumName");
    absent(album, "orderAlbumArtistName");
    absent(album, "externalInfoUpdatedAt");

    let (status, _, body) = get(&fixture, &format!("/album/{}", seed.kept)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["songCount"], json!(2));
    assert_eq!(body["playCount"], json!(1));
    absent(&body, "size");
    absent(&body, "libraryPath");
    absent(&body, "compilation");

    let (status, _, body) = get(&fixture, "/album?name=other").await;
    assert_eq!(status, StatusCode::OK);
    let album = &body[0];
    assert!(
        album.get("rating").is_none(),
        "an unrated album omits rating rather than reporting zero"
    );
    assert!(
        album.get("starred").is_none(),
        "an unstarred album omits starred"
    );
    assert!(
        album.get("playCount").is_none(),
        "an unplayed album omits playCount"
    );
    assert!(
        album.get("playDate").is_none(),
        "an unplayed album omits playDate"
    );

    let (status, _, body) = get(&fixture, "/artist?name=daft").await;
    assert_eq!(status, StatusCode::OK);
    let artist = &body[0];
    assert_eq!(artist["name"], json!("Daft Punk"));
    assert_eq!(artist["albumCount"], json!(1), "the artist's album count");
    assert_eq!(artist["songCount"], json!(2), "the artist's song count");
    assert_eq!(artist["mbzArtistId"], json!("mbz-daft-punk"));
    assert_eq!(artist["sortArtistName"], json!("Punk, Daft"));
    assert_eq!(artist["coverArt"], json!(format!("ar-{}", seed.daft_punk)));
    absent(artist, "size");
    absent(artist, "missing");
    absent(artist, "orderArtistName");

    let (status, _, body) = get(&fixture, "/artist?name=guest%20one").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body[0]["songCount"], json!(2));
    assert_eq!(body[0]["albumCount"], json!(1));
    assert!(
        body[0].get("sortArtistName").is_none(),
        "an artist without a stored sort name omits it rather than guessing"
    );

    let (status, _, body) = get(&fixture, &format!("/artist/{}", seed.justice)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["name"], json!("Justice"));
    assert_eq!(body["albumCount"], json!(1));
    assert!(
        body.get("songCount").is_none(),
        "the detail operation computes no song count, so it is absent"
    );
    absent(&body, "size");
    absent(&body, "missing");
}

#[sqlx::test]
async fn navidrome_album_detail_and_artist_detail_keep_their_contract(pool: PgPool) {
    let fixture = fixture(pool).await;
    let seed = contrast(&fixture).await;

    let (status, _, body) = get(&fixture, &format!("/album/{}", seed.kept)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["name"], json!("Kept"));
    assert_eq!(body["albumArtist"], json!("Daft Punk"));
    assert_eq!(body["songCount"], json!(1));
    assert_eq!(body["coverArtId"], json!(format!("al-{}", seed.kept)));
    assert!(
        body["createdAt"]
            .as_str()
            .is_some_and(|value| !value.is_empty())
    );

    let (status, _, body) = get(&fixture, &format!("/artist/{}", seed.daft_punk)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["name"], json!("Daft Punk"));
    assert_eq!(body["albumCount"], json!(1));
    assert_eq!(body["coverArt"], json!(format!("ar-{}", seed.daft_punk)));

    let (status, _, body) = get(&fixture, "/album/01a07482-0f2d-7511-a4da-ac170d6f97bd").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body, json!({"error": "Not found."}));

    let (status, _, body) = get(&fixture, "/album/not-a-uuid").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body, json!({"error": "Not found."}));

    let (status, _, body) = get(&fixture, "/artist/01a07482-0f2d-7511-a4da-ac170d6f97bd").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body, json!({"error": "Not found."}));

    let (status, _, body) = get(&fixture, "/artist/not-a-uuid").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body, json!({"error": "Not found."}));
}

#[sqlx::test]
async fn navidrome_song_list_is_unaffected(pool: PgPool) {
    let fixture = fixture(pool).await;
    let seed = contrast(&fixture).await;
    let kept_album = seed.kept.to_string();

    let (status, headers, body) = get(&fixture, "/song?title=kept%20track").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(count(&headers), 1);
    assert_eq!(body[0]["title"], json!("Kept Track"));

    let (status, headers, body) = get(&fixture, "/song?_start=0&_end=9").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(count(&headers), 2);
    assert_eq!(body.as_array().unwrap().len(), 2);
    assert!(
        body.as_array()
            .unwrap()
            .iter()
            .any(|song| song["albumId"] == json!(kept_album)),
        "the song list still pairs a song with its album"
    );
}

#[sqlx::test]
async fn navidrome_library_translation_failures_are_mapped(pool: PgPool) {
    let (credential, secret) = database_credential_service(pool.clone()).await;
    let disabled = app_with_libraries(pool.clone(), Arc::clone(&credential), None).await;
    let failing = Arc::new(LibraryRegistry::new(Arc::new(FailingLibraryStore)));
    let broken = app_with_libraries(pool, credential, Some(failing)).await;
    let login = post_login(disabled.clone(), "flynn", &secret).await;
    let token = login["token"].as_str().expect("token");

    let (status, _, body) = request(&disabled, token, "/album?library_id=1").await;
    assert_eq!(
        status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "a disabled registry cannot name a library"
    );
    assert_eq!(body, json!({"error": "Invalid search parameters."}));

    let (status, _, body) = request(&disabled, token, "/artist?library_id=1").await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body, json!({"error": "Invalid search parameters."}));

    let (status, _, _) = request(&disabled, token, "/album").await;
    assert_eq!(
        status,
        StatusCode::OK,
        "a list without library_id needs no registry"
    );

    let (status, _, body) = request(&broken, token, "/album?library_id=1").await;
    assert_eq!(
        status,
        StatusCode::INTERNAL_SERVER_ERROR,
        "a store failure is not a client error"
    );
    assert_eq!(body, json!({"error": "Internal error."}));

    let (status, _, body) = request(&broken, token, "/artist?library_id=1").await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(body, json!({"error": "Internal error."}));

    let (status, _, _) = request(&broken, token, "/album").await;
    assert_eq!(status, StatusCode::OK, "a broken registry is not blanket");
}

#[sqlx::test]
async fn navidrome_album_without_an_album_artist(pool: PgPool) {
    let fixture = fixture(pool).await;
    let credited = contrast(&fixture).await;
    let bootleg = add_album(
        &fixture,
        "Bootleg",
        None,
        None,
        None,
        MUSIC_ROOT,
        "bootleg.flac",
    )
    .await;
    let track = add_track(
        &fixture,
        MUSIC_ROOT,
        "bootleg-track.flac",
        "Bootleg Track",
        "Guest",
        &[],
    )
    .await;
    place(&fixture, &bootleg, &track, 1).await;

    let (status, _, body) = get(&fixture, "/album?name=bootleg").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body.as_array().unwrap().len(), 1);
    let album = &body[0];
    assert_eq!(album["name"], json!("Bootleg"));
    absent(album, "albumArtist");
    absent(album, "albumArtistId");

    let (status, _, body) = get(&fixture, &format!("/album/{bootleg}")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["name"], json!("Bootleg"));
    absent(&body, "albumArtist");
    absent(&body, "albumArtistId");

    let (status, _, body) = get(&fixture, "/album?name=kept").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body[0]["albumArtist"], json!("Daft Punk"));
    assert_eq!(
        body[0]["albumArtistId"],
        json!(credited.daft_punk.to_string())
    );
}

#[sqlx::test]
async fn navidrome_serves_no_invented_update_time(pool: PgPool) {
    let fixture = fixture(pool).await;
    let seed = contrast(&fixture).await;

    let (status, _, body) = get(&fixture, "/album?name=kept").await;
    assert_eq!(status, StatusCode::OK);
    assert!(body[0]["createdAt"].as_str().is_some_and(|v| !v.is_empty()));
    absent(&body[0], "updatedAt");

    let (status, _, body) = get(&fixture, &format!("/album/{}", seed.kept)).await;
    assert_eq!(status, StatusCode::OK);
    absent(&body, "updatedAt");

    let (status, _, body) = get(&fixture, "/artist?name=daft").await;
    assert_eq!(status, StatusCode::OK);
    absent(&body[0], "updatedAt");

    let (status, _, body) = get(&fixture, &format!("/artist/{}", seed.daft_punk)).await;
    assert_eq!(status, StatusCode::OK);
    absent(&body, "updatedAt");
}

#[sqlx::test]
async fn navidrome_omits_an_unknown_song_duration(pool: PgPool) {
    let fixture = fixture(pool).await;
    let unknown = store_track(
        &fixture,
        MUSIC_ROOT,
        "unknown-duration.flac",
        TrackMetadata::from_names(
            "Unknown Duration".to_owned(),
            vec!["Guest".to_owned()],
            None,
        )
        .expect("valid metadata"),
    )
    .await;
    let known = add_track(
        &fixture,
        MUSIC_ROOT,
        "known-duration.flac",
        "Known Duration",
        "Guest",
        &[],
    )
    .await;

    let (status, _, body) = get(&fixture, "/song?title=unknown%20duration").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body[0]["id"], json!(unknown.to_string()));
    absent(&body[0], "duration");

    let (status, _, body) = get(&fixture, "/song?title=known%20duration").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body[0]["id"], json!(known.to_string()));
    assert_eq!(body[0]["duration"], json!(1.0));

    let (status, _, body) = get(&fixture, &format!("/song/{unknown}")).await;
    assert_eq!(status, StatusCode::OK);
    absent(&body, "duration");
}

#[sqlx::test]
async fn navidrome_omits_an_absent_album_year_and_artist_mbid(pool: PgPool) {
    let fixture = fixture(pool).await;
    contrast(&fixture).await;
    let undated = add_album(
        &fixture,
        "Undated",
        None,
        None,
        None,
        MUSIC_ROOT,
        "undated.flac",
    )
    .await;
    let undated_track = add_track(
        &fixture,
        MUSIC_ROOT,
        "undated-track.flac",
        "Undated Track",
        "Guest",
        &[],
    )
    .await;
    place(&fixture, &undated, &undated_track, 1).await;

    let (status, _, body) = get(&fixture, "/album?name=undated").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body[0]["id"], json!(undated.to_string()));
    absent(&body[0], "year");

    let (status, _, body) = get(&fixture, "/album?name=kept").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body[0]["year"], json!(1994));

    let (status, _, body) = get(&fixture, "/artist?name=guest%20one").await;
    assert_eq!(status, StatusCode::OK);
    absent(&body[0], "mbzArtistId");

    let (status, _, body) = get(&fixture, "/artist?name=daft").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body[0]["mbzArtistId"], json!("mbz-daft-punk"));
}
