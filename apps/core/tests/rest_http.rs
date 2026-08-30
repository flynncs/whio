use std::sync::Arc;

mod support;

use support::{
    credential_service_with_known_api_key, credential_service_with_known_app_password,
    playback_stream, router,
};

use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use md5::{Digest, Md5};
use serde_json::{Value, json};
use tower::ServiceExt;
use whio_core::{
    catalogue::CatalogueService, identity::secrets::generate_app_secret, playback::PlaybackService,
    resolver::DisabledResolver, tracks::InMemoryTrackRepository,
};

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

fn failure(code: u16) -> Value {
    json!({
        "subsonic-response": {
            "status": "failed",
            "version": "1.16.1",
            "type": "whio",
            "serverVersion": "0.1.0",
            "openSubsonic": true,
            "error": {
                "code": code,
                "message": match code {
                    0 => "Unsupported operation.",
                    10 => "Required parameter is missing.",
                    40 => "Wrong username or password.",
                    43 => "Multiple conflicting authentication mechanisms provided.",
                    _ => unreachable!("no fixture message for this code"),
                }
            }
        }
    })
}

#[tokio::test]
async fn ping_without_auth_params_reports_missing_parameter() {
    let response = get(
        app(support::credential_service()),
        "/rest/ping?v=1.16.1&c=test-client",
    )
    .await;

    assert_eq!(response, failure(10));
}

#[tokio::test]
async fn ping_without_any_mechanism_reports_missing_parameter() {
    let response = get(
        app(support::credential_service()),
        "/rest/ping?u=flynn&v=1.16.1&c=test-client",
    )
    .await;

    assert_eq!(response, failure(10));
}

#[tokio::test]
async fn ping_with_half_a_token_pair_reports_missing_parameter() {
    let response = get(
        app(support::credential_service()),
        "/rest/ping?u=flynn&v=1.16.1&c=test-client&t=abc123",
    )
    .await;

    assert_eq!(response, failure(10));
}

#[tokio::test]
async fn ping_with_conflicting_mechanisms_reports_code_43() {
    let response = get(
        app(support::credential_service()),
        "/rest/ping?u=flynn&v=1.16.1&c=test-client&p=whio_one&apiKey=whio_two",
    )
    .await;

    assert_eq!(response, failure(43));
}

#[tokio::test]
async fn ping_with_unknown_api_key_is_indistinguishable_from_wrong_password() {
    let (service, _known) = credential_service_with_known_api_key();
    let unknown = generate_app_secret();
    let response = get(
        app(service),
        &format!("/rest/ping?u=flynn&v=1.16.1&c=test-client&apiKey={unknown}"),
    )
    .await;

    assert_eq!(response, failure(40));
}

#[tokio::test]
async fn ping_with_known_api_key_succeeds() {
    let (service, secret) = credential_service_with_known_api_key();
    let response = get(
        app(service),
        &format!("/rest/ping?u=flynn&v=1.16.1&c=test-client&apiKey={secret}"),
    )
    .await;

    assert_eq!(
        response,
        json!({
            "subsonic-response": {
                "status": "ok",
                "version": "1.16.1",
                "type": "whio",
                "serverVersion": "0.1.0",
                "openSubsonic": true
            }
        })
    );
}

#[tokio::test]
async fn ping_accepts_salted_token_of_a_stored_app_password() {
    let (service, secret) = credential_service_with_known_app_password();
    let salt = "abcdef123456";

    let mut hasher = Md5::new();
    hasher.update(secret.as_bytes());
    hasher.update(salt.as_bytes());
    let token: String = hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();

    let response = get(
        app(service),
        &format!("/rest/ping?u=flynn&v=1.16.1&c=test-client&t={token}&s={salt}"),
    )
    .await;

    assert_eq!(
        response["subsonic-response"]["status"],
        json!("ok"),
        "full response: {response}"
    );
}

#[tokio::test]
async fn ping_accepts_enc_prefixed_password() {
    let (service, secret) = credential_service_with_known_app_password();
    let hex_encoded: String = secret.bytes().map(|byte| format!("{byte:02x}")).collect();

    let response = get(
        app(service),
        &format!("/rest/ping?u=flynn&v=1.16.1&c=test-client&p=enc:{hex_encoded}"),
    )
    .await;

    assert_eq!(response["subsonic-response"]["status"], json!("ok"));
}

#[tokio::test]
async fn open_subsonic_extensions_requires_authentication() {
    let response = get(
        app(support::credential_service()),
        "/rest/getOpenSubsonicExtensions",
    )
    .await;

    assert_eq!(response, failure(10));
}

#[tokio::test]
async fn open_subsonic_extensions_returns_an_empty_list() {
    let (service, secret) = credential_service_with_known_api_key();
    let response = get(
        app(service),
        &format!("/rest/getOpenSubsonicExtensions?u=flynn&v=1.16.1&c=test-client&apiKey={secret}"),
    )
    .await;

    assert_eq!(
        response,
        json!({
            "subsonic-response": {
                "status": "ok",
                "version": "1.16.1",
                "type": "whio",
                "serverVersion": "0.1.0",
                "openSubsonic": true,
                "openSubsonicExtensions": []
            }
        })
    );
}

#[tokio::test]
async fn unknown_rest_endpoints_answer_with_a_subsonic_envelope_not_product_json() {
    // clients probe unimplemented endpoints during discovery; the compat plane
    // must speak subsonic even there
    let response = get(
        app(support::credential_service()),
        "/rest/getArtists?u=flynn&v=1.16.1&c=test-client&apiKey=whio_whatever",
    )
    .await;

    assert_eq!(response, failure(0));
}
