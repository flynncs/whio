mod support;

use std::sync::Arc;

use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use serde_json::{Value, json};
use tower::ServiceExt;
use whio_core::{
    playback::PlaybackProvider, providers::YtmAdapter, tracks::InMemoryTrackRepository,
};

use support::{StorePorts, credential_service_with_known_app_password, router};

fn app(credential: Arc<whio_core::identity::service::CredentialService>) -> axum::Router {
    let ports = StorePorts::new();
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
    let body = to_bytes(response.into_body(), 8192).await.unwrap();
    (status, serde_json::from_slice(&body).unwrap())
}

async fn browse_app(
    credential: Arc<whio_core::identity::service::CredentialService>,
) -> axum::Router {
    use whio_core::tracks::{
        AlbumMetadata, AlbumTrack, ArtistCredit, CatalogueWrite, ProviderId, SourceIdentity,
        SourceScope, TrackMetadata,
    };

    let local_source = |external_id: &str| {
        SourceIdentity::new(
            ProviderId::new("local".to_owned()).unwrap(),
            SourceScope::Named("/music".to_owned()),
            external_id.to_owned(),
        )
        .unwrap()
    };

    let repository = Arc::new(InMemoryTrackRepository::default());
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

#[tokio::test]
async fn subsonic_browse_lists_albums_artists_and_songs() {
    let (service, secret) = credential_service_with_known_app_password();
    let app = browse_app(service).await;
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

#[tokio::test]
async fn navidrome_browse_lists_albums_artists_and_songs() {
    let (service, secret) = credential_service_with_known_app_password();
    let app = browse_app(service).await;
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

#[tokio::test]
async fn ping_answers_under_both_compat_roots() {
    let (service, secret) = credential_service_with_known_app_password();

    for root in ["/compat/navidrome/rest", "/compat/subsonic/rest"] {
        let body = get(
            app(service.clone()),
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

#[tokio::test]
async fn login_with_known_app_password_returns_nd_payload() {
    let (service, secret) = credential_service_with_known_app_password();
    let app = app(service);

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

#[tokio::test]
async fn login_with_wrong_password_is_indistinguishable_from_unknown_user() {
    let (service, secret) = credential_service_with_known_app_password();

    for username in ["flynn", "nobody"] {
        let password = if username == "flynn" {
            "wrong"
        } else {
            &secret
        };
        let (status, body) = post_login(app(service.clone()), username, password).await;
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

#[tokio::test]
async fn song_list_rejects_missing_and_forged_tokens() {
    let (service, _) = credential_service_with_known_app_password();

    let request = Request::builder()
        .method("GET")
        .uri("/compat/navidrome/api/song")
        .header("x-request-id", "request-123")
        .body(Body::empty())
        .unwrap();
    let response = app(service.clone()).oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

    let (status, _, body) = get_authed(app(service), "/compat/navidrome/api/song", "forged").await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body, json!({"error": "Not authenticated"}));
}

#[tokio::test]
async fn song_list_returns_mapped_tracks_for_a_login_token() {
    use support::services;
    use whio_core::{
        catalogue::CatalogueCandidate,
        tracks::{ProviderId, SourceIdentity, SourceScope, TrackMetadata},
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
    let (search, library, playback) = services(vec![Ok(vec![candidate])]);
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

#[tokio::test]
async fn source_tags_label_the_artist_in_both_song_surfaces() {
    use support::{router_with_source_tags, services};
    use whio_core::{
        catalogue::CatalogueCandidate,
        compat::SourceTags,
        tracks::{ProviderId, SourceIdentity, SourceScope, TrackMetadata},
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
    let (search, library, playback) =
        services(vec![Ok(vec![candidate.clone()]), Ok(vec![candidate])]);
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

#[tokio::test]
async fn source_tags_reach_an_album_track_listing() {
    use support::router_with_source_tags;
    use whio_core::{
        compat::SourceTags,
        tracks::{
            AlbumMetadata, AlbumTrack, ArtistCredit, CatalogueWrite, ProviderId, SourceIdentity,
            SourceScope, TrackMetadata,
        },
    };

    let (service, secret) = credential_service_with_known_app_password();
    let repository = Arc::new(InMemoryTrackRepository::default());
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

#[tokio::test]
async fn song_detail_returns_the_searched_song() {
    use support::services;
    use whio_core::{
        catalogue::CatalogueCandidate,
        tracks::{ProviderId, SourceIdentity, SourceScope, TrackMetadata},
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
    let (search, library, playback) = services(vec![Ok(vec![candidate])]);
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

#[tokio::test]
async fn compat_answers_cors_preflight() {
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
        let response = app(service.clone()).oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT, "uri: {uri}");
        assert_eq!(
            response.headers()["access-control-allow-origin"],
            "*",
            "uri: {uri}"
        );
    }
}

#[tokio::test]
async fn real_client_view_suffix_reaches_handlers() {
    let (service, secret) = credential_service_with_known_app_password();
    let app = app(service);

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
