#![cfg(feature = "postgres-integration")]

use sqlx::PgPool;
use whio_core::features::music_library::domain::{
    AlbumMetadata, AlbumTrack, ArtistCredit, ArtistListFilter, ArtistSort, ProviderId,
    ProviderKind, SourceIdentity, SourceScope, StateTarget, TrackId, TrackMetadata,
};
use whio_core::features::music_library::history::ListenService;
use whio_core::features::music_library::test_support::{
    CatalogueWrite, LibraryRead, ListenStore, PlaylistStore, PostgresTrackRepository,
    RestoreQueueStore, SourceFileFacts, StateStore,
};
mod support;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use base64::Engine;
use base64::engine::general_purpose::STANDARD_NO_PAD;
use serde_json::{Value, json};
use tower::ServiceExt;
use uuid::Uuid;
use whio_core::{
    app::AppState,
    facades::SourceTags,
    features::identity::secrets::CredentialKey,
    features::identity::service::CredentialService,
    features::music_library::catalogue::{LibraryService, SearchService},
    features::music_library::library_state::LibraryStateService,
    features::music_library::playlists::PlaylistService,
    features::music_library::queue::{RestoreQueueSave, RestoreQueueService},
    features::playback::providers::{ProviderError, YtmAdapter},
    features::playback::{
        PlaybackActivityService, PlaybackProvider, PlaybackSource, ResolvedPlayback,
        StreamingService,
    },
    platform::request::RequestContext,
};

use support::credentials::credential_service_with_roles;
use support::{
    StorePorts, credential_service_with_known_app_password, router, router_with_completion,
    router_with_playlists, router_with_queue,
};

fn role_app(
    pool: PgPool,
    credential: Arc<whio_core::features::identity::service::CredentialService>,
) -> (
    axum::Router,
    Arc<whio_core::features::music_library::library::ScanService>,
) {
    let ports = StorePorts::new(pool.clone());
    let (search, library) = ports.bare_catalogue();
    let playback = ports.streaming(vec![
        Arc::new(YtmAdapter::disabled()) as Arc<dyn PlaybackProvider>
    ]);
    let scanner = whio_core::features::music_library::library::ScanService::new(
        ports.catalogue(),
        ports.sources(),
        vec![std::env::temp_dir()],
    );
    let app = whio_core::app::router(
        whio_core::app::AppState::new(credential, search, library, playback)
            .with_scanner(Arc::clone(&scanner)),
    );
    (app, scanner)
}

fn auth_query(username: &str, secret: &str) -> String {
    format!("u={username}&v=1.16.1&c=test-client&p={secret}")
}

fn app(
    pool: PgPool,
    credential: Arc<whio_core::features::identity::service::CredentialService>,
) -> axum::Router {
    let ports = StorePorts::new(pool.clone());
    let (search, library) = ports.bare_catalogue();
    let playback = ports.streaming(vec![
        Arc::new(YtmAdapter::disabled()) as Arc<dyn PlaybackProvider>
    ]);

    router(credential, search, library, playback)
}

async fn get(app: axum::Router, uri: &str) -> Value {
    let (status, body) = get_status(app, uri).await;
    assert_eq!(status, StatusCode::OK);

    body
}

async fn get_status(app: axum::Router, uri: &str) -> (StatusCode, Value) {
    get_status_bounded(app, uri, 8192).await
}

async fn get_status_bounded(app: axum::Router, uri: &str, limit: usize) -> (StatusCode, Value) {
    let request = Request::builder()
        .method("GET")
        .uri(uri)
        .header("x-request-id", "request-123")
        .body(Body::empty())
        .unwrap();

    let response = app.oneshot(request).await.unwrap();
    let status = response.status();
    let body = to_bytes(response.into_body(), limit).await.unwrap();
    (status, serde_json::from_slice(&body).unwrap())
}

async fn browse_app(
    pool: PgPool,
    credential: Arc<whio_core::features::identity::service::CredentialService>,
) -> axum::Router {
    use whio_core::features::music_library::domain::{
        AlbumMetadata, AlbumTrack, ArtistCredit, ProviderId, SourceIdentity, SourceScope,
        TrackMetadata,
    };
    use whio_core::features::music_library::test_support::CatalogueWrite;

    let local_source = |external_id: &str| {
        SourceIdentity::new(
            ProviderId::new("local".to_owned()).unwrap(),
            SourceScope::Named("/music".to_owned()),
            external_id.to_owned(),
        )
        .unwrap()
    };

    let repository = Arc::new(PostgresTrackRepository::new(pool.clone()));
    let album = repository
        .resolve_or_create_album(
            &AlbumMetadata::new("Discovery".to_owned())
                .unwrap()
                .with_album_artist(ArtistCredit::new("Daft Punk".to_owned(), None).unwrap()),
        )
        .await
        .unwrap();
    let second = repository
        .get_or_create(
            local_source("path-b"),
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
            local_source("path-a"),
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

    let ports = StorePorts { store: repository };
    let (search, library) = ports.bare_catalogue();
    let playback = ports.streaming(vec![
        Arc::new(YtmAdapter::disabled()) as Arc<dyn PlaybackProvider>
    ]);

    router(credential, search, library, playback)
}

#[sqlx::test]
async fn subsonic_browse_lists_albums_artists_and_songs(pool: PgPool) {
    let (service, secret) = credential_service_with_known_app_password();
    let app = browse_app(pool.clone(), service).await;
    let auth = format!("u=flynn&v=1.16.1&c=test-client&p={secret}");

    let body = get(
        app.clone(),
        &format!("/compat/subsonic/rest/getAlbumList2.view?{auth}&type=alphabeticalByName"),
    )
    .await;
    let album = &body["subsonic-response"]["albumList2"]["album"][0];
    assert_eq!(album["name"], json!("Discovery"));
    assert_eq!(album["artist"], json!("Daft Punk"));
    assert_eq!(album["songCount"], json!(2));
    assert_eq!(album["duration"], json!(3));
    let album_id = album["id"].as_str().unwrap();
    assert_eq!(album["coverArt"], json!(format!("al-{album_id}")));

    let body = get(
        app.clone(),
        &format!("/compat/subsonic/rest/getAlbum.view?{auth}&id={album_id}"),
    )
    .await;
    let songs = body["subsonic-response"]["album"]["song"]
        .as_array()
        .unwrap();
    assert_eq!(songs.len(), 2);
    assert_eq!(songs[0]["title"], json!("First"));
    assert_eq!(songs[0]["track"], json!(1));
    assert_eq!(songs[1]["title"], json!("Second"));

    let body = get(
        app.clone(),
        &format!("/compat/subsonic/rest/getArtists.view?{auth}"),
    )
    .await;
    let index = &body["subsonic-response"]["artists"]["index"][0];
    assert_eq!(index["name"], json!("D"));
    assert_eq!(index["artist"][0]["name"], json!("Daft Punk"));
    assert_eq!(index["artist"][0]["albumCount"], json!(1));
    let artist_id = index["artist"][0]["id"].as_str().unwrap();
    assert_eq!(
        index["artist"][0]["coverArt"],
        json!(format!("ar-{artist_id}"))
    );

    let body = get(
        app.clone(),
        &format!("/compat/subsonic/rest/getArtist.view?{auth}&id={artist_id}"),
    )
    .await;
    assert_eq!(
        body["subsonic-response"]["artist"]["album"][0]["name"],
        json!("Discovery")
    );

    let (status, body) = get_status(
        app.clone(),
        &format!("/compat/subsonic/rest/getAlbumList2.view?{auth}&type=random"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["subsonic-response"]["status"], json!("ok"));

    let (status, body) = get_status(
        app,
        &format!("/compat/subsonic/rest/getAlbumList2.view?{auth}&type=notAType"),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["subsonic-response"]["status"], json!("failed"));
}

async fn artist_scope_app(
    pool: PgPool,
    credential: Arc<whio_core::features::identity::service::CredentialService>,
) -> (axum::Router, Arc<PostgresTrackRepository>) {
    use whio_core::features::music_library::domain::{
        AlbumMetadata, AlbumTrack, ArtistCredit, CreditRole, ProviderId, SourceIdentity,
        SourceScope, TrackMetadata,
    };
    use whio_core::features::music_library::test_support::CatalogueWrite;

    let local_source = |external_id: &str| {
        SourceIdentity::new(
            ProviderId::new("local".to_owned()).unwrap(),
            SourceScope::Named("/music".to_owned()),
            external_id.to_owned(),
        )
        .unwrap()
    };
    let credit = |name: &str| ArtistCredit::new(name.to_owned(), None).unwrap();

    let repository = Arc::new(PostgresTrackRepository::new(pool.clone()));

    for (album_title, external, tracks) in [
        ("Discovery", "scope-a", 2usize),
        ("Homework", "scope-b", 1usize),
    ] {
        let album = repository
            .resolve_or_create_album(
                &AlbumMetadata::new(album_title.to_owned())
                    .unwrap()
                    .with_album_artist(credit("Daft Punk")),
            )
            .await
            .unwrap();

        for index in 0..tracks {
            let mut metadata = TrackMetadata::from_names(
                format!("{album_title} {index}"),
                vec!["Daft Punk".to_owned()],
                Some(1_000),
            )
            .unwrap();

            if album_title == "Discovery" && index == 0 {
                metadata = metadata.with_credits(vec![
                    ArtistCredit::new("Guy-Manuel".to_owned(), None)
                        .unwrap()
                        .with_role(CreditRole::Composer),
                ]);
            }

            let track = repository
                .get_or_create(local_source(&format!("{external}-{index}")), metadata)
                .await
                .unwrap();
            repository
                .set_album_track(album.id(), track.id(), &AlbumTrack::unplaced())
                .await
                .unwrap();
        }
    }

    let ports = StorePorts {
        store: Arc::clone(&repository),
    };
    let (search, library) = ports.bare_catalogue();
    let playback = ports.streaming(vec![
        Arc::new(YtmAdapter::disabled()) as Arc<dyn PlaybackProvider>
    ]);

    (router(credential, search, library, playback), repository)
}

#[sqlx::test]
async fn subsonic_artist_surfaces_are_album_artist_scoped(pool: PgPool) {
    let (service, secret) = credential_service_with_known_app_password();
    let (app, repository) = artist_scope_app(pool.clone(), service).await;
    let auth = format!("u=flynn&v=1.16.1&c=test-client&p={secret}");

    let all_roles = repository
        .list_artists(0, 50, ArtistSort::Name, &ArtistListFilter::new(), None)
        .await
        .unwrap()
        .into_iter()
        .map(|summary| summary.artist().name().to_owned())
        .collect::<Vec<_>>();

    assert_eq!(
        all_roles,
        vec!["Daft Punk".to_owned(), "Guy-Manuel".to_owned()],
        "the all-roles list keeps the composer-only artist, which is where the role selector belongs"
    );

    let body = get(
        app.clone(),
        &format!("/compat/subsonic/rest/getArtists.view?{auth}"),
    )
    .await;
    let index = &body["subsonic-response"]["artists"]["index"];
    let rows = index
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|entry| entry["artist"].as_array().unwrap())
        .map(|artist| artist["name"].as_str().unwrap().to_owned())
        .collect::<Vec<_>>();

    assert_eq!(
        rows,
        vec!["Daft Punk".to_owned()],
        "the index is the album-artist index, so a composer-only credit is not a folder"
    );

    let album_count = index[0]["artist"][0]["albumCount"].clone();

    assert_eq!(
        album_count,
        json!(2),
        "the index reports the album-artist membership count"
    );

    let body = get(
        app.clone(),
        &format!(
            "/compat/subsonic/rest/getArtist.view?{auth}&id={}",
            index[0]["artist"][0]["id"].as_str().unwrap()
        ),
    )
    .await;
    let detail = &body["subsonic-response"]["artist"];
    let albums = detail["album"].as_array().unwrap();
    let mut names = albums
        .iter()
        .map(|album| album["name"].as_str().unwrap().to_owned())
        .collect::<Vec<_>>();
    names.sort();

    assert_eq!(
        detail["albumCount"], album_count,
        "getArtist reports the count getArtists reports for the same artist"
    );
    assert_eq!(
        Some(albums.len()),
        detail["albumCount"]
            .as_u64()
            .map(|count| usize::try_from(count).unwrap()),
        "and it is the album list the detail returns"
    );
    assert_eq!(names, vec!["Discovery".to_owned(), "Homework".to_owned()]);
}

#[sqlx::test]
async fn navidrome_browse_lists_albums_artists_and_songs(pool: PgPool) {
    let (service, secret) = credential_service_with_known_app_password();
    let app = browse_app(pool.clone(), service).await;
    let (_, login) = post_login(app.clone(), "flynn", &secret).await;
    let token = login["token"].as_str().unwrap();

    let (status, headers, body) = get_authed(
        app.clone(),
        "/compat/navidrome/api/album?_start=0&_end=10",
        token,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers["x-total-count"], "1");
    assert_eq!(headers["access-control-expose-headers"], "x-total-count");
    assert_eq!(body[0]["name"], json!("Discovery"));
    assert_eq!(body[0]["albumArtist"], json!("Daft Punk"));
    assert_eq!(body[0]["songCount"], json!(2));
    assert!(body[0]["createdAt"].as_str().is_some_and(|v| !v.is_empty()));
    let album_id = body[0]["id"].as_str().unwrap();
    assert_eq!(body[0]["coverArtId"], json!(format!("al-{album_id}")));

    let (status, _, body) = get_authed(
        app.clone(),
        &format!("/compat/navidrome/api/album/{album_id}"),
        token,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["name"], json!("Discovery"));

    let (status, headers, body) = get_authed(
        app.clone(),
        &format!("/compat/navidrome/api/song?album_id={album_id}&_start=0&_end=0"),
        token,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers["x-total-count"], "2");
    assert_eq!(body.as_array().unwrap().len(), 2);
    assert_eq!(body[0]["title"], json!("First"));
    assert_eq!(body[0]["trackNumber"], json!(1));
    assert_eq!(body[0]["albumId"], json!(album_id));

    let (status, headers, body) =
        get_authed(app.clone(), "/compat/navidrome/api/artist", token).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers["x-total-count"], "1");
    assert_eq!(body[0]["name"], json!("Daft Punk"));
    assert_eq!(body[0]["albumCount"], json!(1));
    let artist_id = body[0]["id"].as_str().unwrap();
    assert_eq!(body[0]["coverArt"], json!(format!("ar-{artist_id}")));

    let (status, _, body) = get_authed(
        app.clone(),
        &format!("/compat/navidrome/api/artist/{artist_id}"),
        token,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["name"], json!("Daft Punk"));

    let (status, _, body) = get_authed(
        app.clone(),
        &format!("/compat/navidrome/api/album?artist_id={artist_id}"),
        token,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body.as_array().unwrap().len(), 1);

    let (status, _, body) = get_authed(
        app,
        "/compat/navidrome/api/album/01a07482-0f2d-7511-a4da-ac170d6f97bd",
        token,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body, json!({"error": "Not found."}));
}

#[sqlx::test]
async fn ping_answers_under_both_compat_roots(pool: PgPool) {
    let (service, secret) = credential_service_with_known_app_password();

    for root in ["/compat/navidrome/rest", "/compat/subsonic/rest"] {
        let body = get(
            app(pool.clone(), service.clone()),
            &format!("{root}/ping.view?u=flynn&v=1.16.1&c=test-client&p={secret}"),
        )
        .await;
        assert_eq!(
            body["subsonic-response"]["status"],
            json!("ok"),
            "root: {root}"
        );
    }
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
    let body = to_bytes(response.into_body(), 8192).await.unwrap();
    (status, serde_json::from_slice(&body).unwrap())
}

