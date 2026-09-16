//! Protocol translation only.

use axum::{
    http::StatusCode,
    middleware,
    response::{IntoResponse, Response},
    routing::{get, post},
    Extension, Json, Router,
};
use serde::{Deserialize, Serialize};

use crate::{
    errors::ConnectorError,
    request_id::{self, RequestId},
    subsonic,
};

#[derive(Serialize)]
struct HealthBody {
    status: &'static str,
}

#[derive(Serialize)]
struct CapabilitiesBody {
    contract_version: &'static str,
    providers: Vec<ProviderBody>,
}

#[derive(Serialize)]
struct ProviderBody {
    provider_id: &'static str,
    capabilities: Vec<&'static str>,
}

#[derive(Serialize)]
struct ErrorBody {
    code: &'static str,
    message: &'static str,
    request_id: String,
}

#[derive(Deserialize)]
struct ListUsersRequest {
    base_url: String,
    admin_username: String,
    admin_password: String,
}

#[derive(Serialize)]
struct ListUsersResponse {
    usernames: Vec<String>,
}

#[derive(Deserialize)]
struct VerifyRequest {
    base_url: String,
    username: String,
    password: String,
}

#[derive(Serialize)]
struct VerifyResponse {
    verified: bool,
}

#[derive(Deserialize)]
struct SearchRequest {
    base_url: String,
    username: String,
    password: String,
    query: String,
    limit: u32,
    #[serde(default)]
    offset: u32,
}

#[derive(Serialize)]
struct SearchResponse {
    results: Vec<SearchResult>,
    #[serde(skip_serializing_if = "Option::is_none")]
    next_offset: Option<u32>,
}

#[derive(Serialize)]
struct SearchResult {
    external_id: String,
    title: String,
    artists: Vec<String>,
    duration_ms: Option<u64>,
}

#[derive(Deserialize)]
struct ResolveRequest {
    base_url: String,
    username: String,
    password: String,
    external_id: String,
}

#[derive(Serialize)]
struct ResolveResponse {
    url: String,
}

async fn liveness() -> impl IntoResponse {
    Json(HealthBody { status: "ok" })
}

async fn readiness() -> impl IntoResponse {
    Json(HealthBody { status: "ready" })
}

async fn capabilities() -> impl IntoResponse {
    Json(CapabilitiesBody {
        contract_version: "1",
        providers: vec![ProviderBody {
            provider_id: "navidrome",
            capabilities: vec![
                "upstream.users.list",
                "upstream.credentials.verify",
                "catalogue.search",
                "playback.resolve",
            ],
        }],
    })
}

async fn not_found(Extension(request_id): Extension<RequestId>) -> impl IntoResponse {
    (
        StatusCode::NOT_FOUND,
        Json(ErrorBody {
            code: "not_found",
            message: "Not found.",
            request_id: request_id.0,
        }),
    )
}

async fn list_users(
    Extension(request_id): Extension<RequestId>,
    Extension(client): Extension<reqwest::Client>,
    body: Result<Json<ListUsersRequest>, axum::extract::rejection::JsonRejection>,
) -> Response {
    let Json(request) = match body {
        Ok(body) => body,
        Err(_) => return ConnectorError::InvalidRequest.response(&request_id),
    };

    match subsonic::fetch_usernames(
        &client,
        &request.base_url,
        &request.admin_username,
        &request.admin_password,
    )
    .await
    {
        Ok(usernames) => Json(ListUsersResponse { usernames }).into_response(),
        Err(error) => {
            tracing::warn!(
                request_id = request_id.0.as_str(),
                "upstream user listing failed"
            );
            error.response(&request_id)
        }
    }
}
async fn verify(
    Extension(request_id): Extension<RequestId>,
    Extension(client): Extension<reqwest::Client>,
    body: Result<Json<VerifyRequest>, axum::extract::rejection::JsonRejection>,
) -> Response {
    let Json(request) = match body {
        Ok(body) => body,
        Err(_) => return ConnectorError::InvalidRequest.response(&request_id),
    };

    match subsonic::verify_credentials(
        &client,
        &request.base_url,
        &request.username,
        &request.password,
    )
    .await
    {
        Ok(()) => Json(VerifyResponse { verified: true }).into_response(),
        Err(error) => {
            tracing::warn!(
                request_id = request_id.0.as_str(),
                "upstream credential verification failed"
            );
            error.response(&request_id)
        }
    }
}

