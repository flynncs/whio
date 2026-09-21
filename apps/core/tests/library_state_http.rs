mod support;

use std::sync::Arc;

use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use serde_json::{Value, json};
use tower::ServiceExt;
use whio_core::{
    AppState,
    library::ScanService,
    playback::PlaybackProvider,
    providers::YtmAdapter,
    tracks::{
        AlbumMetadata, AlbumTrack, ArtistCredit, CatalogueWrite, InMemoryTrackRepository,
        ProviderId, SourceIdentity, SourceScope, TrackMetadata,
    },
};

use support::{StorePorts, credential_service_with_known_app_password};

struct Seeded {
    app: axum::Router,
    song: String,
    album: String,
    artist: String,
    secret: String,
}

fn local(external_id: &str) -> SourceIdentity {
    SourceIdentity::new(
        ProviderId::new("local".to_owned()).unwrap(),
        SourceScope::Named("/music".to_owned()),
        external_id.to_owned(),
    )
    .unwrap()
}

async fn seeded_app() -> Seeded {
    let (credential, secret) = credential_service_with_known_app_password();
    let root = std::env::temp_dir().join(format!("whio-state-{}", uuid::Uuid::now_v7()));
    std::fs::create_dir_all(&root).unwrap();

    let repository = Arc::new(InMemoryTrackRepository::default());
    let artist = repository
        .resolve_or_create_artist(&ArtistCredit::new("Daft Punk".to_owned(), None).unwrap())
        .await
        .unwrap();
    let album = repository
        .resolve_or_create_album(
            &AlbumMetadata::new("Discovery".to_owned())
                .unwrap()
                .with_album_artist(ArtistCredit::new("Daft Punk".to_owned(), None).unwrap())
                .with_year(Some(2001))
                .unwrap(),
        )
        .await
        .unwrap();
    let first = repository
        .get_or_create(
            local("path-a"),
            TrackMetadata::from_names(
                "First".to_owned(),
                vec!["Daft Punk".to_owned()],
                Some(1_000),
            )
            .unwrap()
            .with_genre(Some("Rock".to_owned()))
            .unwrap(),
        )
        .await
        .unwrap();
    let second = repository
        .get_or_create(
            local("path-b"),
            TrackMetadata::from_names(
                "Second".to_owned(),
                vec!["Daft Punk".to_owned()],
                Some(2_000),
            )
            .unwrap(),
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
    repository
        .set_album_track(
            album.id(),
            second.id(),
            &AlbumTrack::new(Some(1), Some(2), 1).unwrap(),
        )
        .await
        .unwrap();

    let ports = StorePorts { store: repository };
    let scanner = ScanService::new(ports.catalogue(), ports.sources(), vec![root]);
    scanner.start();
    let catalogue = ports.bare_catalogue();
    let library_state = Arc::new(whio_core::library_state::LibraryStateService::new(
        ports.state(),
        ports.library(),
    ));
    let playback = ports.streaming(vec![
        Arc::new(YtmAdapter::disabled()) as Arc<dyn PlaybackProvider>
    ]);
    let app = whio_core::router(
        AppState::new(credential, catalogue, playback)
            .with_library_state(library_state)
            .with_scanner(scanner),
    );

    Seeded {
        app,
        song: first.id().to_string(),
        album: album.id().to_string(),
        artist: artist.id().to_string(),
        secret,
    }
}

async fn get(app: axum::Router, uri: &str) -> Value {
    let (status, body) = get_status(app, uri).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");

    body
}

async fn get_status(app: axum::Router, uri: &str) -> (StatusCode, Value) {
    let request = Request::builder()
        .method("GET")
        .uri(uri)
        .header("x-request-id", "request-123")
        .body(Body::empty())
        .unwrap();

    let response = app.oneshot(request).await.unwrap();
    let status = response.status();
    let body = to_bytes(response.into_body(), 16384).await.unwrap();
    (status, serde_json::from_slice(&body).unwrap())
}

async fn post_login(app: axum::Router, username: &str, password: &str) -> (StatusCode, Value) {
    let request = Request::builder()
        .method("POST")
        .uri("/compat/navidrome/auth/login")
        .header("content-type", "application/json")
        .header("x-request-id", "request-123")
        .body(Body::from(
            json!({"username": username, "password": password}).to_string(),
        ))
        .unwrap();

    let response = app.oneshot(request).await.unwrap();
    let status = response.status();
    let body = to_bytes(response.into_body(), 16384).await.unwrap();
    (status, serde_json::from_slice(&body).unwrap())
}

async fn get_authed(app: axum::Router, uri: &str, token: &str) -> (StatusCode, Value) {
    let request = Request::builder()
        .method("GET")
        .uri(uri)
        .header("x-request-id", "request-123")
        .header("x-nd-authorization", format!("Bearer {token}"))
        .body(Body::empty())
        .unwrap();

    let response = app.oneshot(request).await.unwrap();
    let status = response.status();
    let body = to_bytes(response.into_body(), 16384).await.unwrap();
    (status, serde_json::from_slice(&body).unwrap())
}

#[tokio::test]
async fn subsonic_star_rating_play_and_genre_routes() {
    let Seeded {
        app,
        song,
        album,
        artist,
        secret,
    } = seeded_app().await;
    let auth = format!("u=flynn&v=1.16.1&c=test-client&p={secret}");

    let body = get(app.clone(), &format!("/rest/getGenres.view?{auth}")).await;
    let genres = body["subsonic-response"]["genres"]["genre"]
        .as_array()
        .unwrap();
    assert_eq!(genres.len(), 1);
    assert_eq!(genres[0]["value"], json!("Rock"));
    assert_eq!(genres[0]["songCount"], json!(1));

    let body = get(
        app.clone(),
        &format!("/rest/star.view?{auth}&id={song}&albumId={album}&artistId={artist}"),
    )
    .await;
    assert_eq!(body["subsonic-response"]["status"], json!("ok"));

    let body = get(
        app.clone(),
        &format!("/rest/setRating.view?{auth}&id={song}&rating=4"),
    )
    .await;
    assert_eq!(body["subsonic-response"]["status"], json!("ok"));

    get(
        app.clone(),
        &format!("/rest/scrobble.view?{auth}&id={song}"),
    )
    .await;
    get(
        app.clone(),
        &format!("/rest/scrobble.view?{auth}&id={song}"),
    )
    .await;

    let body = get(app.clone(), &format!("/rest/getStarred2.view?{auth}")).await;
    let starred = &body["subsonic-response"]["starred2"];
    assert_eq!(starred["song"][0]["title"], json!("First"));
    assert_eq!(starred["album"][0]["name"], json!("Discovery"));
    assert_eq!(starred["artist"][0]["name"], json!("Daft Punk"));

    let body = get(
        app.clone(),
        &format!("/rest/search3.view?{auth}&query=First"),
    )
    .await;
    let child = &body["subsonic-response"]["searchResult3"]["song"][0];
    assert!(
        child["starred"]
            .as_str()
            .is_some_and(|value| !value.is_empty())
    );
    assert_eq!(child["userRating"], json!(4));
    assert_eq!(child["playCount"], json!(2));
    assert_eq!(child["genre"], json!("Rock"));

    for list_type in ["starred", "frequent", "recent", "highest", "random"] {
        let body = get(
            app.clone(),
            &format!("/rest/getAlbumList2.view?{auth}&type={list_type}"),
        )
        .await;
        assert_eq!(
            body["subsonic-response"]["albumList2"]["album"][0]["name"],
            json!("Discovery"),
            "list type: {list_type}"
        );
    }

    let body = get(
        app.clone(),
        &format!("/rest/getAlbumList2.view?{auth}&type=byGenre&genre=Rock"),
    )
    .await;
    assert_eq!(
        body["subsonic-response"]["albumList2"]["album"][0]["name"],
        json!("Discovery")
    );

    let body = get(
        app.clone(),
        &format!("/rest/getAlbumList2.view?{auth}&type=byYear&fromYear=2000&toYear=2005"),
    )
    .await;
    assert_eq!(
        body["subsonic-response"]["albumList2"]["album"][0]["name"],
        json!("Discovery")
    );

    let (status, body) = get_status(
        app.clone(),
        &format!("/rest/getAlbumList2.view?{auth}&type=byYear&fromYear=2005"),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["subsonic-response"]["error"]["code"], json!(10));

    get(app.clone(), &format!("/rest/unstar.view?{auth}&id={song}")).await;
    let body = get(app, &format!("/rest/getStarred2.view?{auth}")).await;
    assert!(body["subsonic-response"]["starred2"]["song"].is_null());
}

#[tokio::test]
async fn subsonic_scan_status_reports_and_starts() {
    let seeded = seeded_app().await;
    let auth = format!("u=flynn&v=1.16.1&c=test-client&p={}", seeded.secret);

    let body = get(
        seeded.app.clone(),
        &format!("/rest/getScanStatus.view?{auth}"),
    )
    .await;
    assert_eq!(
        body["subsonic-response"]["scanStatus"]["scanning"],
        json!(false)
    );

    let body = get(seeded.app, &format!("/rest/startScan.view?{auth}")).await;
    assert_eq!(
        body["subsonic-response"]["scanStatus"]["scanning"],
        json!(true)
    );
}

#[tokio::test]
async fn navidrome_native_exposes_state_and_genres() {
    let seeded = seeded_app().await;
    let auth = format!("u=flynn&v=1.16.1&c=test-client&p={}", seeded.secret);

    get(
        seeded.app.clone(),
        &format!("/rest/star.view?{auth}&id={}", seeded.song),
    )
    .await;
    get(
        seeded.app.clone(),
        &format!("/rest/setRating.view?{auth}&id={}&rating=5", seeded.song),
    )
    .await;
    get(
        seeded.app.clone(),
        &format!("/rest/scrobble.view?{auth}&id={}", seeded.song),
    )
    .await;

    let (status, login) = post_login(seeded.app.clone(), "flynn", &seeded.secret).await;
    assert_eq!(status, StatusCode::OK);
    let token = login["token"].as_str().unwrap();

    let (status, song) = get_authed(
        seeded.app.clone(),
        &format!("/compat/navidrome/api/song/{}", seeded.song),
        token,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(song["starred"], json!(true));
    assert_eq!(song["rating"], json!(5));
    assert_eq!(song["playCount"], json!(1));

    let (status, genres) = get_authed(seeded.app, "/compat/navidrome/api/genre", token).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(genres[0]["name"], json!("Rock"));
}