#[sqlx::test]
async fn login_with_known_app_password_returns_nd_payload(pool: PgPool) {
    let (service, secret) = credential_service_with_known_app_password();
    let app = app(pool.clone(), service);

    let (status, body) = post_login(app.clone(), "flynn", &secret).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["name"], json!("Flynn"));
    assert_eq!(body["username"], json!("flynn"));
    assert_eq!(body["isAdmin"], json!(false));
    for field in ["id", "token", "subsonicSalt", "subsonicToken"] {
        assert!(
            body[field].as_str().is_some_and(|v| !v.is_empty()),
            "field: {field}"
        );
    }

    let salt = body["subsonicSalt"].as_str().unwrap();
    let token = body["subsonicToken"].as_str().unwrap();
    let ping = get(
        app,
        &format!(
            "/compat/navidrome/rest/ping.view?u=flynn&v=1.16.1&c=test-client&t={token}&s={salt}"
        ),
    )
    .await;
    assert_eq!(ping["subsonic-response"]["status"], json!("ok"));
}

#[sqlx::test]
async fn login_with_wrong_password_is_indistinguishable_from_unknown_user(pool: PgPool) {
    let (service, secret) = credential_service_with_known_app_password();

    for username in ["flynn", "nobody"] {
        let password = if username == "flynn" {
            "wrong"
        } else {
            &secret
        };
        let (status, body) =
            post_login(app(pool.clone(), service.clone()), username, password).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "user: {username}");
        assert_eq!(
            body,
            json!({"error": "Invalid username or password"}),
            "user: {username}"
        );
    }
}

async fn get_authed(
    app: axum::Router,
    uri: &str,
    token: &str,
) -> (StatusCode, axum::http::HeaderMap, Value) {
    let request = Request::builder()
        .method("GET")
        .uri(uri)
        .header("x-request-id", "request-123")
        .header("x-nd-authorization", format!("Bearer {token}"))
        .body(Body::empty())
        .unwrap();

    let response = app.oneshot(request).await.unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let body = to_bytes(response.into_body(), 8192).await.unwrap();
    (status, headers, serde_json::from_slice(&body).unwrap())
}

async fn store_local_track(pool: &PgPool, external_id: &str, title: &str, artist: &str) {
    let source = SourceIdentity::new(
        ProviderId::new("local".to_owned()).expect("valid provider"),
        SourceScope::Named("/music".to_owned()),
        external_id.to_owned(),
    )
    .expect("valid source");

    PostgresTrackRepository::new(pool.clone())
        .get_or_create(
            source,
            TrackMetadata::from_names(title.to_owned(), vec![artist.to_owned()], Some(200_000))
                .expect("valid metadata"),
        )
        .await
        .expect("a stored track");
}

#[sqlx::test]
async fn song_list_rejects_missing_and_forged_tokens(pool: PgPool) {
    let (service, _) = credential_service_with_known_app_password();

    let request = Request::builder()
        .method("GET")
        .uri("/compat/navidrome/api/song")
        .header("x-request-id", "request-123")
        .body(Body::empty())
        .unwrap();
    let response = app(pool.clone(), service.clone())
        .oneshot(request)
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

    let (status, _, body) = get_authed(
        app(pool.clone(), service),
        "/compat/navidrome/api/song",
        "forged",
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body, json!({"error": "Not authenticated"}));
}

#[sqlx::test]
async fn song_list_returns_mapped_tracks_for_a_login_token(pool: PgPool) {
    use support::services;

    let (service, secret) = credential_service_with_known_app_password();
    store_local_track(&pool, "path-a", "song", "artist").await;
    let (search, library, playback) = services(pool.clone(), Vec::new());
    let app = router(service.clone(), search, library, playback);
    let (_, login) = post_login(app.clone(), "flynn", &secret).await;
    let token = login["token"].as_str().unwrap();

    let (status, headers, body) = get_authed(
        app.clone(),
        "/compat/navidrome/api/song?title=song&_start=0&_end=9",
        token,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body.as_array().unwrap().len(), 1);
    assert_eq!(body[0]["title"], json!("song"));
    assert_eq!(body[0]["artist"], json!("artist"));
    assert_eq!(body[0]["duration"], json!(200.0));
    assert!(body[0]["id"].as_str().is_some_and(|v| !v.is_empty()));
    assert_eq!(headers["x-total-count"], "1");

    let (status, headers, body) = get_authed(app, "/compat/navidrome/api/song", token).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body.as_array().unwrap().len(), 1);
    assert_eq!(body[0]["title"], json!("song"));
    assert_eq!(headers["x-total-count"], "1");
}

#[sqlx::test]
async fn source_tags_label_the_artist_in_both_song_surfaces(pool: PgPool) {
    use support::{router_with_source_tags, services};
    use whio_core::{
        facades::SourceTags,
        features::music_library::catalogue::CatalogueCandidate,
        features::music_library::domain::{
            ProviderId, SourceIdentity, SourceScope, TrackId, TrackMetadata,
        },
    };

    let (service, secret) = support::credentials::database_credential_service(pool.clone()).await;
    let candidate = CatalogueCandidate::new(
        SourceIdentity::new(
            ProviderId::new("youtube_music".to_owned()).expect("valid provider"),
            SourceScope::Global,
            "vid-1".to_owned(),
        )
        .expect("valid source"),
        TrackMetadata::from_names("song".to_owned(), vec!["artist".to_owned()], Some(200_000))
            .expect("valid metadata"),
    );
    let (search, library, playback) = services(pool.clone(), vec![Ok(vec![candidate])]);
    let app = router_with_source_tags(service, search, library, playback, SourceTags::ArtistPrefix);
    let (_, login) = post_login(app.clone(), "flynn", &secret).await;
    let token = login["token"].as_str().unwrap();
    let user: uuid::Uuid = login["id"].as_str().unwrap().parse().unwrap();

    let search = get(
        app.clone(),
        &format!(
            "/compat/subsonic/rest/search3.view?u=flynn&v=1.16.1&c=test-client&p={secret}&query=song"
        ),
    )
    .await;
    assert_eq!(
        search["subsonic-response"]["searchResult3"]["song"][0]["artist"],
        json!("[YTM] artist")
    );
    let track_id: TrackId = search["subsonic-response"]["searchResult3"]["song"][0]["id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    PostgresTrackRepository::new(pool.clone())
        .set_rating(user, StateTarget::Track(&track_id), Some(3))
        .await
        .unwrap();

    let (status, _, body) = get_authed(
        app,
        "/compat/navidrome/api/song?title=song&_start=0&_end=9",
        token,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body[0]["artist"], json!("[YTM] artist"));
}

#[sqlx::test]
async fn source_tags_reach_an_album_track_listing(pool: PgPool) {
    use support::router_with_source_tags;
    use whio_core::{
        facades::SourceTags,
        features::music_library::domain::{
            AlbumMetadata, AlbumTrack, ArtistCredit, ProviderId, SourceIdentity, SourceScope,
            TrackMetadata,
        },
        features::music_library::test_support::CatalogueWrite,
    };

    let (service, secret) = credential_service_with_known_app_password();
    let repository = Arc::new(PostgresTrackRepository::new(pool.clone()));
    let album = repository
        .resolve_or_create_album(
            &AlbumMetadata::new("Discovery".to_owned())
                .unwrap()
                .with_album_artist(ArtistCredit::new("Daft Punk".to_owned(), None).unwrap()),
        )
        .await
        .unwrap();
    let track = repository
        .get_or_create(
            SourceIdentity::new(
                ProviderId::new("youtube_music".to_owned()).unwrap(),
                SourceScope::Global,
                "vid-1".to_owned(),
            )
            .unwrap(),
            TrackMetadata::from_names("song".to_owned(), vec!["artist".to_owned()], Some(200_000))
                .unwrap(),
        )
        .await
        .unwrap();
    repository
        .set_album_track(album.id(), track.id(), &AlbumTrack::unplaced())
        .await
        .unwrap();

    let ports = StorePorts { store: repository };
    let (search, library) = ports.bare_catalogue();
    let playback = ports.streaming(vec![]);
    let app = router_with_source_tags(service, search, library, playback, SourceTags::ArtistPrefix);

    let auth = format!("u=flynn&v=1.16.1&c=test-client&p={secret}");
    let body = get(
        app.clone(),
        &format!(
            "/compat/subsonic/rest/getAlbum.view?{auth}&id={}",
            album.id()
        ),
    )
    .await;
    assert_eq!(
        body["subsonic-response"]["album"]["song"][0]["artist"],
        json!("[YTM] artist")
    );

    let (_, login) = post_login(app.clone(), "flynn", &secret).await;
    let token = login["token"].as_str().unwrap();
    let (status, _, body) = get_authed(
        app,
        &format!(
            "/compat/navidrome/api/song?album_id={}&_start=0&_end=9",
            album.id()
        ),
        token,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body[0]["artist"], json!("[YTM] artist"));
}

#[sqlx::test]
async fn opening_a_partial_provider_album_completes_it_over_the_facade(pool: PgPool) {
    use async_trait::async_trait;
    use whio_core::{
        features::music_library::availability::ProviderRegistry,
        features::music_library::catalogue::ProviderError,
        features::music_library::catalogue::{
            CatalogueCandidate, CatalogueProvider, CatalogueSearch, CompletionService,
            FetchedAlbum, LibraryService, SearchService,
        },
        features::music_library::domain::{
            AlbumMetadata, AlbumTrack, ArtistCredit, ProviderId, SourceIdentity, SourceScope,
            TrackMetadata,
        },
        features::music_library::test_support::CatalogueWrite,
        platform::request::RequestContext,
    };

    fn provider_source(external_id: &str) -> SourceIdentity {
        SourceIdentity::new(
            ProviderId::new("youtube_music".to_owned()).unwrap(),
            SourceScope::Global,
            external_id.to_owned(),
        )
        .unwrap()
    }

    fn album_metadata() -> AlbumMetadata {
        AlbumMetadata::new("SOUR".to_owned())
            .unwrap()
            .with_album_artist(ArtistCredit::new("Olivia Rodrigo".to_owned(), None).unwrap())
            .with_provider(Some(provider_source("MPREb_album")))
    }

    fn candidate(external_id: &str, title: &str) -> CatalogueCandidate {
        let metadata = TrackMetadata::from_names(
            title.to_owned(),
            vec!["Olivia Rodrigo".to_owned()],
            Some(1_000),
        )
        .unwrap();

        CatalogueCandidate::new(provider_source(external_id), metadata)
            .with_album(Some(album_metadata()))
    }

    struct AlbumProvider {
        candidates: Vec<CatalogueCandidate>,
    }

    #[async_trait]
    impl CatalogueProvider for AlbumProvider {
        async fn search(
            &self,
            _search: &CatalogueSearch,
            _user: Option<&uuid::Uuid>,
            _context: &RequestContext,
        ) -> Result<Vec<CatalogueCandidate>, ProviderError> {
            Ok(Vec::new())
        }

        async fn fetch_album(
            &self,
            _source: &SourceIdentity,
            _context: &RequestContext,
        ) -> Result<FetchedAlbum, ProviderError> {
            Ok(FetchedAlbum::new(self.candidates.clone(), Some(3)))
        }
    }

    let (service, secret) = credential_service_with_known_app_password();
    let ports = StorePorts::new(pool.clone());
    let album = ports
        .store
        .resolve_or_create_album(&album_metadata())
        .await
        .unwrap();
    let partial = ports
        .store
        .get_or_create(
            provider_source("vid-1"),
            TrackMetadata::from_names(
                "brutal".to_owned(),
                vec!["Olivia Rodrigo".to_owned()],
                Some(1_000),
            )
            .unwrap(),
        )
        .await
        .unwrap();
    ports
        .store
        .set_album_source(album.id(), &provider_source("MPREb_album"))
        .await
        .unwrap();
    ports
        .store
        .set_album_track(album.id(), partial.id(), &AlbumTrack::unplaced())
        .await
        .unwrap();

    let provider = Arc::new(AlbumProvider {
        candidates: vec![
            candidate("vid-2", "traitor"),
            candidate("vid-3", "drivers license"),
        ],
    });
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
    let completion = Arc::new(
        CompletionService::new(
            Arc::clone(&library),
            ports.catalogue(),
            ports.sources(),
            vec![provider],
        )
        .with_registry(ProviderRegistry::with_enabled(["youtube_music"])),
    );
    let playback = ports.streaming(vec![]);
    let app = router_with_completion(service, search, library, completion, playback);

    let auth = format!("u=flynn&v=1.16.1&c=test-client&p={secret}");
    let body = get(
        app,
        &format!(
            "/compat/subsonic/rest/getAlbum.view?{auth}&id={}",
            album.id()
        ),
    )
    .await;

    let songs = body["subsonic-response"]["album"]["song"]
        .as_array()
        .unwrap();
    assert_eq!(
        songs.len(),
        3,
        "opening a partial provider album fetches its full tracklist"
    );
}

#[sqlx::test]
async fn subsonic_playlist_verbs_round_trip(pool: PgPool) {
    use whio_core::{
        features::music_library::catalogue::{LibraryService, SearchService},
        features::music_library::domain::{ProviderId, SourceIdentity, SourceScope, TrackMetadata},
        features::music_library::playlists::PlaylistService,
        features::music_library::test_support::{CatalogueWrite, PlaylistStore},
    };

    fn source(provider: &str, scope: SourceScope, external_id: &str) -> SourceIdentity {
        SourceIdentity::new(
            ProviderId::new(provider.to_owned()).unwrap(),
            scope,
            external_id.to_owned(),
        )
        .unwrap()
    }

    let (service, secret) = support::credentials::database_credential_service(pool.clone()).await;
    let ports = StorePorts::new(pool.clone());
    let owned = ports
        .store
        .get_or_create(
            source("local", SourceScope::Named("/music".to_owned()), "path-a"),
            TrackMetadata::from_names("Owned".to_owned(), vec!["Artist".to_owned()], Some(1_000))
                .unwrap(),
        )
        .await
        .unwrap();
    let discovered = ports
        .store
        .get_or_create(
            source("youtube_music", SourceScope::Global, "vid-1"),
            TrackMetadata::from_names(
                "Discovered".to_owned(),
                vec!["Artist".to_owned()],
                Some(1_000),
            )
            .unwrap(),
        )
        .await
        .unwrap();

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
    let playlists = Arc::new(PlaylistService::new(
        Arc::clone(&ports.store) as Arc<dyn PlaylistStore>,
        Arc::clone(&library),
    ));
    let playback = ports.streaming(vec![]);
    let app = router_with_playlists(service, search, library, playlists, playback);

    let auth = format!("u=flynn&v=1.16.1&c=test-client&p={secret}");

    let body = get(
        app.clone(),
        &format!(
            "/compat/subsonic/rest/createPlaylist.view?{auth}&name=Trip&songId={}&songId={}",
            owned.id(),
            discovered.id()
        ),
    )
    .await;
    let playlist = &body["subsonic-response"]["playlist"];
    assert_eq!(playlist["name"], json!("Trip"));
    assert_eq!(playlist["songCount"], json!(2));
    let playlist_id = playlist["id"].as_str().unwrap();
    assert_eq!(playlist["entry"].as_array().unwrap().len(), 2);

    let body = get(
        app.clone(),
        &format!("/compat/subsonic/rest/getPlaylists.view?{auth}"),
    )
    .await;
    let listed = body["subsonic-response"]["playlists"]["playlist"]
        .as_array()
        .unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0]["songCount"], json!(2));

    let body = get(
        app.clone(),
        &format!("/compat/subsonic/rest/updatePlaylist.view?{auth}&playlistId={playlist_id}&songIndexToRemove=0"),
    )
    .await;
    assert_eq!(body["subsonic-response"]["status"], json!("ok"));

    let body = get(
        app.clone(),
        &format!("/compat/subsonic/rest/getPlaylist.view?{auth}&id={playlist_id}"),
    )
    .await;
    let entries = body["subsonic-response"]["playlist"]["entry"]
        .as_array()
        .unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(
        entries[0]["title"],
        json!("Discovered"),
        "a provider track is a legal playlist entry"
    );

    let body = get(
        app.clone(),
        &format!("/compat/subsonic/rest/deletePlaylist.view?{auth}&id={playlist_id}"),
    )
    .await;
    assert_eq!(body["subsonic-response"]["status"], json!("ok"));

    let body = get(
        app,
        &format!("/compat/subsonic/rest/getPlaylists.view?{auth}"),
    )
    .await;
    assert!(
        body["subsonic-response"]["playlists"]
            .get("playlist")
            .is_none()
    );
}