async fn search(
    Extension(request_id): Extension<RequestId>,
    Extension(client): Extension<reqwest::Client>,
    body: Result<Json<SearchRequest>, axum::extract::rejection::JsonRejection>,
) -> Response {
    let Json(request) = match body {
        Ok(body) => body,
        Err(_) => return ConnectorError::InvalidRequest.response(&request_id),
    };
    if request.query.trim().is_empty()
        || request.limit == 0
        || request.limit > 25
        || request.offset > 1000
    {
        return ConnectorError::InvalidRequest.response(&request_id);
    }

    match subsonic::search_songs(
        &client,
        &request.base_url,
        &request.username,
        &request.password,
        &request.query,
        request.limit,
        request.offset,
    )
    .await
    {
        Ok(page) => Json(SearchResponse {
            next_offset: page.next_offset,
            results: page
                .songs
                .into_iter()
                .map(|song| SearchResult {
                    external_id: song.id,
                    title: song.title,
                    artists: vec![song.artist],
                    duration_ms: song.duration_ms,
                })
                .collect(),
        })
        .into_response(),
        Err(error) => {
            tracing::warn!(request_id = request_id.0.as_str(), "upstream search failed");
            error.response(&request_id)
        }
    }
}

async fn resolve(
    Extension(request_id): Extension<RequestId>,
    Extension(client): Extension<reqwest::Client>,
    body: Result<Json<ResolveRequest>, axum::extract::rejection::JsonRejection>,
) -> Response {
    let Json(request) = match body {
        Ok(body) => body,
        Err(_) => return ConnectorError::InvalidRequest.response(&request_id),
    };
    if request.external_id.trim().is_empty() {
        return ConnectorError::InvalidRequest.response(&request_id);
    }

    match subsonic::resolve_stream_url(
        &client,
        &request.base_url,
        &request.username,
        &request.password,
        &request.external_id,
    )
    .await
    {
        Ok(url) => Json(ResolveResponse { url }).into_response(),
        Err(error) => {
            tracing::warn!(
                request_id = request_id.0.as_str(),
                "upstream resolve failed"
            );
            error.response(&request_id)
        }
    }
}

fn http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .expect("tls backend initializes")
}

