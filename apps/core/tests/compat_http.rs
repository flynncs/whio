#![cfg(feature = "postgres-integration")]

use sqlx::PgPool;
use whio_core::features::music_library::domain::{
    AlbumMetadata, AlbumTrack, ArtistCredit, ProviderId, SourceIdentity, SourceScope, StateTarget,
    TrackMetadata,
};
use whio_core::features::music_library::test_support::{
    CatalogueWrite, PostgresTrackRepository, StateStore,
};
mod support;

use std::sync::Arc;

use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use serde_json::{Value, json};
use tower::ServiceExt;
use whio_core::{features::playback::PlaybackProvider, features::playback::providers::YtmAdapter};

use support::credentials::credential_service_with_roles;
use support::{
    StorePorts, credential_service_with_known_app_password, router, router_with_completion,
    router_with_playlists,
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
    let request = Request::builder()
        .method("GET")
        .uri(uri)
        .header("x-request-id", "request-123")
        .body(Body::empty())
        .unwrap();

    let response = app.oneshot(request).await.unwrap();
    let status = response.status();
    let body = to_bytes(response.into_body(), 1 << 20).await.unwrap();
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
    assert_eq!(body[0]["name"], json!("Discovery"));
    assert_eq!(body[0]["albumArtist"], json!("Daft Punk"));
    assert_eq!(body[0]["songCount"], json!(2));
    assert!(body[0]["createdAt"].as_str().is_some_and(|v| !v.is_empty()));
    let album_id = body[0]["id"].as_str().unwrap();
    assert_eq!(body[0]["coverArt"], json!(format!("al-{album_id}")));

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
    use whio_core::{
        features::music_library::catalogue::CatalogueCandidate,
        features::music_library::domain::{ProviderId, SourceIdentity, SourceScope, TrackMetadata},
    };

    let (service, secret) = credential_service_with_known_app_password();
    let candidate = CatalogueCandidate::new(
        SourceIdentity::new(
            ProviderId::new("youtube".to_owned()).expect("valid provider"),
            SourceScope::Global,
            "vid-1".to_owned(),
        )
        .expect("valid source"),
        TrackMetadata::from_names("song".to_owned(), vec!["artist".to_owned()], Some(200_000))
            .expect("valid metadata"),
    );
    let (search, library, playback) = services(pool.clone(), vec![Ok(vec![candidate])]);
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

    let (status, _, body) = get_authed(app, "/compat/navidrome/api/song", token).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, json!([]));
}

#[sqlx::test]
async fn source_tags_label_the_artist_in_both_song_surfaces(pool: PgPool) {
    use support::{router_with_source_tags, services};
    use whio_core::{
        facades::SourceTags,
        features::music_library::catalogue::CatalogueCandidate,
        features::music_library::domain::{ProviderId, SourceIdentity, SourceScope, TrackMetadata},
    };

    let (service, secret) = credential_service_with_known_app_password();
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
    let (search, library, playback) = services(
        pool.clone(),
        vec![Ok(vec![candidate.clone()]), Ok(vec![candidate])],
    );
    let app = router_with_source_tags(service, search, library, playback, SourceTags::ArtistPrefix);
    let (_, login) = post_login(app.clone(), "flynn", &secret).await;
    let token = login["token"].as_str().unwrap();

    let (status, _, body) = get_authed(
        app.clone(),
        "/compat/navidrome/api/song?title=song&_start=0&_end=9",
        token,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body[0]["artist"], json!("[YTM] artist"));

    let search = get(
        app,
        &format!(
            "/compat/subsonic/rest/search3.view?u=flynn&v=1.16.1&c=test-client&p={secret}&query=song"
        ),
    )
    .await;
    assert_eq!(
        search["subsonic-response"]["searchResult3"]["song"][0]["artist"],
        json!("[YTM] artist")
    );
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
    use whio_core::{
        features::music_library::catalogue::CatalogueCandidate,
        features::music_library::domain::{ProviderId, SourceIdentity, SourceScope, TrackMetadata},
    };

    let (service, secret) = credential_service_with_known_app_password();
    let candidate = CatalogueCandidate::new(
        SourceIdentity::new(
            ProviderId::new("youtube".to_owned()).expect("valid provider"),
            SourceScope::Global,
            "vid-1".to_owned(),
        )
        .expect("valid source"),
        TrackMetadata::from_names("song".to_owned(), vec!["artist".to_owned()], Some(200_000))
            .expect("valid metadata"),
    );
    let (search, library, playback) = services(pool.clone(), vec![Ok(vec![candidate])]);
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
    let body = get(app, &subsonic_search3("query=&songCount=100", &auth)).await;

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
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["subsonic-response"]["status"], json!("failed"));
    assert!(body["subsonic-response"]["searchResult3"].is_null());
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