struct PlaylistFixture {
    app: axum::Router,
    owner: String,
    stranger: String,
}

fn playlist_auth(username: &str, secret: &str) -> String {
    format!("u={username}&v=1.16.1&c=test-client&p={secret}")
}

fn playlist_route(route: &str, auth: &str, parameters: &str) -> String {
    let separator = if parameters.is_empty() { "" } else { "&" };

    format!("/compat/subsonic/rest/{route}.view?{auth}{separator}{parameters}")
}

async fn playlist_fixture(pool: PgPool) -> PlaylistFixture {
    let key = CredentialKey::from_base64(&STANDARD_NO_PAD.encode([11u8; 32])).unwrap();
    let (service, owner_secret) =
        support::credentials::database_credential_service(pool.clone()).await;
    let provisioning = whio_core::features::identity::provision(pool.clone());
    provisioning.create_user("other", "Other").await.unwrap();
    let (_, stranger_secret) = provisioning
        .mint_app_password(&key, "other", "integration")
        .await
        .unwrap();
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
    let playlists = Arc::new(PlaylistService::new(
        Arc::clone(&ports.store) as Arc<dyn PlaylistStore>,
        Arc::clone(&library),
    ));
    let playback = ports.streaming(vec![]);

    PlaylistFixture {
        app: router_with_playlists(service, search, library, playlists, playback),
        owner: playlist_auth("flynn", &owner_secret),
        stranger: playlist_auth("other", &stranger_secret),
    }
}

async fn playlist_song(pool: &PgPool, external_id: &str, title: &str) -> TrackId {
    let ports = StorePorts::new(pool.clone());

    ports
        .store
        .get_or_create(
            SourceIdentity::new(
                ProviderId::new("local".to_owned()).unwrap(),
                SourceScope::Named("/music".to_owned()),
                external_id.to_owned(),
            )
            .unwrap(),
            TrackMetadata::from_names(title.to_owned(), vec!["Artist".to_owned()], Some(1_000))
                .unwrap(),
        )
        .await
        .unwrap()
        .id()
        .clone()
}

fn playlist_of(body: &Value) -> &Value {
    &body["subsonic-response"]["playlist"]
}

fn playlist_id_of(body: &Value) -> String {
    playlist_of(body)["id"].as_str().unwrap().to_owned()
}

fn entry_titles(body: &Value) -> Vec<String> {
    playlist_of(body)["entry"]
        .as_array()
        .map(|entries| {
            entries
                .iter()
                .map(|entry| entry["title"].as_str().unwrap().to_owned())
                .collect()
        })
        .unwrap_or_default()
}

fn song_ids(body: &Value) -> Vec<String> {
    playlist_of(body)["entry"]
        .as_array()
        .map(|entries| {
            entries
                .iter()
                .map(|entry| entry["id"].as_str().unwrap().to_owned())
                .collect()
        })
        .unwrap_or_default()
}

async fn read_playlist(app: axum::Router, auth: &str, id: &str) -> Value {
    get(
        app,
        &playlist_route("getPlaylist", auth, &format!("id={id}")),
    )
    .await
}

async fn edit_playlist(app: axum::Router, auth: &str, parameters: &str) -> Value {
    get(app, &playlist_route("updatePlaylist", auth, parameters)).await
}

#[sqlx::test]
async fn subsonic_create_with_an_id_replaces_and_preserves_metadata(pool: PgPool) {
    let fixture = playlist_fixture(pool.clone()).await;
    let first = playlist_song(&pool, "path-first", "First").await;
    let second = playlist_song(&pool, "path-second", "Second").await;
    let third = playlist_song(&pool, "path-third", "Third").await;

    let body = get(
        fixture.app.clone(),
        &playlist_route(
            "createPlaylist",
            &fixture.owner,
            &format!("name=Trip&songId={first}&songId={second}"),
        ),
    )
    .await;
    let id = playlist_id_of(&body);
    assert_eq!(entry_titles(&body), vec!["First", "Second"]);

    let body = edit_playlist(
        fixture.app.clone(),
        &fixture.owner,
        &format!("playlistId={id}&public=true&comment=hello"),
    )
    .await;
    assert_eq!(body["subsonic-response"]["status"], json!("ok"));
    let body = get(
        fixture.app.clone(),
        &playlist_route(
            "createPlaylist",
            &fixture.owner,
            &format!("playlistId={id}&songId={third}"),
        ),
    )
    .await;
    assert_eq!(
        body["subsonic-response"]["status"],
        json!("ok"),
        "an existing-id create is accepted"
    );
    assert_eq!(
        entry_titles(&body),
        vec!["Third"],
        "the entries are replaced rather than appended"
    );

    let body = read_playlist(fixture.app, &fixture.owner, &id).await;
    assert_eq!(
        entry_titles(&body),
        vec!["Third"],
        "the replaced entries are gone from the stored playlist"
    );
    assert_eq!(
        playlist_of(&body)["name"],
        json!("Trip"),
        "an omitted name is kept"
    );
    assert_eq!(
        playlist_of(&body)["comment"],
        json!("hello"),
        "the comment is not cleared"
    );
    assert_eq!(
        playlist_of(&body)["public"],
        json!(true),
        "the visibility is not forced private"
    );
    assert_eq!(playlist_of(&body)["songCount"], json!(1));
}

#[sqlx::test]
async fn subsonic_update_applies_only_present_fields(pool: PgPool) {
    let fixture = playlist_fixture(pool.clone()).await;
    let first = playlist_song(&pool, "path-first", "First").await;
    let second = playlist_song(&pool, "path-second", "Second").await;

    let body = get(
        fixture.app.clone(),
        &playlist_route(
            "createPlaylist",
            &fixture.owner,
            &format!("name=Trip&songId={first}&songId={second}"),
        ),
    )
    .await;
    let id = playlist_id_of(&body);
    let body = edit_playlist(
        fixture.app.clone(),
        &fixture.owner,
        &format!("playlistId={id}&comment=hello&public=true"),
    )
    .await;
    assert_eq!(body["subsonic-response"]["status"], json!("ok"));
    let body = read_playlist(fixture.app.clone(), &fixture.owner, &id).await;
    assert_eq!(playlist_of(&body)["comment"], json!("hello"));
    assert_eq!(
        playlist_of(&body)["public"],
        json!(true),
        "the fixture is public, so an omitted public flag is observable"
    );

    let body = edit_playlist(
        fixture.app.clone(),
        &fixture.owner,
        &format!("playlistId={id}&name=Tripped"),
    )
    .await;
    assert_eq!(body["subsonic-response"]["status"], json!("ok"));
    let body = read_playlist(fixture.app.clone(), &fixture.owner, &id).await;
    assert_eq!(playlist_of(&body)["name"], json!("Tripped"));
    assert_eq!(
        playlist_of(&body)["comment"],
        json!("hello"),
        "an omitted comment is left alone"
    );
    assert_eq!(
        playlist_of(&body)["public"],
        json!(true),
        "an omitted public flag is left alone"
    );

    let body = edit_playlist(
        fixture.app.clone(),
        &fixture.owner,
        &format!("playlistId={id}&comment="),
    )
    .await;
    assert_eq!(body["subsonic-response"]["status"], json!("ok"));
    let body = read_playlist(fixture.app.clone(), &fixture.owner, &id).await;
    assert_eq!(
        playlist_of(&body)["comment"],
        Value::Null,
        "an empty comment clears it"
    );
    assert_eq!(
        playlist_of(&body)["name"],
        json!("Tripped"),
        "clearing the comment leaves the name"
    );
    assert_eq!(
        playlist_of(&body)["public"],
        json!(true),
        "clearing the comment leaves the visibility"
    );

    let (status, body) = get_status(
        fixture.app.clone(),
        &playlist_route(
            "updatePlaylist",
            &fixture.owner,
            &format!("playlistId={id}&name="),
        ),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "an empty name cannot clear a required field"
    );
    assert_eq!(body["subsonic-response"]["error"]["code"], json!(0));
    let body = read_playlist(fixture.app.clone(), &fixture.owner, &id).await;
    assert_eq!(
        playlist_of(&body)["name"],
        json!("Tripped"),
        "the rejected edit changed nothing"
    );
    assert_eq!(playlist_of(&body)["public"], json!(true));

    let body = edit_playlist(
        fixture.app.clone(),
        &fixture.owner,
        &format!("playlistId={id}&public=false"),
    )
    .await;
    assert_eq!(body["subsonic-response"]["status"], json!("ok"));
    let body = read_playlist(fixture.app, &fixture.owner, &id).await;
    assert_eq!(
        playlist_of(&body)["public"],
        json!(false),
        "a present public flag is applied"
    );
    assert_eq!(playlist_of(&body)["name"], json!("Tripped"));
    assert_eq!(playlist_of(&body)["comment"], Value::Null);
}

#[sqlx::test]
async fn subsonic_remove_index_is_zero_based(pool: PgPool) {
    let fixture = playlist_fixture(pool.clone()).await;
    let first = playlist_song(&pool, "path-first", "First").await;
    let second = playlist_song(&pool, "path-second", "Second").await;
    let third = playlist_song(&pool, "path-third", "Third").await;

    let body = get(
        fixture.app.clone(),
        &playlist_route(
            "createPlaylist",
            &fixture.owner,
            &format!("name=Trip&songId={first}&songId={second}&songId={third}"),
        ),
    )
    .await;
    let id = playlist_id_of(&body);
    assert_eq!(entry_titles(&body), vec!["First", "Second", "Third"]);

    let body = edit_playlist(
        fixture.app.clone(),
        &fixture.owner,
        &format!("playlistId={id}&songIndexToRemove=0"),
    )
    .await;
    assert_eq!(body["subsonic-response"]["status"], json!("ok"));
    let body = read_playlist(fixture.app.clone(), &fixture.owner, &id).await;
    assert_eq!(
        entry_titles(&body),
        vec!["Second", "Third"],
        "index 0 removes the first entry"
    );

    let body = edit_playlist(
        fixture.app.clone(),
        &fixture.owner,
        &format!("playlistId={id}&songIndexToRemove=1"),
    )
    .await;
    assert_eq!(body["subsonic-response"]["status"], json!("ok"));
    let body = read_playlist(fixture.app.clone(), &fixture.owner, &id).await;
    assert_eq!(
        entry_titles(&body),
        vec!["Second"],
        "index n-1 removes the last entry, not the one a one-based index names"
    );

    let (status, body) = get_status(
        fixture.app.clone(),
        &playlist_route(
            "updatePlaylist",
            &fixture.owner,
            &format!("playlistId={id}&songIndexToRemove=1"),
        ),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "an index past the end is a client error, not a silent no-op"
    );
    assert_eq!(body["subsonic-response"]["error"]["code"], json!(0));
    let body = read_playlist(fixture.app.clone(), &fixture.owner, &id).await;
    assert_eq!(entry_titles(&body), vec!["Second"]);

    let (status, body) = get_status(
        fixture.app,
        &playlist_route(
            "updatePlaylist",
            &fixture.owner,
            &format!("playlistId={id}&songIndexToRemove=first"),
        ),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "an unparsable index is a client error, not a dropped parameter"
    );
    assert_eq!(body["subsonic-response"]["error"]["code"], json!(0));
}

#[sqlx::test]
async fn subsonic_add_and_remove_in_one_call_keeps_the_added_occurrence(pool: PgPool) {
    let fixture = playlist_fixture(pool.clone()).await;
    let first = playlist_song(&pool, "path-first", "First").await;
    let second = playlist_song(&pool, "path-second", "Second").await;
    let third = playlist_song(&pool, "path-third", "Third").await;

    let body = get(
        fixture.app.clone(),
        &playlist_route(
            "createPlaylist",
            &fixture.owner,
            &format!("name=Trip&songId={first}&songId={second}"),
        ),
    )
    .await;
    let id = playlist_id_of(&body);

    let body = edit_playlist(
        fixture.app.clone(),
        &fixture.owner,
        &format!("playlistId={id}&songIndexToRemove=0&songIdToAdd={first}"),
    )
    .await;
    assert_eq!(body["subsonic-response"]["status"], json!("ok"));
    let body = read_playlist(fixture.app.clone(), &fixture.owner, &id).await;
    assert_eq!(
        entry_titles(&body),
        vec!["Second", "First"],
        "the removal is resolved before the addition, so the added occurrence survives"
    );
    assert_eq!(
        song_ids(&body),
        vec![second.to_string(), first.to_string()],
        "the added occurrence is the one present"
    );

    let (status, body) = get_status(
        fixture.app.clone(),
        &playlist_route(
            "updatePlaylist",
            &fixture.owner,
            &format!("playlistId={id}&songIndexToRemove=2&songIdToAdd={third}"),
        ),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "index 2 is the position the addition would occupy, and the removal is resolved against the \
         snapshot without it: a snapshot extended with the pending addition would resolve it to the \
         freshly added occurrence and answer ok"
    );
    assert_eq!(body["subsonic-response"]["error"]["code"], json!(0));
    let body = read_playlist(fixture.app, &fixture.owner, &id).await;
    assert_eq!(
        entry_titles(&body),
        vec!["Second", "First"],
        "the failed edit left no partial result: the addition was not applied"
    );
}

#[sqlx::test]
async fn subsonic_repeated_tracks_are_distinct_entries(pool: PgPool) {
    let fixture = playlist_fixture(pool.clone()).await;
    let first = playlist_song(&pool, "path-first", "First").await;
    let second = playlist_song(&pool, "path-second", "Second").await;

    let body = get(
        fixture.app.clone(),
        &playlist_route(
            "createPlaylist",
            &fixture.owner,
            &format!("name=Trip&songId={first}&songId={first}&songId={second}"),
        ),
    )
    .await;
    let id = playlist_id_of(&body);
    assert_eq!(
        song_ids(&body),
        vec![first.to_string(), first.to_string(), second.to_string()],
        "the same song id twice is two entries"
    );
    assert_eq!(playlist_of(&body)["songCount"], json!(3));

    let body = edit_playlist(
        fixture.app.clone(),
        &fixture.owner,
        &format!("playlistId={id}&songIndexToRemove=0"),
    )
    .await;
    assert_eq!(body["subsonic-response"]["status"], json!("ok"));
    let body = read_playlist(fixture.app.clone(), &fixture.owner, &id).await;
    assert_eq!(
        song_ids(&body),
        vec![first.to_string(), second.to_string()],
        "removing one occurrence leaves the other"
    );

    let body = edit_playlist(
        fixture.app.clone(),
        &fixture.owner,
        &format!("playlistId={id}&songIdToAdd={first}"),
    )
    .await;
    assert_eq!(body["subsonic-response"]["status"], json!("ok"));
    let body = read_playlist(fixture.app, &fixture.owner, &id).await;
    assert_eq!(
        song_ids(&body),
        vec![first.to_string(), second.to_string(), first.to_string()],
        "adding a present track appends a further occurrence"
    );
}

