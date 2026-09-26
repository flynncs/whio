#![cfg(feature = "postgres-integration")]

use sqlx::PgPool;
use std::sync::Arc;

mod support;

use support::{
    StorePorts, credential_service_with_known_api_key, credential_service_with_known_app_password,
    router,
};

use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use md5::{Digest, Md5};
use serde_json::{Value, json};
use tower::ServiceExt;
use whio_core::{
    features::identity::secrets::generate_app_secret, features::playback::PlaybackProvider,
    features::playback::providers::YtmAdapter,
};

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

#[sqlx::test]
async fn ping_without_auth_params_reports_missing_parameter(pool: PgPool) {
    let response = get(
        app(pool.clone(), support::credential_service()),
        "/rest/ping.view?v=1.16.1&c=test-client",
    )
    .await;

    assert_eq!(response, failure(10));
}

#[sqlx::test]
async fn ping_without_any_mechanism_reports_missing_parameter(pool: PgPool) {
    let response = get(
        app(pool.clone(), support::credential_service()),
        "/rest/ping.view?u=flynn&v=1.16.1&c=test-client",
    )
    .await;

    assert_eq!(response, failure(10));
}

#[sqlx::test]
async fn ping_with_half_a_token_pair_reports_missing_parameter(pool: PgPool) {
    let response = get(
        app(pool.clone(), support::credential_service()),
        "/rest/ping.view?u=flynn&v=1.16.1&c=test-client&t=abc123",
    )
    .await;

    assert_eq!(response, failure(10));
}

#[sqlx::test]
async fn ping_with_conflicting_mechanisms_reports_code_43(pool: PgPool) {
    let response = get(
        app(pool.clone(), support::credential_service()),
        "/rest/ping.view?u=flynn&v=1.16.1&c=test-client&p=whio_one&apiKey=whio_two",
    )
    .await;

    assert_eq!(response, failure(43));
}

#[sqlx::test]
async fn ping_with_unknown_api_key_is_indistinguishable_from_wrong_password(pool: PgPool) {
    let (service, _known) = credential_service_with_known_api_key();
    let unknown = generate_app_secret();
    let response = get(
        app(pool.clone(), service),
        &format!("/rest/ping.view?u=flynn&v=1.16.1&c=test-client&apiKey={unknown}"),
    )
    .await;

    assert_eq!(response, failure(40));
}

#[sqlx::test]
async fn ping_with_known_api_key_succeeds(pool: PgPool) {
    let (service, secret) = credential_service_with_known_api_key();
    let response = get(
        app(pool.clone(), service),
        &format!("/rest/ping.view?u=flynn&v=1.16.1&c=test-client&apiKey={secret}"),
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

#[sqlx::test]
async fn ping_accepts_salted_token_of_a_stored_app_password(pool: PgPool) {
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
        app(pool.clone(), service),
        &format!("/rest/ping.view?u=flynn&v=1.16.1&c=test-client&t={token}&s={salt}"),
    )
    .await;

    assert_eq!(
        response["subsonic-response"]["status"],
        json!("ok"),
        "full response: {response}"
    );
}

#[sqlx::test]
async fn ping_accepts_enc_prefixed_password(pool: PgPool) {
    let (service, secret) = credential_service_with_known_app_password();
    let hex_encoded: String = secret.bytes().map(|byte| format!("{byte:02x}")).collect();

    let response = get(
        app(pool.clone(), service),
        &format!("/rest/ping.view?u=flynn&v=1.16.1&c=test-client&p=enc:{hex_encoded}"),
    )
    .await;

    assert_eq!(response["subsonic-response"]["status"], json!("ok"));
}

#[sqlx::test]
async fn open_subsonic_extensions_requires_authentication(pool: PgPool) {
    let response = get(
        app(pool.clone(), support::credential_service()),
        "/rest/getOpenSubsonicExtensions.view",
    )
    .await;

    assert_eq!(response, failure(10));
}

#[sqlx::test]
async fn open_subsonic_extensions_returns_an_empty_list(pool: PgPool) {
    let (service, secret) = credential_service_with_known_api_key();
    let response = get(
        app(pool.clone(), service),
        &format!(
            "/rest/getOpenSubsonicExtensions.view?u=flynn&v=1.16.1&c=test-client&apiKey={secret}"
        ),
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

#[sqlx::test]
async fn unknown_rest_endpoints_answer_with_a_subsonic_envelope_not_product_json(pool: PgPool) {
    // clients probe unimplemented endpoints during discovery; the compat plane
    // must speak subsonic even there
    let response = get(
        app(pool.clone(), support::credential_service()),
        "/rest/getIndexes.view?u=flynn&v=1.16.1&c=test-client&apiKey=whio_whatever",
    )
    .await;

    assert_eq!(response, failure(0));
}

#[sqlx::test]
async fn revoked_credentials_still_fail_authentication(pool: PgPool) {
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
        key,
    ));
    let app = app(pool, service);
    let auth = format!("u=flynn&v=1.16.1&c=test-client&p={secret}");

    let response = get(app.clone(), &format!("/rest/ping.view?{auth}")).await;
    assert_eq!(response["subsonic-response"]["status"], json!("ok"));

    assert!(provisioning.revoke_credential(credential_id).await.unwrap());

    let response = get(app, &format!("/rest/ping.view?{auth}")).await;
    assert_eq!(response, failure(40));
}
