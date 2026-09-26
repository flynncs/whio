#![cfg(feature = "postgres-integration")]

mod support;

use std::sync::Arc;

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
        AlbumId, AlbumMetadata, AlbumTrack, ArtistCredit, ProviderId, SourceIdentity, SourceScope,
        StateTarget, TrackId, TrackMetadata,
    },
    test_support::{CatalogueWrite, PostgresTrackRepository, StateStore},
};

use support::{credentials::database_credential_service, router, services};

const API: &str = "/compat/navidrome/api/song";

struct Fixture {
    app: axum::Router,
    token: String,
    secret: String,
    user: Uuid,
    store: Arc<PostgresTrackRepository>,
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

async fn get(app: axum::Router, token: &str, query: &str) -> (StatusCode, HeaderMap, Value) {
    let request = Request::builder()
        .method("GET")
        .uri(format!("{API}{query}"))
        .header("x-request-id", "request-123")
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
        .header("x-request-id", "request-123")
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
async fn title_filter_matches_normalised_text(pool: PgPool) {
    let fixture = api_fixture(pool).await;
    for (index, title) in [
        "Instant Crush",
        "One More Time",
        "100% Pure",
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
    assert_eq!(titles(&body).len(), 5);
    assert_eq!(total(&headers), "5");

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
    assert_eq!(body.as_array().unwrap().len(), 100);
    assert_eq!(titles(&body)[0], "Track 0000");
    assert_eq!(titles(&body)[99], "Track 0099");
    assert_eq!(total(&headers), "520");

    let (status, headers, body) =
        get(fixture.app.clone(), &fixture.token, "?_start=0&_end=100000").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body.as_array().unwrap().len(), 500);
    assert_eq!(total(&headers), "520");

    let (status, _, body) = get(fixture.app.clone(), &fixture.token, "?_start=10&_end=9").await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body, json!({"error": "Invalid search parameters."}));

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
    assert_eq!(titles(&body), ["Track 0001", "Track 0002"]);
    assert_eq!(total(&headers), "5");

    let (status, headers, body) = get(fixture.app, &fixture.token, "?_start=4&_end=4").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(titles(&body), ["Track 0004"]);
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
async fn unengaged_provider_tracks_stay_out(pool: PgPool) {
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
    let fixture = fixture(pool, vec![Ok(vec![candidate])]).await;

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
    assert!(
        titles(&body).is_empty(),
        "an un-engaged provider track stays out"
    );
    assert_eq!(total(&headers), "0");

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

#[sqlx::test]
async fn dead_song_search_path_is_removed(pool: PgPool) {
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
    let (credential, secret) = database_credential_service(pool.clone()).await;
    let (search, library, playback) = services(pool.clone(), vec![Ok(vec![candidate])]);
    let app = router(credential, search, library, playback);
    let login = post_login(app.clone(), "flynn", &secret).await;
    let token = login["token"].as_str().unwrap();

    let (status, _, body) = get(app.clone(), token, "?title=provider%20song").await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        titles(&body).is_empty(),
        "the song route is not a search path"
    );

    let found = get_plain(
        app.clone(),
        &format!(
            "/compat/subsonic/rest/search3.view?u=flynn&v=1.16.1&c=test-client&p={secret}&query=provider"
        ),
    )
    .await;
    let songs = found["subsonic-response"]["searchResult3"]["song"]
        .as_array()
        .expect("provider results");
    assert_eq!(
        songs.len(),
        1,
        "AppState search still serves the search paths"
    );

    let (status, _, body) = get(app, token, "?title=provider%20song").await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        titles(&body).is_empty(),
        "materialising a search result is not engagement"
    );
}