#[sqlx::test]
async fn subsonic_visibility_and_ownership(pool: PgPool) {
    let fixture = playlist_fixture(pool.clone()).await;
    let first = playlist_song(&pool, "path-first", "First").await;
    let second = playlist_song(&pool, "path-second", "Second").await;

    let body = get(
        fixture.app.clone(),
        &playlist_route(
            "createPlaylist",
            &fixture.owner,
            &format!("name=Shared&songId={first}"),
        ),
    )
    .await;
    let shared = playlist_id_of(&body);
    let body = edit_playlist(
        fixture.app.clone(),
        &fixture.owner,
        &format!("playlistId={shared}&public=true"),
    )
    .await;
    assert_eq!(body["subsonic-response"]["status"], json!("ok"));
    let body = get(
        fixture.app.clone(),
        &playlist_route(
            "createPlaylist",
            &fixture.owner,
            &format!("name=Hidden&songId={second}"),
        ),
    )
    .await;
    let hidden = playlist_id_of(&body);

    let body = get(
        fixture.app.clone(),
        &playlist_route("getPlaylists", &fixture.stranger, ""),
    )
    .await;
    let listed = body["subsonic-response"]["playlists"]["playlist"]
        .as_array()
        .unwrap();
    assert_eq!(
        listed.len(),
        1,
        "another principal's private playlist is invisible"
    );
    assert_eq!(listed[0]["id"], json!(shared));
    assert_eq!(
        listed[0]["owner"],
        json!("flynn"),
        "a non-owner is told the real owner, not themselves"
    );

    let body = read_playlist(fixture.app.clone(), &fixture.stranger, &shared).await;
    assert_eq!(entry_titles(&body), vec!["First"]);
    assert_eq!(
        playlist_of(&body)["owner"],
        json!("flynn"),
        "the real owner is reported on the read"
    );

    let (status, body) = get_status(
        fixture.app.clone(),
        &playlist_route("getPlaylist", &fixture.stranger, &format!("id={hidden}")),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["subsonic-response"]["error"]["code"], json!(70));

    let (status, body) = get_status(
        fixture.app.clone(),
        &playlist_route(
            "updatePlaylist",
            &fixture.stranger,
            &format!("playlistId={shared}&name=Hijacked"),
        ),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "a public playlist is read-only for a non-owner"
    );
    assert_eq!(body["subsonic-response"]["error"]["code"], json!(50));

    let (status, body) = get_status(
        fixture.app.clone(),
        &playlist_route("deletePlaylist", &fixture.stranger, &format!("id={shared}")),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["subsonic-response"]["error"]["code"], json!(50));

    let (status, body) = get_status(
        fixture.app.clone(),
        &playlist_route(
            "createPlaylist",
            &fixture.stranger,
            &format!("playlistId={shared}&songId={second}"),
        ),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "replacing an existing playlist's entries is an edit"
    );
    assert_eq!(body["subsonic-response"]["error"]["code"], json!(50));

    let (status, body) = get_status(
        fixture.app.clone(),
        &playlist_route(
            "updatePlaylist",
            &fixture.stranger,
            &format!("playlistId={hidden}&name=Hijacked"),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body["subsonic-response"]["error"]["code"],
        json!(70),
        "a private playlist is not even visible"
    );

    let body = read_playlist(fixture.app.clone(), &fixture.owner, &shared).await;
    assert_eq!(entry_titles(&body), vec!["First"]);
    let body = read_playlist(fixture.app.clone(), &fixture.owner, &hidden).await;
    assert_eq!(entry_titles(&body), vec!["Second"]);

    let body = edit_playlist(
        fixture.app.clone(),
        &fixture.owner,
        &format!("playlistId={shared}&name=StillMine"),
    )
    .await;
    assert_eq!(
        body["subsonic-response"]["status"],
        json!("ok"),
        "public does not stop the owner editing"
    );
    let body = edit_playlist(
        fixture.app,
        &fixture.owner,
        &format!("playlistId={hidden}&name=AlsoMine"),
    )
    .await;
    assert_eq!(body["subsonic-response"]["status"], json!("ok"));
}

#[sqlx::test]
async fn subsonic_playlist_errors_are_the_existing_shapes(pool: PgPool) {
    let fixture = playlist_fixture(pool.clone()).await;
    let first = playlist_song(&pool, "path-first", "First").await;
    let unknown = Uuid::now_v7();

    for (route, parameters) in [
        ("getPlaylist", format!("id={unknown}")),
        (
            "updatePlaylist",
            format!("playlistId={unknown}&name=Hijacked"),
        ),
        ("deletePlaylist", format!("id={unknown}")),
        ("getPlaylist", "id=not-a-playlist".to_owned()),
        ("updatePlaylist", "playlistId=not-a-playlist".to_owned()),
        ("deletePlaylist", "id=not-a-playlist".to_owned()),
    ] {
        let (status, body) = get_status(
            fixture.app.clone(),
            &playlist_route(route, &fixture.owner, &parameters),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{route}: {parameters}");
        assert_eq!(body["subsonic-response"]["status"], json!("failed"));
        assert_eq!(
            body["subsonic-response"]["error"]["code"],
            json!(70),
            "{route}: {parameters}"
        );
        assert_eq!(
            body["subsonic-response"]["error"]["message"],
            json!("Playlist not found.")
        );
    }

    let body = get(
        fixture.app.clone(),
        &playlist_route(
            "createPlaylist",
            &fixture.owner,
            &format!("name=Shared&songId={first}"),
        ),
    )
    .await;
    let shared = playlist_id_of(&body);
    let body = edit_playlist(
        fixture.app.clone(),
        &fixture.owner,
        &format!("playlistId={shared}&public=true"),
    )
    .await;
    assert_eq!(body["subsonic-response"]["status"], json!("ok"));

    let (status, body) = get_status(
        fixture.app.clone(),
        &playlist_route(
            "updatePlaylist",
            &fixture.stranger,
            &format!("playlistId={shared}&name=Hijacked"),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["subsonic-response"]["status"], json!("failed"));
    assert_eq!(body["subsonic-response"]["error"]["code"], json!(50));
    assert_eq!(
        body["subsonic-response"]["error"]["message"],
        json!("Not authorized.")
    );

    let (status, body) = get_status(
        fixture.app.clone(),
        &playlist_route("deletePlaylist", &fixture.stranger, &format!("id={shared}")),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["subsonic-response"]["error"]["code"], json!(50));

    let body = read_playlist(fixture.app, &fixture.owner, &shared).await;
    assert_eq!(
        playlist_of(&body)["name"],
        json!("Shared"),
        "the refused edits changed nothing"
    );
}

#[sqlx::test]
async fn subsonic_playlist_client_errors_name_the_parameter(pool: PgPool) {
    let fixture = playlist_fixture(pool.clone()).await;
    let first = playlist_song(&pool, "path-first", "First").await;

    let (status, body) = get_status(
        fixture.app.clone(),
        &playlist_route("createPlaylist", &fixture.owner, &format!("songId={first}")),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["subsonic-response"]["status"], json!("failed"));
    assert_eq!(
        body["subsonic-response"]["error"]["code"],
        json!(10),
        "a create without a name or an id is the missing-parameter shape"
    );

    let (status, body) = get_status(
        fixture.app.clone(),
        &playlist_route(
            "createPlaylist",
            &fixture.owner,
            "playlistId=not-a-playlist&songId=whatever",
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body["subsonic-response"]["error"]["code"],
        json!(70),
        "a malformed playlist id is the not-found shape, not a silent create"
    );

    let body = get(
        fixture.app.clone(),
        &playlist_route(
            "createPlaylist",
            &fixture.owner,
            &format!("name=Trip&songId={first}"),
        ),
    )
    .await;
    let id = playlist_id_of(&body);

    let (status, body) = get_status(
        fixture.app.clone(),
        &playlist_route(
            "updatePlaylist",
            &fixture.owner,
            &format!("playlistId={id}&songIdToAdd=not-a-track"),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["subsonic-response"]["error"]["code"], json!(0));
    assert_eq!(
        body["subsonic-response"]["error"]["message"],
        json!("Invalid track ID.")
    );

    let unknown = Uuid::now_v7();
    let (status, body) = get_status(
        fixture.app.clone(),
        &playlist_route(
            "updatePlaylist",
            &fixture.owner,
            &format!("playlistId={id}&songIdToAdd={unknown}"),
        ),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "an unknown track is a client error rather than a server failure"
    );
    assert_eq!(body["subsonic-response"]["error"]["code"], json!(0));

    let body = read_playlist(fixture.app, &fixture.owner, &id).await;
    assert_eq!(
        entry_titles(&body),
        vec!["First"],
        "the rejected additions changed nothing"
    );
}

#[sqlx::test]
async fn subsonic_edit_without_a_revision_is_last_write_wins(pool: PgPool) {
    let fixture = playlist_fixture(pool.clone()).await;
    let first = playlist_song(&pool, "path-first", "First").await;
    let second = playlist_song(&pool, "path-second", "Second").await;
    let third = playlist_song(&pool, "path-third", "Third").await;
    let fourth = playlist_song(&pool, "path-fourth", "Fourth").await;

    let body = get(
        fixture.app.clone(),
        &playlist_route(
            "createPlaylist",
            &fixture.owner,
            &format!("name=Trip&songId={first}&songId={second}&songId={third}&songId={fourth}"),
        ),
    )
    .await;
    let id = playlist_id_of(&body);

    let body = edit_playlist(
        fixture.app.clone(),
        &fixture.owner,
        &format!("playlistId={id}&songIndexToRemove=0"),
    )
    .await;
    assert_eq!(body["subsonic-response"]["status"], json!("ok"));
    let body = read_playlist(fixture.app.clone(), &fixture.owner, &id).await;
    assert_eq!(entry_titles(&body), vec!["Second", "Third", "Fourth"]);

    let body = edit_playlist(
        fixture.app.clone(),
        &fixture.owner,
        &format!("playlistId={id}&songIndexToRemove=2"),
    )
    .await;
    assert_eq!(
        body["subsonic-response"]["status"],
        json!("ok"),
        "the compat wire carries no revision, so a stale edit is not a conflict"
    );
    let body = read_playlist(fixture.app, &fixture.owner, &id).await;
    assert_eq!(
        entry_titles(&body),
        vec!["Second", "Third"],
        "the stale index addressed a different occurrence than the caller read"
    );
}

#[sqlx::test]
async fn navidrome_playlist_surface_lists_its_songs(pool: PgPool) {
    use whio_core::{
        features::music_library::catalogue::{LibraryService, SearchService},
        features::music_library::domain::{ProviderId, SourceIdentity, SourceScope, TrackMetadata},
        features::music_library::playlists::PlaylistService,
        features::music_library::test_support::{CatalogueWrite, PlaylistStore},
    };

    let (service, secret) = support::credentials::database_credential_service(pool.clone()).await;
    let ports = StorePorts::new(pool.clone());
    let track = ports
        .store
        .get_or_create(
            SourceIdentity::new(
                ProviderId::new("local".to_owned()).unwrap(),
                SourceScope::Named("/music".to_owned()),
                "path-a".to_owned(),
            )
            .unwrap(),
            TrackMetadata::from_names("Owned".to_owned(), vec!["Artist".to_owned()], Some(1_000))
                .unwrap(),
        )
        .await
        .unwrap();

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
    let playlists = Arc::new(PlaylistService::new(
        Arc::clone(&ports.store) as Arc<dyn PlaylistStore>,
        Arc::clone(&library),
    ));
    let playback = ports.streaming(vec![]);
    let app = router_with_playlists(service, search, library, playlists, playback);

    let auth = format!("u=flynn&v=1.16.1&c=test-client&p={secret}");
    let body = get(
        app.clone(),
        &format!(
            "/compat/subsonic/rest/createPlaylist.view?{auth}&name=Trip&songId={}",
            track.id()
        ),
    )
    .await;
    let playlist_id = body["subsonic-response"]["playlist"]["id"]
        .as_str()
        .unwrap()
        .to_owned();

    let (_, login) = post_login(app.clone(), "flynn", &secret).await;
    let token = login["token"].as_str().unwrap();

    let (status, _, body) = get_authed(app.clone(), "/compat/navidrome/api/playlist", token).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body.as_array().unwrap().len(), 1);
    assert_eq!(body[0]["name"], json!("Trip"));
    assert_eq!(body[0]["songCount"], json!(1));
    assert_eq!(body[0]["songs"][0]["title"], json!("Owned"));

    let (status, _, body) = get_authed(
        app,
        &format!("/compat/navidrome/api/playlist/{playlist_id}"),
        token,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["songs"].as_array().unwrap().len(), 1);
    assert_eq!(body["songs"][0]["artist"], json!("Artist"));
}

#[sqlx::test]
async fn song_detail_returns_the_searched_song(pool: PgPool) {
    use support::services;

    let (service, secret) = credential_service_with_known_app_password();
    store_local_track(&pool, "path-a", "song", "artist").await;
    let (search, library, playback) = services(pool.clone(), Vec::new());
    let app = router(service.clone(), search, library, playback);
    let (_, login) = post_login(app.clone(), "flynn", &secret).await;
    let token = login["token"].as_str().unwrap();

    let (status, _, body) = get_authed(
        app.clone(),
        "/compat/navidrome/api/song?title=song&_start=0&_end=9",
        token,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let id = body[0]["id"].as_str().unwrap();

    let (status, _, body) = get_authed(
        app.clone(),
        &format!("/compat/navidrome/api/song/{id}"),
        token,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["id"], json!(id));
    assert_eq!(body["title"], json!("song"));
    assert_eq!(body["artist"], json!("artist"));
    assert_eq!(body["duration"], json!(200.0));

    let (status, _, body) = get_authed(
        app.clone(),
        "/compat/navidrome/api/song/01a07482-0f2d-7511-a4da-ac170d6f97bd",
        token,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body, json!({"error": "Not found."}));

    let (status, _, body) =
        get_authed(app.clone(), "/compat/navidrome/api/song/not-a-uuid", token).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body, json!({"error": "Not found."}));

    let (status, _, body) =
        get_authed(app, &format!("/compat/navidrome/api/song/{id}"), "forged").await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body, json!({"error": "Not authenticated"}));
}

#[sqlx::test]
async fn compat_answers_cors_preflight(pool: PgPool) {
    let (service, _) = credential_service_with_known_app_password();

    for uri in [
        "/compat/navidrome/auth/login",
        "/compat/navidrome/api/song",
        "/compat/subsonic/rest/ping.view",
    ] {
        let request = Request::builder()
            .method("OPTIONS")
            .uri(uri)
            .header("origin", "http://localhost:3000")
            .header("access-control-request-method", "POST")
            .body(Body::empty())
            .unwrap();
        let response = app(pool.clone(), service.clone())
            .oneshot(request)
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT, "uri: {uri}");
        assert_eq!(
            response.headers()["access-control-allow-origin"],
            "*",
            "uri: {uri}"
        );
    }
}

#[sqlx::test]
async fn real_client_view_suffix_reaches_handlers(pool: PgPool) {
    let (service, secret) = credential_service_with_known_app_password();
    let app = app(pool.clone(), service);

    let ping = get(
        app.clone(),
        &format!("/compat/navidrome/rest/ping.view?u=flynn&v=1.13.0&c=Feishin&p={secret}"),
    )
    .await;
    assert_eq!(ping["subsonic-response"]["status"], json!("ok"));

    let user = get(
        app,
        &format!(
            "/compat/navidrome/rest/getUser.view?u=flynn&v=1.13.0&c=Feishin&p={secret}&username=flynn"
        ),
    )
    .await;
    assert_eq!(user["subsonic-response"]["status"], json!("ok"));
    assert_eq!(
        user["subsonic-response"]["user"]["username"],
        json!("flynn")
    );
    assert_eq!(user["subsonic-response"]["user"]["streamRole"], json!(true));
}

#[sqlx::test]
async fn member_start_scan_is_forbidden(pool: PgPool) {
    let fixture = credential_service_with_roles();
    let (app, scanner) = role_app(pool, fixture.service.clone());
    let auth = auth_query(fixture.member, &fixture.member_secret);

    let (status, body) =
        get_status(app, &format!("/compat/subsonic/rest/startScan.view?{auth}")).await;

    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["subsonic-response"]["status"], json!("failed"));
    assert_eq!(body["subsonic-response"]["error"]["code"], json!(50));
    let status = scanner.status();
    assert!(!status.scanning);
    assert!(status.last_scan.is_none());
    assert!(status.last_report.is_none());
}

#[sqlx::test]
async fn admin_start_scan_succeeds(pool: PgPool) {
    let fixture = credential_service_with_roles();
    let (app, _scanner) = role_app(pool, fixture.service.clone());
    let auth = auth_query(fixture.admin, &fixture.admin_secret);

    let (status, body) =
        get_status(app, &format!("/compat/subsonic/rest/startScan.view?{auth}")).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["subsonic-response"]["status"], json!("ok"));
    assert_eq!(
        body["subsonic-response"]["scanStatus"]["scanning"],
        json!(true)
    );
    assert_eq!(body["subsonic-response"]["scanStatus"]["count"], json!(0));
}

