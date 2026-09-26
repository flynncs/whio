#![cfg(feature = "postgres-integration")]

use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use serde_json::Value;
use sqlx::PgPool;
use tower::ServiceExt;
use whio_core::features::music_library::catalogue::{CatalogueCandidate, ProviderError};
use whio_core::features::music_library::domain::{
    AlbumMetadata, AlbumTrack, ArtistCredit, ProviderId, SourceIdentity, SourceScope, TrackMetadata,
};
use whio_core::features::music_library::test_support::{CatalogueWrite, PostgresTrackRepository};

mod support;

use support::{credential_service_with_known_app_password, router, services};

fn local_source(external_id: &str) -> SourceIdentity {
    SourceIdentity::new(
        ProviderId::new("local".to_owned()).unwrap(),
        SourceScope::Named("/music".to_owned()),
        external_id.to_owned(),
    )
    .unwrap()
}

fn local_metadata(title: &str, artist: &str) -> TrackMetadata {
    TrackMetadata::from_names(title.to_owned(), vec![artist.to_owned()], Some(1_000)).unwrap()
}

async fn seed_matches(pool: &PgPool) {
    let repository = PostgresTrackRepository::new(pool.clone());

    for index in 0..4u32 {
        let artist = format!("Song Artist {index}");
        let album = repository
            .resolve_or_create_album(
                &AlbumMetadata::new(format!("Song Album {index}"))
                    .unwrap()
                    .with_album_artist(ArtistCredit::new(artist.clone(), None).unwrap()),
            )
            .await
            .unwrap();

        for number in 0..2u32 {
            let track = repository
                .get_or_create(
                    local_source(&format!("path-{index}-{number}")),
                    local_metadata(&format!("Song Track {index}{number}"), &artist),
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

fn client(
    pool: PgPool,
    responses: Vec<Result<Vec<CatalogueCandidate>, ProviderError>>,
) -> (axum::Router, String) {
    let (credential, secret) = credential_service_with_known_app_password();
    let (search, library, playback) = services(pool, responses);

    (router(credential, search, library, playback), secret)
}

fn empty_responses(count: usize) -> Vec<Result<Vec<CatalogueCandidate>, ProviderError>> {
    (0..count).map(|_| Ok(Vec::new())).collect()
}

fn auth_query(secret: &str) -> String {
    format!("u=flynn&v=1.16.1&c=test-client&p={secret}")
}

fn search3(uri: &str, auth: &str) -> String {
    format!("/compat/subsonic/rest/search3.view?{auth}&{uri}")
}

async fn get(app: axum::Router, uri: &str) -> Value {
    let request = Request::builder()
        .method("GET")
        .uri(uri)
        .header("x-request-id", "request-123")
        .body(Body::empty())
        .unwrap();

    let response = app.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), 1 << 20).await.unwrap();
    serde_json::from_slice(&body).unwrap()
}

fn names(body: &Value, kind: &str, field: &str) -> Vec<String> {
    body["subsonic-response"]["searchResult3"][kind]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item[field].as_str().unwrap().to_owned())
        .collect()
}

#[sqlx::test]
async fn search3_windows_each_kind_independently(pool: PgPool) {
    seed_matches(&pool).await;
    let (app, secret) = client(pool, empty_responses(4));
    let auth = auth_query(&secret);

    let one_song = get(app.clone(), &search3("query=song&songCount=1", &auth)).await;
    assert_eq!(
        names(&one_song, "song", "title"),
        ["Song Track 00"],
        "songCount=1 takes one song"
    );
    assert_eq!(
        names(&one_song, "album", "name"),
        [
            "Song Album 0",
            "Song Album 1",
            "Song Album 2",
            "Song Album 3"
        ],
        "songCount does not truncate albums"
    );
    assert_eq!(
        names(&one_song, "artist", "name"),
        [
            "Song Artist 0",
            "Song Artist 1",
            "Song Artist 2",
            "Song Artist 3"
        ],
        "songCount does not truncate artists"
    );

    let one_album = get(app.clone(), &search3("query=song&albumCount=1", &auth)).await;
    assert_eq!(
        names(&one_album, "song", "title").len(),
        8,
        "albumCount does not truncate songs"
    );
    assert_eq!(names(&one_album, "album", "name"), ["Song Album 0"]);
    assert_eq!(names(&one_album, "artist", "name").len(), 4);

    let one_artist = get(app.clone(), &search3("query=song&artistCount=1", &auth)).await;
    assert_eq!(names(&one_artist, "song", "title").len(), 8);
    assert_eq!(names(&one_artist, "album", "name").len(), 4);
    assert_eq!(names(&one_artist, "artist", "name"), ["Song Artist 0"]);

    let all = get(
        app,
        &search3("query=song&songCount=1&albumCount=1&artistCount=1", &auth),
    )
    .await;
    assert_eq!(names(&all, "song", "title"), ["Song Track 00"]);
    assert_eq!(names(&all, "album", "name"), ["Song Album 0"]);
    assert_eq!(names(&all, "artist", "name"), ["Song Artist 0"]);
}

#[sqlx::test]
async fn search3_offsets_are_per_kind(pool: PgPool) {
    seed_matches(&pool).await;
    let (app, secret) = client(pool, empty_responses(3));
    let auth = auth_query(&secret);

    let songs = get(app.clone(), &search3("query=song&songOffset=1", &auth)).await;
    assert_eq!(
        names(&songs, "song", "title").len(),
        7,
        "songOffset=1 skips one song"
    );
    assert_eq!(
        names(&songs, "song", "title")[0],
        "Song Track 01",
        "songOffset=1 starts at the second song"
    );
    assert_eq!(
        names(&songs, "album", "name").len(),
        4,
        "songOffset does not page albums"
    );
    assert_eq!(
        names(&songs, "artist", "name").len(),
        4,
        "songOffset does not page artists"
    );

    let albums = get(app.clone(), &search3("query=song&albumOffset=1", &auth)).await;
    assert_eq!(names(&albums, "song", "title").len(), 8);
    assert_eq!(
        names(&albums, "album", "name"),
        ["Song Album 1", "Song Album 2", "Song Album 3"],
        "albumOffset=1 skips one album"
    );
    assert_eq!(names(&albums, "artist", "name").len(), 4);

    let artists = get(app, &search3("query=song&artistOffset=1", &auth)).await;
    assert_eq!(names(&artists, "song", "title").len(), 8);
    assert_eq!(names(&artists, "album", "name").len(), 4);
    assert_eq!(
        names(&artists, "artist", "name"),
        ["Song Artist 1", "Song Artist 2", "Song Artist 3"],
        "artistOffset=1 skips one artist"
    );
}

#[sqlx::test]
async fn search3_with_one_window_is_unchanged(pool: PgPool) {
    seed_matches(&pool).await;
    let (app, secret) = client(pool, empty_responses(1));
    let auth = auth_query(&secret);

    let body = get(
        app,
        &search3(
            "query=song&songCount=2&songOffset=1&albumCount=2&albumOffset=1&artistCount=2&artistOffset=1",
            &auth,
        ),
    )
    .await;

    assert_eq!(
        names(&body, "song", "title"),
        ["Song Track 01", "Song Track 10"],
        "the one window pages songs"
    );
    assert_eq!(
        names(&body, "album", "name"),
        ["Song Album 1", "Song Album 2"],
        "the same window pages albums"
    );
    assert_eq!(
        names(&body, "artist", "name"),
        ["Song Artist 1", "Song Artist 2"],
        "the same window pages artists"
    );
}

#[sqlx::test]
async fn the_provider_is_called_once(pool: PgPool) {
    seed_matches(&pool).await;
    let (app, secret) = client(pool, empty_responses(1));
    let auth = auth_query(&secret);

    let body = get(
        app,
        &search3("query=song&songCount=1&albumCount=1&artistCount=1", &auth),
    )
    .await;

    assert_eq!(body["subsonic-response"]["status"], "ok");
    assert_eq!(names(&body, "song", "title"), ["Song Track 00"]);
    assert_eq!(names(&body, "album", "name"), ["Song Album 0"]);
    assert_eq!(names(&body, "artist", "name"), ["Song Artist 0"]);
}
