mod support;

use std::sync::Arc;

use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use serde_json::{Value, json};
use tower::ServiceExt;
use whio_core::{
    catalogue::CatalogueService, playback::PlaybackService, resolver::DisabledResolver,
    tracks::InMemoryTrackRepository,
};

use support::{credential_service_with_known_app_password, playback_stream, router};

fn app(credential: Arc<whio_core::identity::service::CredentialService>) -> axum::Router {
    let resolver = Arc::new(DisabledResolver);
    let repository = Arc::new(InMemoryTrackRepository::default());
    let catalogue = Arc::new(CatalogueService::new(resolver.clone(), repository.clone()));
    let playback = Arc::new(PlaybackService::new(resolver, repository));
    let stream = playback_stream(Arc::clone(&playback));

    router(credential, catalogue, playback, stream)
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

    let body = to_bytes(response.into_body(), 8192).await.unwrap();
    serde_json::from_slice(&body).unwrap()
}

#[tokio::test]
async fn ping_answers_under_both_compat_roots() {
    let (service, secret) = credential_service_with_known_app_password();

    for root in ["/compat/navidrome/rest", "/compat/subsonic/rest"] {
        let body = get(
            app(service.clone()),
            &format!("{root}/ping?u=flynn&v=1.16.1&c=test-client&p={secret}"),
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
        &format!("/compat/navidrome/rest/ping?u=flynn&v=1.16.1&c=test-client&t={token}&s={salt}"),
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
    let body = to_bytes(response.into_body(), 8192).await.unwrap();
    (status, serde_json::from_slice(&body).unwrap())
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

    let (status, body) = get_authed(app(service), "/compat/navidrome/api/song", "forged").await;
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
        TrackMetadata::new("song".to_owned(), vec!["artist".to_owned()], Some(200_000))
            .expect("valid metadata"),
    );
    let (catalogue, playback, stream) = services(vec![Ok(vec![candidate])]);
    let app = router(service.clone(), catalogue, playback, stream);

    let (_, login) = post_login(app.clone(), "flynn", &secret).await;
    let token = login["token"].as_str().unwrap();

    let (status, body) = get_authed(
        app.clone(),
        "/compat/navidrome/api/song?filter=%7B%22q%22%3A%22song%22%7D",
        token,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body.as_array().unwrap().len(), 1);
    assert_eq!(body[0]["title"], json!("song"));
    assert_eq!(body[0]["artist"], json!("artist"));
    assert_eq!(body[0]["duration"], json!(200.0));
    assert!(body[0]["id"].as_str().is_some_and(|v| !v.is_empty()));

    let (status, body) = get_authed(app, "/compat/navidrome/api/song", token).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, json!([]));
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