#[sqlx::test]
async fn member_cannot_read_another_user(pool: PgPool) {
    let fixture = credential_service_with_roles();
    let app = app(pool, fixture.service.clone());
    let auth = auth_query(fixture.member, &fixture.member_secret);

    let other = get(
        app.clone(),
        &format!(
            "/compat/subsonic/rest/getUser.view?{auth}&username={}",
            fixture.admin
        ),
    )
    .await;
    assert_eq!(other["subsonic-response"]["status"], json!("failed"));
    assert_eq!(other["subsonic-response"]["error"]["code"], json!(70));
    assert_eq!(
        other["subsonic-response"]["error"]["message"],
        json!("User not found.")
    );

    let own = get(
        app,
        &format!(
            "/compat/subsonic/rest/getUser.view?{auth}&username={}",
            fixture.member
        ),
    )
    .await;
    assert_eq!(own["subsonic-response"]["status"], json!("ok"));
    assert_eq!(
        own["subsonic-response"]["user"]["username"],
        json!(fixture.member)
    );
    assert_eq!(own["subsonic-response"]["user"]["adminRole"], json!(false));
}

#[sqlx::test]
async fn admin_can_read_another_user(pool: PgPool) {
    let fixture = credential_service_with_roles();
    let app = app(pool, fixture.service.clone());
    let auth = auth_query(fixture.admin, &fixture.admin_secret);

    let other = get(
        app,
        &format!(
            "/compat/subsonic/rest/getUser.view?{auth}&username={}",
            fixture.second_admin
        ),
    )
    .await;
    assert_eq!(other["subsonic-response"]["status"], json!("ok"));
    assert_eq!(
        other["subsonic-response"]["user"]["username"],
        json!(fixture.second_admin)
    );
    assert_eq!(other["subsonic-response"]["user"]["adminRole"], json!(true));

    let rendered = other.to_string();
    assert!(!rendered.contains(&fixture.admin_secret));
    assert!(!rendered.contains(&fixture.member_secret));
}

#[sqlx::test]
async fn subsonic_user_roles_match_the_approved_table(pool: PgPool) {
    let fixture = credential_service_with_roles();
    let app = app(pool, fixture.service.clone());

    let cases = [
        (fixture.member, fixture.member_secret.as_str(), false),
        (fixture.admin, fixture.admin_secret.as_str(), true),
    ];

    for (username, secret, admin) in cases {
        let auth = auth_query(username, secret);
        let body = get(
            app.clone(),
            &format!("/compat/subsonic/rest/getUser.view?{auth}&username={username}"),
        )
        .await;
        let user = &body["subsonic-response"]["user"];
        assert_eq!(user["username"], json!(username));
        assert_eq!(user["adminRole"], json!(admin));
        assert_eq!(user["streamRole"], json!(true));
        assert_eq!(user["coverArtRole"], json!(true));
        assert_eq!(user["playlistRole"], json!(true));
        assert_eq!(user["scrobblingEnabled"], json!(false));
        assert_eq!(user["settingsRole"], json!(false));
        assert_eq!(user["downloadRole"], json!(false));
        assert_eq!(user["uploadRole"], json!(false));
        assert_eq!(user["commentRole"], json!(false));
        assert_eq!(user["podcastRole"], json!(false));
        assert_eq!(user["jukeboxRole"], json!(false));
        assert_eq!(user["shareRole"], json!(false));
        assert_eq!(user["videoConversionRole"], json!(false));
    }
}

#[sqlx::test]
async fn navidrome_login_reports_the_real_admin_flag(pool: PgPool) {
    let fixture = credential_service_with_roles();
    let app = app(pool, fixture.service.clone());

    let (_, member) = post_login(app.clone(), fixture.member, &fixture.member_secret).await;
    assert_eq!(member["isAdmin"], json!(false));

    let (_, admin) = post_login(app, fixture.admin, &fixture.admin_secret).await;
    assert_eq!(admin["isAdmin"], json!(true));
}

#[sqlx::test]
async fn page_routes_and_role_changes_do_not_leak_credentials(pool: PgPool) {
    use base64::Engine;
    use base64::engine::general_purpose::STANDARD_NO_PAD;

    let key = whio_core::features::identity::secrets::CredentialKey::from_base64(
        &STANDARD_NO_PAD.encode([11u8; 32]),
    )
    .expect("valid key");
    let provisioning = whio_core::features::identity::provision(pool.clone());
    provisioning.create_user("flynn", "Flynn").await.unwrap();
    let (credential_id, secret) = provisioning
        .mint_app_password(&key, "flynn", "integration")
        .await
        .unwrap();
    let service = Arc::new(whio_core::features::identity::credentials(
        pool.clone(),
        key.clone(),
    ));
    let app = app(pool, service);
    let auth = auth_query("flynn", &secret);

    let before = get(app.clone(), &format!("/rest/ping.view?{auth}")).await;
    assert_eq!(before["subsonic-response"]["status"], json!("ok"));

    provisioning.assign_admin("flynn").await.unwrap();

    let after = get(app.clone(), &format!("/rest/ping.view?{auth}")).await;
    assert_eq!(after["subsonic-response"]["status"], json!("ok"));

    let digest_hex = hex::encode(whio_core::features::identity::secrets::lookup_digest(
        &secret,
    ));
    let credential_id = credential_id.to_string();

    let user = get(
        app.clone(),
        &format!("/compat/subsonic/rest/getUser.view?{auth}&username=flynn"),
    )
    .await;
    assert_eq!(user["subsonic-response"]["user"]["adminRole"], json!(true));
    let rendered = user.to_string();
    assert!(!rendered.contains(&secret));
    assert!(!rendered.contains(&credential_id));
    assert!(!rendered.contains(&digest_hex));

    let (_, login) = post_login(app, "flynn", &secret).await;
    let rendered = login.to_string();
    assert!(!rendered.contains(&secret));
    assert!(!rendered.contains(&credential_id));
    assert!(!rendered.contains(&digest_hex));
}

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

async fn seed_tracks(pool: &PgPool, count: u32) {
    let repository = PostgresTrackRepository::new(pool.clone());
    let album = repository
        .resolve_or_create_album(
            &AlbumMetadata::new("Discovery".to_owned())
                .unwrap()
                .with_album_artist(ArtistCredit::new("Daft Punk".to_owned(), None).unwrap()),
        )
        .await
        .unwrap();

    for index in 0..count {
        let track = repository
            .get_or_create(
                local_source(&format!("path-{index:04}")),
                local_metadata(&format!("song-{index:04}"), "Daft Punk"),
            )
            .await
            .unwrap();
        repository
            .set_album_track(album.id(), track.id(), &AlbumTrack::unplaced())
            .await
            .unwrap();
    }
}

async fn seed_three_kinds(pool: &PgPool) {
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
                    local_metadata(&format!("song-{number:02}"), artist),
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

fn enumeration_client(
    pool: PgPool,
    responses: Vec<
        Result<
            Vec<whio_core::features::music_library::catalogue::CatalogueCandidate>,
            whio_core::features::music_library::catalogue::ProviderError,
        >,
    >,
) -> (axum::Router, String) {
    let (service, secret) = credential_service_with_known_app_password();
    let (search, library, playback) = support::services(pool, responses);

    (router(service, search, library, playback), secret)
}

fn field_str<'a>(values: &'a Value, field: &str) -> Vec<&'a str> {
    values
        .as_array()
        .unwrap()
        .iter()
        .map(|value| value[field].as_str().unwrap())
        .collect()
}

fn subsonic_search3(uri: &str, auth: &str) -> String {
    format!("/compat/subsonic/rest/search3.view?{auth}&{uri}")
}

#[sqlx::test]
async fn empty_search_enumerates_three_kinds_with_independent_windows(pool: PgPool) {
    seed_three_kinds(&pool).await;
    let (app, secret) = enumeration_client(pool, vec![]);
    let auth = auth_query("flynn", &secret);
    let body = get(
        app,
        &subsonic_search3(
            "query=&songCount=2&songOffset=1&albumCount=1&albumOffset=1&artistCount=1&artistOffset=2",
            &auth,
        ),
    )
    .await;

    let result = &body["subsonic-response"]["searchResult3"];
    assert_eq!(field_str(&result["song"], "title"), ["song-02", "song-03"]);
    assert_eq!(field_str(&result["album"], "name"), ["Album B"]);
    assert_eq!(field_str(&result["artist"], "name"), ["Artist Two"]);
}

#[sqlx::test]
async fn empty_search_does_not_call_the_catalogue_provider(pool: PgPool) {
    seed_three_kinds(&pool).await;
    let (app, secret) = enumeration_client(pool, vec![]);
    let auth = auth_query("flynn", &secret);
    let body = get(app, &subsonic_search3("query=", &auth)).await;

    assert_eq!(body["subsonic-response"]["status"], json!("ok"));
    let result = &body["subsonic-response"]["searchResult3"];
    assert_eq!(result["song"].as_array().unwrap().len(), 6);
    assert_eq!(result["album"].as_array().unwrap().len(), 3);
    assert_eq!(result["artist"].as_array().unwrap().len(), 3);
}

#[sqlx::test]
async fn zero_song_count_returns_no_songs_and_still_serves_other_kinds(pool: PgPool) {
    seed_three_kinds(&pool).await;
    let (app, secret) = enumeration_client(pool, vec![]);
    let auth = auth_query("flynn", &secret);
    let body = get(
        app,
        &subsonic_search3("query=&songCount=0&albumCount=2&artistCount=2", &auth),
    )
    .await;

    let result = &body["subsonic-response"]["searchResult3"];
    assert!(result.get("song").is_none());
    assert_eq!(field_str(&result["album"], "name"), ["Album A", "Album B"]);
    assert_eq!(
        field_str(&result["artist"], "name"),
        ["Artist One", "Artist Three"]
    );
}

#[sqlx::test]
async fn song_count_above_25_is_honoured(pool: PgPool) {
    seed_tracks(&pool, 200).await;
    let (app, secret) = enumeration_client(pool, vec![]);
    let auth = auth_query("flynn", &secret);
    let (status, body) = get_status_bounded(
        app,
        &subsonic_search3("query=&songCount=100", &auth),
        1 << 20,
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let result = &body["subsonic-response"]["searchResult3"];
    let songs = result["song"].as_array().unwrap();
    assert_eq!(songs.len(), 100);
    assert_eq!(songs[0]["title"], json!("song-0000"));
    assert_eq!(songs[99]["title"], json!("song-0099"));
}

#[sqlx::test]
async fn song_count_above_max_is_an_invalid_search_error(pool: PgPool) {
    seed_three_kinds(&pool).await;
    let (app, secret) = enumeration_client(pool, vec![]);
    let auth = auth_query("flynn", &secret);

    for window in [
        "songCount=501",
        "albumCount=501",
        "artistCount=501",
        "songCount=abc",
        "songOffset=abc",
    ] {
        let (status, body) = get_status(
            app.clone(),
            &subsonic_search3(&format!("query=&{window}"), &auth),
        )
        .await;

        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "window: {window}");
        assert_eq!(
            body["subsonic-response"]["status"],
            json!("failed"),
            "window: {window}"
        );
        assert!(
            body["subsonic-response"]["searchResult3"].is_null(),
            "window: {window}"
        );
    }
}

#[sqlx::test]
async fn deep_song_offset_enumeration_passes(pool: PgPool) {
    seed_tracks(&pool, 1001).await;
    let (app, secret) = enumeration_client(pool, vec![]);
    let auth = auth_query("flynn", &secret);
    let body = get(
        app,
        &subsonic_search3("query=&songCount=20&songOffset=1000", &auth),
    )
    .await;

    let result = &body["subsonic-response"]["searchResult3"];
    let songs = result["song"].as_array().unwrap();
    assert_eq!(songs.len(), 1);
    assert_eq!(songs[0]["title"], json!("song-1000"));
}

#[sqlx::test]
async fn nonempty_query_still_uses_bounded_provider_search(pool: PgPool) {
    use whio_core::features::music_library::catalogue::CatalogueCandidate;

    let candidates = (0..30)
        .map(|index| {
            CatalogueCandidate::new(
                SourceIdentity::new(
                    ProviderId::new("youtube".to_owned()).unwrap(),
                    SourceScope::Global,
                    format!("vid-{index}"),
                )
                .unwrap(),
                local_metadata(&format!("song-{index:02}"), "artist"),
            )
        })
        .collect::<Vec<_>>();
    let (app, secret) = enumeration_client(pool, vec![Ok(candidates)]);
    let auth = auth_query("flynn", &secret);
    let body = get(
        app.clone(),
        &subsonic_search3("query=song&songCount=100&songOffset=0", &auth),
    )
    .await;

    assert_eq!(body["subsonic-response"]["status"], json!("ok"));
    let result = &body["subsonic-response"]["searchResult3"];
    assert_eq!(result["song"].as_array().unwrap().len(), 25);

    let (status, body) = get_status(
        app,
        &subsonic_search3("query=song&songCount=0&songOffset=0", &auth),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["subsonic-response"]["status"], json!("ok"));
    assert!(
        body["subsonic-response"]["searchResult3"]
            .get("song")
            .is_none()
    );
}

