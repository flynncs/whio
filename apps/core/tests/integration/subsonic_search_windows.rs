use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use serde_json::Value;
use sqlx::PgPool;
use std::collections::BTreeSet;
use tower::ServiceExt;
use whio_core::features::music_library::catalogue::{
    CatalogueCandidate, ProviderError, SearchLimit, SearchOffset, SearchQuery,
    SearchRequest as CatalogueSearchRequest, SearchWindow,
};
use whio_core::features::music_library::domain::{
    AlbumMetadata, AlbumTrack, ArtistCredit, ArtistListFilter, ArtistSort, CreditRole, ProviderId,
    RoleSelector, RoleSet, SourceIdentity, SourceScope, TrackMetadata,
};
use whio_core::features::music_library::test_support::{
    CatalogueWrite, LibraryRead, PostgresTrackRepository,
};
use whio_core::platform::request::{RequestContext, RequestId};

use crate::support;

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
    let (status, body) = status(app, uri).await;
    assert_eq!(status, StatusCode::OK);

    body
}

async fn status(app: axum::Router, uri: &str) -> (StatusCode, Value) {
    let request = Request::builder()
        .method("GET")
        .uri(uri)
        .header("x-request-id", support::TEST_REQUEST_ID)
        .body(Body::empty())
        .unwrap();

    let response = app.oneshot(request).await.unwrap();
    let status = response.status();
    let body = to_bytes(response.into_body(), 1 << 20).await.unwrap();
    (status, serde_json::from_slice(&body).unwrap())
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
async fn search3_zero_kind_count_suppresses_only_that_kind(pool: PgPool) {
    seed_matches(&pool).await;
    let (app, secret) = client(pool, empty_responses(3));
    let auth = auth_query(&secret);

    let control = get(
        app.clone(),
        &search3(
            "query=song&songCount=20&albumCount=20&artistCount=20",
            &auth,
        ),
    )
    .await;
    assert_eq!(control["subsonic-response"]["status"], "ok");
    let control = &control["subsonic-response"]["searchResult3"];

    for (uri, suppressed) in [
        (
            "query=song&songCount=0&albumCount=20&artistCount=20",
            "song",
        ),
        (
            "query=song&songCount=20&albumCount=0&artistCount=20",
            "album",
        ),
        (
            "query=song&songCount=20&albumCount=20&artistCount=0",
            "artist",
        ),
    ] {
        let body = get(app.clone(), &search3(uri, &auth)).await;
        assert_eq!(body["subsonic-response"]["status"], "ok", "uri: {uri}");
        let result = &body["subsonic-response"]["searchResult3"];
        assert!(
            result.get(suppressed).is_none(),
            "the zero kind is absent, uri: {uri}"
        );
        for kept in ["song", "album", "artist"] {
            if kept == suppressed {
                continue;
            }
            assert_eq!(
                result[kept], control[kept],
                "kept kind {kept} matches the control, uri: {uri}"
            );
        }
    }
}

#[sqlx::test]
async fn search3_all_kinds_suppressed_returns_an_empty_ok_result(pool: PgPool) {
    seed_matches(&pool).await;
    let (app, secret) = client(pool, empty_responses(0));
    let auth = auth_query(&secret);

    let body = get(
        app,
        &search3("query=song&songCount=0&albumCount=0&artistCount=0", &auth),
    )
    .await;

    assert_eq!(body["subsonic-response"]["status"], "ok");
    let result = &body["subsonic-response"]["searchResult3"];
    assert!(result.get("song").is_none());
    assert!(result.get("album").is_none());
    assert!(result.get("artist").is_none());
}

#[sqlx::test]
async fn search3_suppressed_kind_still_validates_parameters(pool: PgPool) {
    seed_matches(&pool).await;
    let (app, secret) = client(pool, empty_responses(0));
    let auth = auth_query(&secret);

    for uri in [
        "query=song&songCount=0&songOffset=abc",
        "query=song&songCount=0&songOffset=1001",
        "query=song&albumCount=many",
    ] {
        let (status, body) = status(app.clone(), &search3(uri, &auth)).await;

        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "uri: {uri}");
        assert_eq!(body["subsonic-response"]["status"], "failed", "uri: {uri}");
        assert!(
            body["subsonic-response"]["searchResult3"].is_null(),
            "uri: {uri}"
        );
    }
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

async fn seed_album_artist_membership(pool: &PgPool) {
    let repository = PostgresTrackRepository::new(pool.clone());
    let album = repository
        .resolve_or_create_album(
            &AlbumMetadata::new("Search Album".to_owned())
                .unwrap()
                .with_album_artist(
                    ArtistCredit::new("Search Albumartist".to_owned(), None).unwrap(),
                ),
        )
        .await
        .unwrap();
    let track = repository
        .get_or_create(
            local_source("path-search-credit"),
            TrackMetadata::from_names(
                "Search Song".to_owned(),
                vec!["Search Trackcredit".to_owned()],
                Some(1_000),
            )
            .unwrap()
            .with_credits(vec![
                ArtistCredit::new("Search Composer".to_owned(), None)
                    .unwrap()
                    .with_role(CreditRole::Composer),
            ]),
        )
        .await
        .unwrap();

    repository
        .set_album_track(album.id(), track.id(), &AlbumTrack::unplaced())
        .await
        .unwrap();
}

#[sqlx::test]
async fn search3_query_artists_are_album_artists(pool: PgPool) {
    seed_album_artist_membership(&pool).await;
    let (app, secret) = client(pool, empty_responses(1));
    let auth = auth_query(&secret);

    let body = get(app, &search3("query=search", &auth)).await;

    assert_eq!(
        names(&body, "artist", "name"),
        ["Search Albumartist"],
        "search3's query arm returns the album artists, not every credited artist"
    );
}

#[sqlx::test]
async fn native_search_artists_are_unchanged(pool: PgPool) {
    seed_album_artist_membership(&pool).await;
    let (search, _, _) = services(pool, empty_responses(1));
    let request = CatalogueSearchRequest::uniform(
        SearchQuery::new("search".to_owned()).unwrap(),
        SearchWindow::new(SearchLimit::new(20).unwrap(), SearchOffset::new(0).unwrap()),
    );
    let context = RequestContext::new(RequestId::new("native-search".to_owned()).unwrap());

    let outcome = search.search(&request, None, &context).await.unwrap();

    assert_eq!(
        outcome
            .artists
            .iter()
            .map(|summary| summary.artist().name().to_owned())
            .collect::<BTreeSet<_>>(),
        BTreeSet::from([
            "Search Albumartist".to_owned(),
            "Search Trackcredit".to_owned(),
            "Search Composer".to_owned(),
        ]),
        "the native search keeps searching every credited artist"
    );
}

#[sqlx::test]
async fn the_read_takes_one_membership(pool: PgPool) {
    seed_album_artist_membership(&pool).await;
    let repository = PostgresTrackRepository::new(pool);

    let filters = [
        ArtistListFilter::default(),
        ArtistListFilter::new().with_role(Some(RoleSet::new([RoleSelector::AlbumArtist]).unwrap())),
        ArtistListFilter::new().with_role(Some(RoleSet::new([RoleSelector::Composer]).unwrap())),
    ];

    for filter in filters {
        let listed = repository
            .list_artists(0, 25, ArtistSort::Name, &filter, None)
            .await
            .unwrap()
            .into_iter()
            .map(|summary| summary.artist().name().to_owned())
            .filter(|name| name.starts_with("Search "))
            .collect::<BTreeSet<_>>();
        let searched = repository
            .search_artists("search", 25, None, &filter)
            .await
            .unwrap()
            .into_iter()
            .map(|summary| summary.artist().name().to_owned())
            .collect::<BTreeSet<_>>();

        assert_eq!(
            searched, listed,
            "the search read and the artist list share one membership expression"
        );
    }
}