pub(crate) fn router() -> Router {
    Router::new()
        .route("/health/live", get(liveness))
        .route("/health/ready", get(readiness))
        .route("/v1/capabilities", get(capabilities))
        .route("/v1/admin/users/list", post(list_users))
        .route("/v1/admin/servers/verify", post(verify))
        .route("/v1/catalogue/search", post(search))
        .route("/v1/playback/resolve", post(resolve))
        .fallback(not_found)
        .layer(Extension(http_client()))
        .layer(middleware::from_fn(request_id::middleware))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        body::{to_bytes, Body},
        http::{HeaderMap, Request},
    };
    use tower::ServiceExt;

    async fn get(uri: &str) -> (StatusCode, HeaderMap, serde_json::Value) {
        let request = Request::builder()
            .uri(uri)
            .header("x-request-id", "request-123")
            .body(Body::empty())
            .unwrap();
        let response = router().oneshot(request).await.unwrap();
        let status = response.status();
        let headers = response.headers().clone();
        let body = to_bytes(response.into_body(), 8192).await.unwrap();
        (status, headers, serde_json::from_slice(&body).unwrap())
    }

    async fn post_list(body: serde_json::Value) -> (StatusCode, serde_json::Value) {
        let request = Request::builder()
            .method("POST")
            .uri("/v1/admin/users/list")
            .header("content-type", "application/json")
            .header("x-request-id", "request-123")
            .body(Body::from(body.to_string()))
            .unwrap();
        let response = router().oneshot(request).await.unwrap();
        let status = response.status();
        let bytes = to_bytes(response.into_body(), 8192).await.unwrap();
        (status, serde_json::from_slice(&bytes).unwrap())
    }

    async fn post_verify(body: serde_json::Value) -> (StatusCode, serde_json::Value) {
        let request = Request::builder()
            .method("POST")
            .uri("/v1/admin/servers/verify")
            .header("content-type", "application/json")
            .header("x-request-id", "request-123")
            .body(Body::from(body.to_string()))
            .unwrap();
        let response = router().oneshot(request).await.unwrap();
        let status = response.status();
        let bytes = to_bytes(response.into_body(), 8192).await.unwrap();
        (status, serde_json::from_slice(&bytes).unwrap())
    }

    #[tokio::test]
    async fn verify_maps_upstream_outcomes() {
        use axum::{extract::Query, routing::get as route_get};
        use std::collections::HashMap;

        async fn stub(Query(params): Query<HashMap<String, String>>) -> Response {
            match params.get("u").map(String::as_str) {
                Some("ghost") => (
                    StatusCode::OK,
                    r#"{"subsonic-response": {"status": "failed", "error": {"code": 40, "message": "wrong"}}}"#,
                )
                    .into_response(),
                _ => (
                    StatusCode::OK,
                    r#"{"subsonic-response": {"status": "ok", "version": "1.16.1"}}"#,
                )
                    .into_response(),
            }
        }

        let upstream = Router::new().route("/rest/ping.view", route_get(stub));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, upstream).await });
        let base_url = format!("http://{address}");

        let credentials = |username: &str| {
            serde_json::json!({
                "base_url": base_url,
                "username": username,
                "password": "password",
            })
        };

        let (status, body) = post_verify(credentials("friend")).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["verified"], true);
        let (status, body) = post_verify(credentials("ghost")).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(body["code"], "upstream_auth_failed");
    }

    #[tokio::test]
    async fn health_endpoints_answer_with_request_ids() {
        for (uri, status_value) in [("/health/live", "ok"), ("/health/ready", "ready")] {
            let (status, headers, body) = get(uri).await;
            assert_eq!(status, StatusCode::OK);
            assert_eq!(body["status"], status_value);
            assert_eq!(headers["x-request-id"], "request-123");
        }
    }

    #[tokio::test]
    async fn capabilities_advertise_user_listing() {
        let (status, _, body) = get("/v1/capabilities").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["contract_version"], "1");
        assert_eq!(body["providers"][0]["provider_id"], "navidrome");
        assert_eq!(
            body["providers"][0]["capabilities"],
            serde_json::json!([
                "upstream.users.list",
                "upstream.credentials.verify",
                "catalogue.search",
                "playback.resolve"
            ])
        );
    }

    #[tokio::test]
    async fn unknown_paths_return_a_safe_envelope() {
        let (status, _, body) = get("/v1/search").await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(body["code"], "not_found");
        assert_eq!(body["request_id"], "request-123");
    }

    #[tokio::test]
    async fn missing_request_id_is_generated() {
        let request = Request::builder()
            .uri("/health/live")
            .body(Body::empty())
            .unwrap();
        let response = router().oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert!(response.headers()["x-request-id"]
            .to_str()
            .is_ok_and(|value| !value.is_empty()));
    }

    #[tokio::test]
    async fn list_users_maps_upstream_outcomes() {
        use axum::{extract::Query, routing::get as route_get};
        use std::collections::HashMap;

        async fn stub(Query(params): Query<HashMap<String, String>>) -> Response {
            match params.get("u").map(String::as_str) {
                Some("ghost") => (
                    StatusCode::OK,
                    r#"{"subsonic-response": {"status": "failed", "error": {"code": 40, "message": "wrong"}}}"#,
                )
                    .into_response(),
                Some("plain") => (
                    StatusCode::OK,
                    r#"{"subsonic-response": {"status": "failed", "error": {"code": 50, "message": "admin"}}}"#,
                )
                    .into_response(),
                Some("garbled") => (StatusCode::OK, "<html>nope</html>").into_response(),
                _ => (
                    StatusCode::OK,
                    r#"{"subsonic-response": {"status": "ok", "users": {"user": [{"username": "admin"}, {"username": "friend"}]}}}"#,
                )
                    .into_response(),
            }
        }

        let upstream = Router::new().route("/rest/getUsers.view", route_get(stub));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, upstream).await });
        let base_url = format!("http://{address}");

        let credentials = |username: &str| {
            serde_json::json!({
                "base_url": base_url,
                "admin_username": username,
                "admin_password": "password",
            })
        };

        let (status, body) = post_list(credentials("admin")).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["usernames"], serde_json::json!(["admin", "friend"]));

        let (status, body) = post_list(credentials("ghost")).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(body["code"], "upstream_auth_failed");
        assert_eq!(body["request_id"], "request-123");

        let (status, body) = post_list(credentials("plain")).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_eq!(body["code"], "upstream_forbidden");

        let (status, body) = post_list(credentials("garbled")).await;
        assert_eq!(status, StatusCode::BAD_GATEWAY);
        assert_eq!(body["code"], "upstream_bad_response");

        let (status, body) = post_list(serde_json::json!({
            "base_url": "http://music.example.test",
            "admin_username": "admin",
            "admin_password": "password",
        }))
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["code"], "invalid_request");

        let (status, body) = post_list(serde_json::json!({
            "base_url": "http://127.0.0.1:1",
            "admin_username": "admin",
            "admin_password": "password",
        }))
        .await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(body["code"], "provider_unavailable");

        let request = Request::builder()
            .method("POST")
            .uri("/v1/admin/users/list")
            .header("content-type", "application/json")
            .header("x-request-id", "request-123")
            .body(Body::from("{oops"))
            .unwrap();
        let response = router().oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn search_returns_upstream_songs() {
        use axum::{extract::Query, routing::get as route_get};
        use std::collections::HashMap;

        async fn stub(Query(params): Query<HashMap<String, String>>) -> Response {
            if params
                .get("songOffset")
                .is_some_and(|offset| offset == "25")
            {
                assert_eq!(params.get("songCount").map(String::as_str), Some("1"));
                return (
                    StatusCode::OK,
                    r#"{"subsonic-response":{"status":"ok","searchResult3":{"song":[{"id":"song-26","title":"Later song","artist":"Artist"}]}}}"#,
                ).into_response();
            }
            if params.get("u").is_some_and(|user| user == "ghost") {
                return (
                    StatusCode::OK,
                    r#"{"subsonic-response": {"status": "failed", "error": {"code": 40, "message": "wrong"}}}"#,
                )
                    .into_response();
            }
            (
                StatusCode::OK,
                r#"{"subsonic-response": {"status": "ok", "searchResult3": {"song": [{"id": "song-1", "title": "No One Noticed", "artist": "The Marias", "duration": 237}, {"id": "song-2", "title": "Sienna", "artist": "The Marias"}]}}}"#,
            )
                .into_response()
        }

        let upstream = Router::new().route("/rest/search3.view", route_get(stub));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, upstream).await });
        let base_url = format!("http://{address}");

        let search = |username: &str, query: &str, limit: u32| {
            serde_json::json!({
                "base_url": base_url,
                "username": username,
                "password": "password",
                "query": query,
                "limit": limit,
            })
        };
        let post_search = |body: serde_json::Value| async move {
            let request = Request::builder()
                .method("POST")
                .uri("/v1/catalogue/search")
                .header("content-type", "application/json")
                .header("x-request-id", "request-123")
                .body(Body::from(body.to_string()))
                .unwrap();
            let response = router().oneshot(request).await.unwrap();
            let status = response.status();
            let bytes = to_bytes(response.into_body(), 8192).await.unwrap();
            (
                status,
                serde_json::from_slice::<serde_json::Value>(&bytes).unwrap(),
            )
        };

        let (status, body) = post_search(search("friend", "marias", 10)).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            body["results"],
            serde_json::json!([
                {"external_id": "song-1", "title": "No One Noticed", "artists": ["The Marias"], "duration_ms": 237000},
                {"external_id": "song-2", "title": "Sienna", "artists": ["The Marias"], "duration_ms": null},
            ])
        );

        let (status, body) = post_search(search("ghost", "marias", 10)).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(body["code"], "upstream_auth_failed");

        let (status, body) = post_search(search("friend", "  ", 10)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["code"], "invalid_request");

        let (status, body) = post_search(search("friend", "marias", 26)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["code"], "invalid_request");

        let mut later = search("friend", "marias", 1);
        later["offset"] = serde_json::json!(25);
        let (status, body) = post_search(later).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["results"][0]["external_id"], "song-26");
        assert_eq!(body["next_offset"], 26);

        let mut invalid = search("friend", "marias", 1);
        invalid["offset"] = serde_json::json!(1001);
        let (status, _) = post_search(invalid).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn resolve_returns_probed_stream_url() {
        use axum::{extract::Query, routing::get as route_get};
        use std::collections::HashMap;

        async fn stub(Query(params): Query<HashMap<String, String>>) -> Response {
            let json = [("content-type", "application/json")];
            match params.get("id").map(String::as_str) {
                Some("ghost") => (
                    StatusCode::OK,
                    json,
                    r#"{"subsonic-response": {"status": "failed", "error": {"code": 40, "message": "wrong"}}}"#,
                )
                    .into_response(),
                Some("gone") => (
                    StatusCode::NOT_FOUND,
                    json,
                    r#"{"subsonic-response": {"status": "failed", "error": {"code": 70, "message": "gone"}}}"#,
                )
                    .into_response(),
                _ => (StatusCode::PARTIAL_CONTENT, "audio-bytes").into_response(),
            }
        }

        let upstream = Router::new().route("/rest/stream.view", route_get(stub));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, upstream).await });
        let base_url = format!("http://{address}");

        let resolve = |id: &str| {
            serde_json::json!({
                "base_url": base_url,
                "username": "friend",
                "password": "password",
                "external_id": id,
            })
        };
        let post_resolve = |body: serde_json::Value| async move {
            let request = Request::builder()
                .method("POST")
                .uri("/v1/playback/resolve")
                .header("content-type", "application/json")
                .header("x-request-id", "request-123")
                .body(Body::from(body.to_string()))
                .unwrap();
            let response = router().oneshot(request).await.unwrap();
            let status = response.status();
            let bytes = to_bytes(response.into_body(), 8192).await.unwrap();
            (
                status,
                serde_json::from_slice::<serde_json::Value>(&bytes).unwrap(),
            )
        };

        let (status, body) = post_resolve(resolve("song-1")).await;
        assert_eq!(status, StatusCode::OK);
        assert!(body["url"]
            .as_str()
            .is_some_and(|url| url.contains("/rest/stream.view") && url.contains("id=song-1")));

        let (status, body) = post_resolve(resolve("ghost")).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(body["code"], "upstream_auth_failed");

        let (status, body) = post_resolve(resolve("gone")).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(body["code"], "source_not_found");

        let (status, body) = post_resolve(resolve("")).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["code"], "invalid_request");
    }
}