#[sqlx::test]
async fn absent_query_is_still_missing_parameter(pool: PgPool) {
    seed_three_kinds(&pool).await;
    let (app, secret) = enumeration_client(pool, vec![]);
    let auth = auth_query("flynn", &secret);

    let (status, body) = get_status(
        app.clone(),
        &format!("/compat/subsonic/rest/search3.view?{auth}&songCount=10"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["subsonic-response"]["status"], json!("failed"));
    assert_eq!(body["subsonic-response"]["error"]["code"], json!(10));

    let (status, body) =
        get_status(app, &format!("/compat/subsonic/rest/search3.view?{auth}")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["subsonic-response"]["status"], json!("failed"));
    assert_eq!(body["subsonic-response"]["error"]["code"], json!(10));
}

#[sqlx::test]
async fn enumerated_song_children_carry_the_membership_facts(pool: PgPool) {
    let (credential, secret) =
        support::credentials::database_credential_service(pool.clone()).await;
    let repository = PostgresTrackRepository::new(pool.clone());
    let album = repository
        .resolve_or_create_album(
            &AlbumMetadata::new("Discovery".to_owned())
                .unwrap()
                .with_album_artist(ArtistCredit::new("Daft Punk".to_owned(), None).unwrap()),
        )
        .await
        .unwrap();
    let placed = repository
        .get_or_create(
            local_source("path-a"),
            local_metadata("Placed", "Daft Punk"),
        )
        .await
        .unwrap();
    repository
        .get_or_create(local_source("path-b"), local_metadata("Loose", "Daft Punk"))
        .await
        .unwrap();
    repository
        .set_album_track(
            album.id(),
            placed.id(),
            &AlbumTrack::new(Some(1), Some(2), 0).unwrap(),
        )
        .await
        .unwrap();
    let user_id: uuid::Uuid = sqlx::query_scalar("SELECT id FROM users WHERE username = 'flynn'")
        .fetch_one(&pool)
        .await
        .unwrap();
    repository
        .set_rating(user_id, StateTarget::Track(placed.id()), Some(4))
        .await
        .unwrap();

    let (search, library, playback) = support::services(pool, vec![]);
    let app = router(credential, search, library, playback);
    let auth = auth_query("flynn", &secret);
    let body = get(app, &subsonic_search3("query=", &auth)).await;

    let result = &body["subsonic-response"]["searchResult3"];
    let songs = result["song"].as_array().unwrap();
    assert_eq!(field_str(&result["song"], "title"), ["Loose", "Placed"]);

    let loose = &songs[0];
    assert!(loose.get("track").is_none());
    assert!(loose.get("discNumber").is_none());
    assert!(loose.get("userRating").is_none());
    assert!(loose.get("albumId").is_none());

    let enumerated = &songs[1];
    assert_eq!(enumerated["track"], json!(2));
    assert_eq!(enumerated["discNumber"], json!(1));
    assert_eq!(enumerated["userRating"], json!(4));
    assert_eq!(enumerated["album"], json!("Discovery"));
    assert_eq!(enumerated["albumId"], json!(album.id().to_string()));
}

struct QueueFixture {
    app: axum::Router,
    credential: Arc<CredentialService>,
    search: Arc<SearchService>,
    library: Arc<LibraryService>,
    queue: Arc<RestoreQueueService>,
    playback: Arc<StreamingService>,
    owner: String,
    stranger: String,
    stranger_id: Uuid,
}

async fn queue_fixture(pool: PgPool) -> QueueFixture {
    let key = CredentialKey::from_base64(&STANDARD_NO_PAD.encode([11u8; 32])).unwrap();
    let (service, owner_secret) =
        support::credentials::database_credential_service(pool.clone()).await;
    let provisioning = whio_core::features::identity::provision(pool.clone());
    let stranger_id = provisioning.create_user("other", "Other").await.unwrap();
    let (_, stranger_secret) = provisioning
        .mint_app_password(&key, "other", "integration")
        .await
        .unwrap();
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
    let queue = Arc::new(RestoreQueueService::new(
        Arc::clone(&ports.store) as Arc<dyn RestoreQueueStore>
    ));
    let playback = ports.streaming(vec![]);

    QueueFixture {
        app: router_with_queue(
            Arc::clone(&service),
            Arc::clone(&search),
            Arc::clone(&library),
            Arc::clone(&queue),
            Arc::clone(&playback),
        ),
        credential: service,
        search,
        library,
        queue,
        playback,
        owner: playlist_auth("flynn", &owner_secret),
        stranger: playlist_auth("other", &stranger_secret),
        stranger_id,
    }
}

async fn provider_song(pool: &PgPool, external_id: &str, title: &str) -> TrackId {
    let ports = StorePorts::new(pool.clone());

    ports
        .store
        .get_or_create(
            SourceIdentity::new(
                ProviderId::new("youtube_music".to_owned()).unwrap(),
                SourceScope::Global,
                external_id.to_owned(),
            )
            .unwrap(),
            TrackMetadata::from_names(title.to_owned(), vec!["Artist".to_owned()], Some(1_000))
                .unwrap(),
        )
        .await
        .unwrap()
        .id()
        .clone()
}

fn play_queue(body: &Value) -> &Value {
    &body["subsonic-response"]["playQueue"]
}

fn play_queue_by_index(body: &Value) -> &Value {
    &body["subsonic-response"]["playQueueByIndex"]
}

fn queue_entry_ids(queue: &Value) -> Vec<String> {
    queue["entry"]
        .as_array()
        .map(|entries| {
            entries
                .iter()
                .map(|entry| entry["id"].as_str().unwrap().to_owned())
                .collect()
        })
        .unwrap_or_default()
}

fn queue_entry_titles(queue: &Value) -> Vec<String> {
    queue["entry"]
        .as_array()
        .map(|entries| {
            entries
                .iter()
                .map(|entry| entry["title"].as_str().unwrap().to_owned())
                .collect()
        })
        .unwrap_or_default()
}

async fn read_queue(app: &axum::Router, auth: &str) -> Value {
    get(app.clone(), &playlist_route("getPlayQueue", auth, "")).await
}

async fn read_queue_by_index(app: &axum::Router, auth: &str) -> Value {
    get(
        app.clone(),
        &playlist_route("getPlayQueueByIndex", auth, ""),
    )
    .await
}

async fn save_queue(app: &axum::Router, auth: &str, parameters: &str) -> Value {
    get(
        app.clone(),
        &playlist_route("savePlayQueue", auth, parameters),
    )
    .await
}

async fn save_queue_by_index(app: &axum::Router, auth: &str, parameters: &str) -> Value {
    get(
        app.clone(),
        &playlist_route("savePlayQueueByIndex", auth, parameters),
    )
    .await
}

#[sqlx::test]
async fn subsonic_play_queue_round_trips(pool: PgPool) {
    let fixture = queue_fixture(pool.clone()).await;
    let first = playlist_song(&pool, "path-first", "First").await;
    let second = playlist_song(&pool, "path-second", "Second").await;
    let third = playlist_song(&pool, "path-third", "Third").await;

    let body = save_queue(
        &fixture.app,
        &fixture.owner,
        &format!("id={first}&id={second}&id={third}&current={second}&position=12000"),
    )
    .await;
    assert_eq!(body["subsonic-response"]["status"], json!("ok"));

    let body = read_queue(&fixture.app, &fixture.owner).await;
    let queue = play_queue(&body);
    assert_eq!(
        queue_entry_ids(queue),
        vec![first.to_string(), second.to_string(), third.to_string()],
        "the entries keep the saved order"
    );
    assert_eq!(queue_entry_titles(queue), ["First", "Second", "Third"]);
    assert_eq!(queue["current"], json!(second.to_string()));
    assert_eq!(queue["position"], json!(12000));
    assert_eq!(queue["username"], json!("flynn"));
    assert!(
        queue["changed"]
            .as_str()
            .is_some_and(|value| !value.is_empty()),
        "a saved queue carries when it changed"
    );

    let body = read_queue_by_index(&fixture.app, &fixture.owner).await;
    let queue = play_queue_by_index(&body);
    assert_eq!(queue["currentIndex"], json!(1));
    assert_eq!(queue["position"], json!(12000));
    assert_eq!(
        queue_entry_ids(queue),
        vec![first.to_string(), second.to_string(), third.to_string()],
        "the index-based read returns the same occurrences"
    );
}

#[sqlx::test]
async fn subsonic_absent_and_empty_queues_are_an_identical_shape(pool: PgPool) {
    let fixture = queue_fixture(pool.clone()).await;

    let absent = read_queue(&fixture.app, &fixture.stranger).await;
    let absent_by_index = read_queue_by_index(&fixture.app, &fixture.stranger).await;

    assert_eq!(absent["subsonic-response"]["status"], json!("ok"));
    assert_eq!(
        play_queue(&absent),
        &json!({
            "username": "other",
            "changed": "0001-01-01T00:00:00Z",
            "changedBy": "",
        }),
        "nothing saved is an empty queue, not an error"
    );

    fixture
        .queue
        .save(
            fixture.stranger_id,
            RestoreQueueSave {
                entries: Vec::new(),
                current_index: None,
                position_ms: 0,
            },
        )
        .await
        .unwrap();

    let empty = read_queue(&fixture.app, &fixture.stranger).await;
    let empty_by_index = read_queue_by_index(&fixture.app, &fixture.stranger).await;

    assert_eq!(
        play_queue(&empty),
        play_queue(&absent),
        "the wire cannot tell a saved empty queue from none at all"
    );
    assert_eq!(
        play_queue_by_index(&empty_by_index),
        play_queue_by_index(&absent_by_index),
        "nor the index-based read"
    );
}

#[sqlx::test]
async fn subsonic_a_save_replaces_the_whole_queue(pool: PgPool) {
    let fixture = queue_fixture(pool.clone()).await;
    let first = playlist_song(&pool, "path-first", "First").await;
    let second = playlist_song(&pool, "path-second", "Second").await;
    let third = playlist_song(&pool, "path-third", "Third").await;

    save_queue(
        &fixture.app,
        &fixture.owner,
        &format!("id={first}&id={second}&current={first}"),
    )
    .await;
    save_queue(
        &fixture.app,
        &fixture.owner,
        &format!("id={third}&current={third}"),
    )
    .await;

    let body = read_queue(&fixture.app, &fixture.owner).await;
    let queue = play_queue(&body);
    assert_eq!(
        queue_entry_ids(queue),
        vec![third.to_string()],
        "the second save leaves nothing of the first"
    );
    assert_eq!(queue["current"], json!(third.to_string()));
}

#[sqlx::test]
async fn subsonic_current_names_the_first_matching_occurrence(pool: PgPool) {
    let fixture = queue_fixture(pool.clone()).await;
    let first = playlist_song(&pool, "path-first", "First").await;
    let second = playlist_song(&pool, "path-second", "Second").await;

    save_queue(
        &fixture.app,
        &fixture.owner,
        &format!("id={first}&id={second}&id={first}&current={first}"),
    )
    .await;

    let body = read_queue_by_index(&fixture.app, &fixture.owner).await;
    assert_eq!(
        play_queue_by_index(&body)["currentIndex"],
        json!(0),
        "an ambiguous current id names the first matching occurrence"
    );

    let body = get(
        fixture.app.clone(),
        &playlist_route(
            "savePlayQueueByIndex",
            &fixture.owner,
            &format!("id={first}&id={second}&id={first}&currentIndex=2"),
        ),
    )
    .await;
    assert_eq!(body["subsonic-response"]["status"], json!("ok"));

    let body = read_queue_by_index(&fixture.app, &fixture.owner).await;
    let queue = play_queue_by_index(&body);
    assert_eq!(queue["currentIndex"], json!(2));
    assert_eq!(
        queue_entry_ids(queue),
        vec![first.to_string(), second.to_string(), first.to_string()],
        "the repeated track is two distinct occurrences"
    );

    let body = read_queue(&fixture.app, &fixture.owner).await;
    assert_eq!(
        play_queue(&body)["current"],
        json!(first.to_string()),
        "the id-based read cannot say which occurrence is current"
    );
}

#[sqlx::test]
async fn subsonic_an_index_outside_the_queue_is_a_client_error(pool: PgPool) {
    let fixture = queue_fixture(pool.clone()).await;
    let first = playlist_song(&pool, "path-first", "First").await;
    let second = playlist_song(&pool, "path-second", "Second").await;

    save_queue_by_index(
        &fixture.app,
        &fixture.owner,
        &format!("id={first}&id={second}&currentIndex=1&position=5000"),
    )
    .await;

    let (status, body) = get_status(
        fixture.app.clone(),
        &playlist_route(
            "savePlayQueueByIndex",
            &fixture.owner,
            &format!("id={first}&id={second}&currentIndex=7&position=5000"),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["subsonic-response"]["status"], json!("failed"));
    assert_eq!(body["subsonic-response"]["error"]["code"], json!(10));

    let saved = read_queue(&fixture.app, &fixture.owner).await;
    assert_eq!(play_queue(&saved)["position"], json!(5000));
    let rejected = read_queue(&fixture.app, &fixture.owner).await;
    assert_eq!(
        play_queue(&rejected),
        play_queue(&saved),
        "the rejected save changed nothing"
    );
}

#[sqlx::test]
async fn subsonic_an_over_bound_save_is_rejected_and_changes_nothing(pool: PgPool) {
    let fixture = queue_fixture(pool.clone()).await;
    let first = playlist_song(&pool, "path-first", "First").await;
    let second = playlist_song(&pool, "path-second", "Second").await;

    save_queue(
        &fixture.app,
        &fixture.owner,
        &format!("id={first}&id={second}&current={first}"),
    )
    .await;

    let over_bound = std::iter::repeat_n(format!("id={first}"), 1001).collect::<Vec<_>>();
    let (status, body) = get_status(
        fixture.app.clone(),
        &playlist_route(
            "savePlayQueue",
            &fixture.owner,
            &format!("{}&current={first}", over_bound.join("&")),
        ),
    )
    .await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["subsonic-response"]["status"], json!("failed"));
    assert_eq!(
        body["subsonic-response"]["error"]["message"],
        json!("Queue holds at most 1000 entries."),
        "the client error names the bound it must respect, not an internal identifier"
    );

    let body = read_queue(&fixture.app, &fixture.owner).await;
    assert_eq!(
        queue_entry_ids(play_queue(&body)),
        vec![first.to_string(), second.to_string()],
        "the rejected save left the previous snapshot untouched"
    );
}

#[sqlx::test]
async fn subsonic_an_unavailable_track_keeps_its_entry(pool: PgPool) {
    let fixture = queue_fixture(pool.clone()).await;
    let unavailable = provider_song(&pool, "vid-1", "Unplayable").await;
    let app = whio_core::app::router(
        AppState::new(
            Arc::clone(&fixture.credential),
            Arc::clone(&fixture.search),
            Arc::clone(&fixture.library),
            Arc::clone(&fixture.playback),
        )
        .with_queue(Arc::clone(&fixture.queue))
        .with_source_tags(SourceTags::ArtistPrefix),
    );

    save_queue(
        &fixture.app,
        &fixture.owner,
        &format!("id={unavailable}&current={unavailable}"),
    )
    .await;

    let body = read_queue(&app, &fixture.owner).await;
    let queue = play_queue(&body);
    assert_eq!(
        queue_entry_ids(queue),
        vec![unavailable.to_string()],
        "an entry nothing can play is reported, not dropped"
    );
    assert_eq!(
        queue["entry"][0]["artist"],
        json!("[YTM] Artist"),
        "the entry carries the availability its sources report"
    );
}

#[sqlx::test]
async fn subsonic_a_save_without_ids_clears_the_queue(pool: PgPool) {
    let fixture = queue_fixture(pool.clone()).await;
    let first = playlist_song(&pool, "path-first", "First").await;

    save_queue(
        &fixture.app,
        &fixture.owner,
        &format!("id={first}&current={first}&position=900"),
    )
    .await;

    let body = save_queue(&fixture.app, &fixture.owner, "").await;
    assert_eq!(body["subsonic-response"]["status"], json!("ok"));

    let body = read_queue(&fixture.app, &fixture.owner).await;
    let queue = play_queue(&body);
    assert!(queue.get("entry").is_none());
    assert!(queue.get("current").is_none());
    assert_eq!(queue["username"], json!("flynn"));

    let (status, body) = get_status(
        fixture.app.clone(),
        &playlist_route("savePlayQueue", &fixture.owner, &format!("current={first}")),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["subsonic-response"]["error"]["code"], json!(10));
    assert_eq!(
        body["subsonic-response"]["status"],
        json!("failed"),
        "current without a queue is a missing parameter, not a clear"
    );
}

#[sqlx::test]
async fn subsonic_another_users_queue_is_invisible(pool: PgPool) {
    let fixture = queue_fixture(pool.clone()).await;
    let first = playlist_song(&pool, "path-first", "First").await;
    let second = playlist_song(&pool, "path-second", "Second").await;

    save_queue(
        &fixture.app,
        &fixture.owner,
        &format!("id={first}&current={first}"),
    )
    .await;
    save_queue(
        &fixture.app,
        &fixture.stranger,
        &format!("id={second}&current={second}"),
    )
    .await;

    let body = read_queue(&fixture.app, &fixture.stranger).await;
    assert_eq!(
        queue_entry_ids(play_queue(&body)),
        vec![second.to_string()],
        "a caller reads their own queue"
    );

    let body = read_queue(&fixture.app, &fixture.owner).await;
    assert_eq!(
        queue_entry_ids(play_queue(&body)),
        vec![first.to_string()],
        "one caller's save never reaches another's queue"
    );
}

#[sqlx::test]
async fn subsonic_the_advertisement_ships_with_the_index_based_endpoints(pool: PgPool) {
    let fixture = queue_fixture(pool.clone()).await;
    let first = playlist_song(&pool, "path-first", "First").await;

    let body = get(
        fixture.app.clone(),
        &playlist_route("getOpenSubsonicExtensions", &fixture.owner, ""),
    )
    .await;
    let extensions = body["subsonic-response"]["openSubsonicExtensions"]
        .as_array()
        .unwrap();
    let queue_extension = extensions
        .iter()
        .find(|extension| extension["name"] == json!("indexBasedQueue"))
        .expect("the index-based queue is advertised");

    assert_eq!(queue_extension["versions"], json!([1]));

    save_queue_by_index(
        &fixture.app,
        &fixture.owner,
        &format!("id={first}&currentIndex=0"),
    )
    .await;

    let body = read_queue_by_index(&fixture.app, &fixture.owner).await;
    assert_eq!(
        queue_entry_ids(play_queue_by_index(&body)),
        vec![first.to_string()],
        "the advertised extension answers on the same revision"
    );
}

#[sqlx::test]
async fn subsonic_play_queue_request_errors_name_the_parameter(pool: PgPool) {
    let fixture = queue_fixture(pool.clone()).await;
    let first = playlist_song(&pool, "path-first", "First").await;
    let second = playlist_song(&pool, "path-second", "Second").await;

    let (status, body) = get_status(
        fixture.app.clone(),
        &playlist_route(
            "savePlayQueue",
            &fixture.owner,
            &format!("id={first}&id={second}"),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["subsonic-response"]["error"]["code"], json!(10));

    let (status, body) = get_status(
        fixture.app.clone(),
        &playlist_route(
            "savePlayQueue",
            &fixture.owner,
            &format!("id={first}&current={second}"),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["subsonic-response"]["status"], json!("failed"));

    let (status, _) = get_status(
        fixture.app.clone(),
        &playlist_route(
            "savePlayQueue",
            &fixture.owner,
            &format!("id={first}&current={first}&position=soon"),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let missing = Uuid::now_v7();
    let (status, body) = get_status(
        fixture.app.clone(),
        &playlist_route(
            "savePlayQueue",
            &fixture.owner,
            &format!("id={missing}&current={missing}"),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(
        body["subsonic-response"]["error"]["message"],
        json!("Invalid track ID.")
    );

    let body = read_queue(&fixture.app, &fixture.owner).await;
    assert!(
        play_queue(&body).get("entry").is_none(),
        "no rejected save reached the snapshot"
    );
}

async fn post_status(
    app: axum::Router,
    uri: &str,
    content_type: Option<&str>,
    body: &str,
) -> (StatusCode, Value) {
    let mut builder = Request::builder()
        .method("POST")
        .uri(uri)
        .header("x-request-id", "request-123");

    if let Some(content_type) = content_type {
        builder = builder.header("content-type", content_type);
    }

    let response = app
        .oneshot(builder.body(Body::from(body.to_owned())).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let body = to_bytes(response.into_body(), 8192).await.unwrap();
    (status, serde_json::from_slice(&body).unwrap())
}

#[sqlx::test]
async fn subsonic_a_position_the_wire_cannot_carry_is_rejected(pool: PgPool) {
    let fixture = queue_fixture(pool.clone()).await;
    let first = playlist_song(&pool, "path-first", "First").await;

    save_queue(
        &fixture.app,
        &fixture.owner,
        &format!("id={first}&current={first}&position=10"),
    )
    .await;
    let saved = read_queue(&fixture.app, &fixture.owner).await;

    let beyond_wire = i32::MAX as u64 + 1;
    for route in ["savePlayQueue", "savePlayQueueByIndex"] {
        for position in [beyond_wire, i64::MAX as u64 + 1] {
            let (status, body) = get_status(
                fixture.app.clone(),
                &playlist_route(
                    route,
                    &fixture.owner,
                    &format!("id={first}&current={first}&currentIndex=0&position={position}"),
                ),
            )
            .await;

            assert_eq!(
                status,
                StatusCode::BAD_REQUEST,
                "{route} rejects position {position}, which its own wire cannot carry"
            );
            assert_eq!(
                body["subsonic-response"]["error"]["message"],
                json!("Invalid position.")
            );
        }
    }

    let rejected = read_queue(&fixture.app, &fixture.owner).await;
    assert_eq!(
        play_queue(&rejected),
        play_queue(&saved),
        "a rejected position changed nothing"
    );

    let (status, body) = get_status(
        fixture.app.clone(),
        &playlist_route(
            "savePlayQueue",
            &fixture.owner,
            &format!("id={first}&current={first}&position={}", i32::MAX),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["subsonic-response"]["status"], json!("ok"));

    let body = read_queue(&fixture.app, &fixture.owner).await;
    assert_eq!(
        play_queue(&body)["position"],
        json!(i32::MAX),
        "the widest position the wire carries is read back as it was written"
    );
}

#[sqlx::test]
async fn subsonic_a_body_the_facade_cannot_read_is_not_a_clear(pool: PgPool) {
    let fixture = queue_fixture(pool.clone()).await;
    let first = playlist_song(&pool, "path-first", "First").await;
    let second = playlist_song(&pool, "path-second", "Second").await;

    save_queue(
        &fixture.app,
        &fixture.owner,
        &format!("id={first}&current={first}"),
    )
    .await;

    let (status, body) = post_status(
        fixture.app.clone(),
        &playlist_route("savePlayQueue", &fixture.owner, ""),
        Some("application/x-www-form-urlencoded"),
        &format!("id={second}&current={second}&position=1000"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["subsonic-response"]["error"]["code"], json!(10));

    let body = read_queue(&fixture.app, &fixture.owner).await;
    assert_eq!(
        queue_entry_ids(play_queue(&body)),
        vec![first.to_string()],
        "a save whose parameters arrived in a body the facade cannot read is not a clear"
    );

    let (status, body) = post_status(
        fixture.app.clone(),
        &playlist_route(
            "savePlayQueue",
            &fixture.owner,
            &format!("id={second}&current={second}"),
        ),
        None,
        "",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["subsonic-response"]["status"], json!("ok"));

    let body = read_queue(&fixture.app, &fixture.owner).await;
    assert_eq!(
        queue_entry_ids(play_queue(&body)),
        vec![second.to_string()],
        "a POST whose parameters arrive in the query is read as the protocol intends"
    );
}

struct ReportingFixture {
    app: axum::Router,
    secret: String,
    pool: PgPool,
    track: TrackId,
    quick: TrackId,
}

fn reporting_metadata(title: &str, duration_ms: u64) -> TrackMetadata {
    TrackMetadata::from_names(
        title.to_owned(),
        vec!["Daft Punk".to_owned()],
        Some(duration_ms),
    )
    .unwrap()
}

async fn reporting_fixture(pool: PgPool) -> ReportingFixture {
    let ports = StorePorts::new(pool.clone());
    let album = ports
        .store
        .resolve_or_create_album(
            &AlbumMetadata::new("Discovery".to_owned())
                .unwrap()
                .with_album_artist(ArtistCredit::new("Daft Punk".to_owned(), None).unwrap()),
        )
        .await
        .unwrap();
    let track = ports
        .store
        .get_or_create(
            local_source("reporting.flac"),
            reporting_metadata("Reported", 240_000),
        )
        .await
        .unwrap();
    let quick = ports
        .store
        .get_or_create(
            local_source("reporting-quick.flac"),
            reporting_metadata("Quick", 2_000),
        )
        .await
        .unwrap();
    ports
        .store
        .set_album_track(album.id(), track.id(), &AlbumTrack::unplaced())
        .await
        .unwrap();
    ports
        .store
        .set_album_track(album.id(), quick.id(), &AlbumTrack::unplaced())
        .await
        .unwrap();

    let (search, library) = ports.bare_catalogue();
    let playback = ports.streaming(vec![
        Arc::new(YtmAdapter::disabled()) as Arc<dyn PlaybackProvider>
    ]);
    let listens = Arc::new(ListenService::new(
        Arc::clone(&ports.store) as Arc<dyn ListenStore>,
        Arc::clone(&ports.store) as Arc<dyn LibraryRead>,
    ));
    let activity = Arc::new(PlaybackActivityService::new(listens, Arc::clone(&library)));
    let key = CredentialKey::from_base64(&STANDARD_NO_PAD.encode([11u8; 32])).expect("valid key");
    let provisioning = whio_core::features::identity::provision(pool.clone());
    provisioning.create_user("flynn", "Flynn").await.unwrap();
    let (_, secret) = provisioning
        .mint_app_password(&key, "flynn", "integration")
        .await
        .unwrap();
    let credential = Arc::new(whio_core::features::identity::credentials(
        pool.clone(),
        key,
    ));
    let app = whio_core::app::router(
        AppState::new(credential, search, library, playback).with_playback_activity(activity),
    );

    ReportingFixture {
        app,
        secret,
        pool,
        track: track.id().clone(),
        quick: quick.id().clone(),
    }
}

fn subsonic_report(
    auth: &str,
    track: &TrackId,
    state: &str,
    position_ms: u64,
    extra: &str,
) -> String {
    format!(
        "/compat/subsonic/rest/reportPlayback.view?{auth}&mediaId={track}&mediaType=song&positionMs={position_ms}&state={state}{extra}"
    )
}

async fn recorded_listens(pool: &PgPool) -> Vec<(String, Option<bool>)> {
    sqlx::query_as("SELECT origin, qualified FROM listens ORDER BY listen_at, origin")
        .fetch_all(pool)
        .await
        .unwrap()
}

async fn counted_plays(pool: &PgPool, track: &TrackId) -> i64 {
    sqlx::query_scalar(
        "SELECT COALESCE(SUM(play_count), 0)::BIGINT FROM track_play_stats WHERE track_id = $1",
    )
    .bind(track.as_uuid())
    .fetch_one(pool)
    .await
    .unwrap()
}

async fn album_song_play_count(app: axum::Router, auth: &str, track: &TrackId) -> Value {
    let body = get(
        app.clone(),
        &format!("/compat/subsonic/rest/getAlbumList2.view?{auth}&type=alphabeticalByName"),
    )
    .await;
    let album_id = body["subsonic-response"]["albumList2"]["album"][0]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let body = get(
        app,
        &format!("/compat/subsonic/rest/getAlbum.view?{auth}&id={album_id}"),
    )
    .await;

    body["subsonic-response"]["album"]["song"]
        .as_array()
        .unwrap()
        .iter()
        .find(|song| song["id"] == json!(track.to_string()))
        .map(|song| song["playCount"].clone())
        .expect("the reported track belongs to the album")
}

async fn now_playing_entries(app: axum::Router, auth: &str) -> Vec<Value> {
    let body = get(
        app,
        &format!("/compat/subsonic/rest/getNowPlaying.view?{auth}"),
    )
    .await;

    body["subsonic-response"]["nowPlaying"]["entry"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

#[sqlx::test]
async fn subsonic_scrobble_submission_records_one_qualified_play(pool: PgPool) {
    let fixture = reporting_fixture(pool).await;
    let auth = auth_query("flynn", &fixture.secret);

    let body = get(
        fixture.app.clone(),
        &format!(
            "/compat/subsonic/rest/scrobble.view?{auth}&id={}",
            fixture.track
        ),
    )
    .await;

    assert_eq!(body["subsonic-response"]["status"], json!("ok"));
    assert_eq!(
        recorded_listens(&fixture.pool).await,
        [("subsonic_scrobble".to_owned(), None)]
    );
    assert_eq!(counted_plays(&fixture.pool, &fixture.track).await, 1);
    assert_eq!(
        album_song_play_count(fixture.app.clone(), &auth, &fixture.track).await,
        json!(1),
        "the counter the client reads moves by the one play"
    );
}

#[sqlx::test]
async fn subsonic_scrobble_without_submission_is_now_playing_only(pool: PgPool) {
    let fixture = reporting_fixture(pool).await;
    let auth = auth_query("flynn", &fixture.secret);

    let body = get(
        fixture.app.clone(),
        &format!(
            "/compat/subsonic/rest/scrobble.view?{auth}&id={}&submission=false",
            fixture.track
        ),
    )
    .await;

    assert_eq!(body["subsonic-response"]["status"], json!("ok"));
    assert!(recorded_listens(&fixture.pool).await.is_empty());
    assert_eq!(counted_plays(&fixture.pool, &fixture.track).await, 0);
    assert_eq!(
        album_song_play_count(fixture.app.clone(), &auth, &fixture.track).await,
        Value::Null
    );

    let entries = now_playing_entries(fixture.app.clone(), &auth).await;

    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0]["id"], json!(fixture.track.to_string()));
    assert_eq!(entries[0]["title"], json!("Reported"));
    assert_eq!(entries[0]["username"], json!("flynn"));
    assert_eq!(entries[0]["playerName"], json!("test-client"));
    assert_eq!(entries[0]["state"], json!("playing"));
    assert!(
        entries[0]["positionMs"].is_number(),
        "a playing session reports the position the server estimates"
    );
    assert_eq!(entries[0]["playbackRate"], json!(1.0));
}

#[sqlx::test]
async fn subsonic_report_playback_records_one_play_for_a_qualified_sequence(pool: PgPool) {
    let fixture = reporting_fixture(pool).await;
    let auth = auth_query("flynn", &fixture.secret);

    for (state, position_ms) in [
        ("starting", 0_u64),
        ("playing", 0),
        ("paused", 1_000),
        ("stopped", 1_000),
    ] {
        let body = get(
            fixture.app.clone(),
            &subsonic_report(
                &auth,
                &fixture.quick,
                state,
                position_ms,
                "&playbackRate=1000000",
            ),
        )
        .await;

        assert_eq!(body["subsonic-response"]["status"], json!("ok"), "{state}");
    }

    assert_eq!(
        recorded_listens(&fixture.pool).await,
        [("report_playback".to_owned(), Some(true))]
    );
    assert_eq!(counted_plays(&fixture.pool, &fixture.quick).await, 1);
    assert_eq!(
        album_song_play_count(fixture.app.clone(), &auth, &fixture.quick).await,
        json!(1)
    );
}

#[sqlx::test]
async fn subsonic_report_playback_below_the_threshold_counts_nothing(pool: PgPool) {
    let fixture = reporting_fixture(pool).await;
    let auth = auth_query("flynn", &fixture.secret);

    for (state, position_ms) in [("starting", 0_u64), ("playing", 0), ("paused", 1_000)] {
        get(
            fixture.app.clone(),
            &subsonic_report(&auth, &fixture.track, state, position_ms, ""),
        )
        .await;
    }

    let entries = now_playing_entries(fixture.app.clone(), &auth).await;

    assert_eq!(entries.len(), 1, "an uncounted sequence is still live");
    assert_eq!(entries[0]["state"], json!("paused"));
    assert_eq!(entries[0]["positionMs"], json!(1_000));

    get(
        fixture.app.clone(),
        &subsonic_report(&auth, &fixture.track, "stopped", 1_000, ""),
    )
    .await;

    assert!(recorded_listens(&fixture.pool).await.is_empty());
    assert_eq!(counted_plays(&fixture.pool, &fixture.track).await, 0);
    assert!(
        now_playing_entries(fixture.app.clone(), &auth)
            .await
            .is_empty()
    );
}

#[sqlx::test]
async fn subsonic_report_playback_ignores_scrobble_across_the_sequence(pool: PgPool) {
    let fixture = reporting_fixture(pool).await;
    let auth = auth_query("flynn", &fixture.secret);

    for (state, position_ms) in [
        ("starting", 0_u64),
        ("playing", 0),
        ("paused", 1_000),
        ("stopped", 1_000),
    ] {
        let body = get(
            fixture.app.clone(),
            &subsonic_report(
                &auth,
                &fixture.quick,
                state,
                position_ms,
                "&playbackRate=1000000&ignoreScrobble=true",
            ),
        )
        .await;

        assert_eq!(body["subsonic-response"]["status"], json!("ok"), "{state}");
    }

    assert!(recorded_listens(&fixture.pool).await.is_empty());
    assert_eq!(counted_plays(&fixture.pool, &fixture.quick).await, 0);
    assert_eq!(
        album_song_play_count(fixture.app.clone(), &auth, &fixture.quick).await,
        Value::Null
    );
    assert!(
        now_playing_entries(fixture.app.clone(), &auth)
            .await
            .is_empty()
    );
}

#[sqlx::test]
async fn subsonic_repeated_scrobble_of_one_play_counts_once(pool: PgPool) {
    let fixture = reporting_fixture(pool).await;
    let auth = auth_query("flynn", &fixture.secret);
    let uri = format!(
        "/compat/subsonic/rest/scrobble.view?{auth}&id={}&time=1000",
        fixture.track
    );

    for _ in 0..2 {
        let body = get(fixture.app.clone(), &uri).await;

        assert_eq!(body["subsonic-response"]["status"], json!("ok"));
    }

    assert_eq!(recorded_listens(&fixture.pool).await.len(), 1);
    assert_eq!(counted_plays(&fixture.pool, &fixture.track).await, 1);

    get(
        fixture.app.clone(),
        &format!(
            "/compat/subsonic/rest/scrobble.view?{auth}&id={}&time=1001",
            fixture.track
        ),
    )
    .await;

    assert_eq!(recorded_listens(&fixture.pool).await.len(), 2);
    assert_eq!(counted_plays(&fixture.pool, &fixture.track).await, 2);
}

#[sqlx::test]
async fn subsonic_scrobble_rejects_a_batch_before_writing_any_of_it(pool: PgPool) {
    let fixture = reporting_fixture(pool).await;
    let auth = auth_query("flynn", &fixture.secret);

    let (status, body) = get_status(
        fixture.app.clone(),
        &format!(
            "/compat/subsonic/rest/scrobble.view?{auth}&id={}&id=not-a-track&time=1000&time=2000",
            fixture.track
        ),
    )
    .await;

    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["subsonic-response"]["error"]["code"], json!(0));

    let (status, body) = get_status(
        fixture.app.clone(),
        &format!(
            "/compat/subsonic/rest/scrobble.view?{auth}&id={}&id={}&time=1000",
            fixture.track, fixture.quick
        ),
    )
    .await;

    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["subsonic-response"]["error"]["code"], json!(0));
    assert!(recorded_listens(&fixture.pool).await.is_empty());
    assert_eq!(counted_plays(&fixture.pool, &fixture.track).await, 0);
}

#[sqlx::test]
async fn subsonic_reporting_rejects_unknown_parameters_and_values(pool: PgPool) {
    let fixture = reporting_fixture(pool).await;
    let auth = auth_query("flynn", &fixture.secret);

    for uri in [
        subsonic_report(&auth, &fixture.track, "playing", 0, "&unknown=1"),
        subsonic_report(&auth, &fixture.track, "stalled", 0, ""),
        format!(
            "/compat/subsonic/rest/reportPlayback.view?{auth}&mediaId={}&mediaType=podcast&positionMs=0&state=playing",
            fixture.track
        ),
        format!(
            "/compat/subsonic/rest/reportPlayback.view?{auth}&mediaId={}&mediaType=song&positionMs=0&state=playing&ignoreScrobble=maybe",
            fixture.track
        ),
        format!(
            "/compat/subsonic/rest/scrobble.view?{auth}&id={}&submission=maybe",
            fixture.track
        ),
    ] {
        let (status, body) = get_status(fixture.app.clone(), &uri).await;

        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{uri}");
        assert_eq!(
            body["subsonic-response"]["status"],
            json!("failed"),
            "{uri}"
        );
        assert_eq!(
            body["subsonic-response"]["error"]["code"],
            json!(0),
            "{uri}"
        );
        assert_eq!(
            body["subsonic-response"]["error"]["message"],
            json!("Invalid parameter."),
            "{uri}"
        );
    }

    let (status, body) = get_status(
        fixture.app.clone(),
        &format!(
            "/compat/subsonic/rest/reportPlayback.view?{auth}&mediaId={}&positionMs=0&state=playing",
            fixture.track
        ),
    )
    .await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["subsonic-response"]["status"], json!("failed"));
    assert_eq!(body["subsonic-response"]["error"]["code"], json!(10));
    assert_eq!(
        body["subsonic-response"]["error"]["message"],
        json!("Required parameter is missing."),
        "an absent required parameter is missing, not invalid"
    );
    assert!(recorded_listens(&fixture.pool).await.is_empty());
}

#[sqlx::test]
async fn subsonic_advertises_only_the_extensions_it_serves(pool: PgPool) {
    let fixture = reporting_fixture(pool).await;
    let auth = auth_query("flynn", &fixture.secret);

    let body = get(
        fixture.app.clone(),
        &format!("/compat/subsonic/rest/getOpenSubsonicExtensions.view?{auth}"),
    )
    .await;
    let extensions = body["subsonic-response"]["openSubsonicExtensions"]
        .as_array()
        .unwrap();
    let names: Vec<&str> = extensions
        .iter()
        .map(|extension| extension["name"].as_str().unwrap())
        .collect();

    assert_eq!(names, ["indexBasedQueue", "playbackReport"]);
    assert_eq!(
        extensions
            .iter()
            .find(|extension| extension["name"] == json!("playbackReport"))
            .unwrap()["versions"],
        json!([1])
    );

    for state in ["starting", "playing", "paused", "stopped"] {
        let body = get(
            fixture.app.clone(),
            &subsonic_report(&auth, &fixture.track, state, 0, ""),
        )
        .await;

        assert_eq!(
            body["subsonic-response"]["status"],
            json!("ok"),
            "the advertised state {state} is served"
        );
    }
}

fn selected_facts() -> SourceFileFacts {
    SourceFileFacts {
        path: Some("Library/selected.flac".to_owned()),
        size_bytes: 4_000,
        modified_at: None,
        missing_at: None,
        content_type: Some("audio/flac".to_owned()),
        suffix: Some("flac".to_owned()),
        container: Some("flac".to_owned()),
        codec: Some("flac".to_owned()),
        bitrate_kbps: Some(900),
        sample_rate_hz: Some(44_100),
        bit_depth: Some(16),
        channels: Some(2),
        facts_version: 1,
        acoustid_fingerprint: None,
    }
}

fn representation_song(body: &Value, surface: &str, track: &str) -> Value {
    if surface == "getSong" || surface == "getSongPost" {
        return body["subsonic-response"]["song"].clone();
    }

    let songs = match surface {
        "search" | "searchQuery" => body["subsonic-response"]["searchResult3"]["song"].as_array(),
        "album" => body["subsonic-response"]["album"]["song"].as_array(),
        "directory" => body["subsonic-response"]["directory"]["child"].as_array(),
        "starred" => body["subsonic-response"]["starred2"]["song"].as_array(),
        "playlist" => body["subsonic-response"]["playlist"]["entry"].as_array(),
        "queue" => body["subsonic-response"]["playQueue"]["entry"].as_array(),
        "nowPlaying" => body["subsonic-response"]["nowPlaying"]["entry"].as_array(),
        other => panic!("unknown surface {other}"),
    };

    songs
        .and_then(|songs| songs.iter().find(|song| song["id"] == json!(track)))
        .cloned()
        .unwrap_or_else(|| panic!("{surface} does not serve the track"))
}

struct PayloadFixture {
    app: axum::Router,
    auth: String,
    track: TrackId,
    album: String,
}

async fn payload_fixture(pool: PgPool) -> PayloadFixture {
    let (service, secret) = support::credentials::database_credential_service(pool.clone()).await;
    let ports = StorePorts::new(pool.clone());
    let album = ports
        .store
        .resolve_or_create_album(
            &AlbumMetadata::new("Discovery".to_owned())
                .unwrap()
                .with_album_artist(ArtistCredit::new("Daft Punk".to_owned(), None).unwrap()),
        )
        .await
        .unwrap();
    let source = local_source("payload.flac");
    let track = ports
        .store
        .get_or_create(source.clone(), reporting_metadata("Selected", 200_000))
        .await
        .unwrap();
    ports
        .store
        .set_source_file_facts(&source, &selected_facts())
        .await
        .unwrap();
    ports
        .store
        .set_album_track(
            album.id(),
            track.id(),
            &AlbumTrack::new(Some(1), Some(2), 0).unwrap(),
        )
        .await
        .unwrap();
    let user_id: Uuid = sqlx::query_scalar("SELECT id FROM users WHERE username = 'flynn'")
        .fetch_one(&pool)
        .await
        .unwrap();
    ports
        .store
        .set_starred(user_id, StateTarget::Track(track.id()), true)
        .await
        .unwrap();

    let (search, library) = ports.bare_catalogue();
    let playlists = Arc::new(PlaylistService::new(
        Arc::clone(&ports.store) as Arc<dyn PlaylistStore>,
        Arc::clone(&library),
    ));
    let queue = Arc::new(RestoreQueueService::new(
        Arc::clone(&ports.store) as Arc<dyn RestoreQueueStore>
    ));
    let listens = Arc::new(ListenService::new(
        Arc::clone(&ports.store) as Arc<dyn ListenStore>,
        Arc::clone(&ports.store) as Arc<dyn LibraryRead>,
    ));
    let activity = Arc::new(PlaybackActivityService::new(listens, Arc::clone(&library)));
    let library_state = Arc::new(LibraryStateService::new(ports.state(), ports.library()));
    let playback = ports.streaming(vec![]);
    let app = whio_core::app::router(
        AppState::new(service, search, library, playback)
            .with_playlists(playlists)
            .with_queue(queue)
            .with_playback_activity(activity)
            .with_library_state(library_state),
    );

    PayloadFixture {
        app,
        auth: auth_query("flynn", &secret),
        track: track.id().clone(),
        album: album.id().to_string(),
    }
}

#[sqlx::test]
async fn subsonic_song_surfaces_share_representation_facts(pool: PgPool) {
    let fixture = payload_fixture(pool).await;
    let auth = &fixture.auth;
    let track = fixture.track.to_string();

    let body = get(
        fixture.app.clone(),
        &playlist_route(
            "createPlaylist",
            auth,
            &format!("name=Facts&songId={track}"),
        ),
    )
    .await;
    assert_eq!(body["subsonic-response"]["status"], json!("ok"));
    let playlist_id = playlist_id_of(&body);

    let body = get(
        fixture.app.clone(),
        &playlist_route(
            "savePlayQueue",
            auth,
            &format!("id={track}&current={track}"),
        ),
    )
    .await;
    assert_eq!(body["subsonic-response"]["status"], json!("ok"));

    let body = get(
        fixture.app.clone(),
        &playlist_route("scrobble", auth, &format!("id={track}&submission=false")),
    )
    .await;
    assert_eq!(body["subsonic-response"]["status"], json!("ok"));

    let (post_status, post_song) = post_status(
        fixture.app.clone(),
        &format!("/compat/subsonic/rest/getSong.view?{auth}&id={track}"),
        None,
        "",
    )
    .await;
    assert_eq!(post_status, StatusCode::OK);

    let surfaces = [
        ("getSongPost", post_song),
        (
            "search",
            get(fixture.app.clone(), &subsonic_search3("query=", auth)).await,
        ),
        (
            "searchQuery",
            get(
                fixture.app.clone(),
                &subsonic_search3("query=Selected", auth),
            )
            .await,
        ),
        (
            "getSong",
            get(
                fixture.app.clone(),
                &format!("/compat/subsonic/rest/getSong.view?{auth}&id={track}"),
            )
            .await,
        ),
        (
            "album",
            get(
                fixture.app.clone(),
                &format!(
                    "/compat/subsonic/rest/getAlbum.view?{auth}&id={}",
                    fixture.album
                ),
            )
            .await,
        ),
        (
            "directory",
            get(
                fixture.app.clone(),
                &format!(
                    "/compat/subsonic/rest/getMusicDirectory.view?{auth}&id={}",
                    fixture.album
                ),
            )
            .await,
        ),
        (
            "starred",
            get(
                fixture.app.clone(),
                &format!("/compat/subsonic/rest/getStarred2.view?{auth}"),
            )
            .await,
        ),
        (
            "playlist",
            get(
                fixture.app.clone(),
                &playlist_route("getPlaylist", auth, &format!("id={playlist_id}")),
            )
            .await,
        ),
        (
            "queue",
            get(
                fixture.app.clone(),
                &format!("/compat/subsonic/rest/getPlayQueue.view?{auth}"),
            )
            .await,
        ),
        (
            "nowPlaying",
            get(
                fixture.app.clone(),
                &format!("/compat/subsonic/rest/getNowPlaying.view?{auth}"),
            )
            .await,
        ),
    ];

    for (surface, body) in &surfaces {
        let song = representation_song(body, surface, &track);
        assert_eq!(song["contentType"], json!("audio/flac"), "{surface}");
        assert_eq!(song["suffix"], json!("flac"), "{surface}");
        assert_eq!(song["size"], json!(4_000), "{surface}");
        assert_eq!(song["bitRate"], json!(900), "{surface}");
        assert_eq!(song["samplingRate"], json!(44_100), "{surface}");
        assert_eq!(song["bitDepth"], json!(16), "{surface}");
        assert_eq!(song["channelCount"], json!(2), "{surface}");
    }
}

#[sqlx::test]
async fn subsonic_get_song_errors_follow_entity_shapes(pool: PgPool) {
    let fixture = payload_fixture(pool).await;
    let path = "/compat/subsonic/rest/getSong.view";

    let (status, body) = get_status(fixture.app.clone(), &format!("{path}?{}", fixture.auth)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["subsonic-response"]["error"]["code"], json!(10));

    let missing_track_id = Uuid::now_v7().to_string();
    for id in ["not-a-track", missing_track_id.as_str()] {
        let (status, body) = get_status(
            fixture.app.clone(),
            &format!("{path}?{}&id={id}", fixture.auth),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{id}");
        assert_eq!(body["subsonic-response"]["error"]["code"], json!(70));
    }

    let (status, body) = get_status(
        fixture.app,
        &format!(
            "{path}?u=flynn&v=1.16.1&c=test-client&p=forged&id={}",
            fixture.track
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["subsonic-response"]["error"]["code"], json!(40));
}

struct CountingPlayback {
    calls: Arc<AtomicUsize>,
}

#[async_trait]
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

#[sqlx::test]
async fn provider_only_subsonic_songs_use_an_unknown_representation(pool: PgPool) {
    let (service, secret) = support::credentials::database_credential_service(pool.clone()).await;
    let ports = StorePorts::new(pool.clone());
    let album = ports
        .store
        .resolve_or_create_album(
            &AlbumMetadata::new("Discovery".to_owned())
                .unwrap()
                .with_album_artist(ArtistCredit::new("Daft Punk".to_owned(), None).unwrap()),
        )
        .await
        .unwrap();
    let track = ports
        .store
        .get_or_create(
            SourceIdentity::new(
                ProviderId::new("youtube_music".to_owned()).unwrap(),
                SourceScope::Global,
                "vid-provider".to_owned(),
            )
            .unwrap(),
            reporting_metadata("Provider Only", 200_000),
        )
        .await
        .unwrap();
    ports
        .store
        .set_album_track(album.id(), track.id(), &AlbumTrack::unplaced())
        .await
        .unwrap();

    let calls = Arc::new(AtomicUsize::new(0));
    let (search, library) = ports.bare_catalogue();
    let playback = ports.streaming(vec![Arc::new(CountingPlayback {
        calls: Arc::clone(&calls),
    }) as Arc<dyn PlaybackProvider>]);
    let app = whio_core::app::router(AppState::new(service, search, library, playback));
    let auth = auth_query("flynn", &secret);
    let track_id = track.id().to_string();

    let surfaces = [
        (
            "search",
            get(app.clone(), &subsonic_search3("query=Provider", &auth)).await,
        ),
        (
            "getSong",
            get(
                app.clone(),
                &format!("/compat/subsonic/rest/getSong.view?{auth}&id={track_id}"),
            )
            .await,
        ),
        (
            "album",
            get(
                app.clone(),
                &format!(
                    "/compat/subsonic/rest/getAlbum.view?{auth}&id={}",
                    album.id()
                ),
            )
            .await,
        ),
    ];

    for (surface, body) in &surfaces {
        let song = representation_song(body, surface, &track_id);
        assert_eq!(
            song["contentType"],
            json!("application/octet-stream"),
            "{surface}"
        );
        assert!(song.get("suffix").is_none(), "{surface}");
        assert!(song.get("size").is_none(), "{surface}");
        assert!(song.get("bitRate").is_none(), "{surface}");
        assert!(song.get("samplingRate").is_none(), "{surface}");
        assert!(song.get("bitDepth").is_none(), "{surface}");
        assert!(song.get("channelCount").is_none(), "{surface}");
    }

    assert_eq!(
        calls.load(Ordering::SeqCst),
        0,
        "listing a provider-only song never resolves its media"
    );
}
