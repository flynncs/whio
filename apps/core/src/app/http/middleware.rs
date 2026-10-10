use std::{net::SocketAddr, sync::Arc, time::Instant};

use axum::{
    extract::{ConnectInfo, Extension, MatchedPath, Request, State},
    http::HeaderValue,
    middleware::Next,
    response::Response as AxumResponse,
};
use tracing::Instrument;

use crate::platform::{
    client_ip::{self, ClientIpPolicy},
    request::{RequestContext, RequestId},
};

fn subsonic_client(query: Option<&str>) -> Option<String> {
    let (_, client) = form_urlencoded::parse(query?.as_bytes()).find(|(key, _)| key == "c")?;
    let client = client.into_owned();
    (1..=64)
        .contains(&client.len())
        .then_some(client)
        .filter(|client| {
            client.bytes().all(|byte| {
                byte.is_ascii_alphanumeric() || matches!(byte, b' ' | b'.' | b'_' | b'-')
            })
        })
}

pub(crate) async fn request_id(
    State(policy): State<Arc<ClientIpPolicy>>,
    peer: Option<Extension<ConnectInfo<SocketAddr>>>,
    mut request: Request,
    next: Next,
) -> AxumResponse {
    let request_id = request
        .headers()
        .get("x-request-id")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| uuid::Uuid::parse_str(value).ok())
        .map(|value| RequestId::new(value.to_string()).expect("UUID is a valid request ID"))
        .unwrap_or_else(RequestId::generate);

    let context = match peer {
        Some(Extension(ConnectInfo(peer))) => RequestContext::new(request_id.clone())
            .with_client_ip(client_ip::client_ip(
                peer,
                request.headers().get("x-forwarded-for"),
                &policy,
            )),
        None => RequestContext::new(request_id.clone()),
    };
    let client_ip = context.client_ip();
    request.extensions_mut().insert(context);
    let method = request.method().clone();
    let route = request
        .extensions()
        .get::<MatchedPath>()
        .map(|path| path.as_str().to_owned())
        .unwrap_or_else(|| "<unmatched>".to_owned());
    let query = request
        .uri()
        .query()
        .map(crate::platform::redact::redact_query);
    let subsonic_client = subsonic_client(request.uri().query());
    let client_ip = client_ip.map(|ip| ip.to_string());
    let health = route.starts_with("/health/");
    let span = tracing::info_span!(
        "http_request",
        request_id = %request_id.as_str(),
        method = %method,
        route = %route,
        query = tracing::field::Empty,
        status = tracing::field::Empty,
        latency_ms = tracing::field::Empty,
        client_ip = tracing::field::Empty,
        subsonic_client = tracing::field::Empty,
        subsonic_code = tracing::field::Empty,
    );
    if let Some(query) = query.as_deref() {
        span.record("query", tracing::field::display(query));
    }
    if let Some(client_ip) = client_ip.as_deref() {
        span.record("client_ip", tracing::field::display(client_ip));
    }
    if let Some(client) = subsonic_client.as_deref() {
        span.record("subsonic_client", tracing::field::display(client));
    }
    let log_span = span.clone();
    let started = Instant::now();

    let mut response = next.run(request).instrument(span).await;
    let _entered = log_span.enter();
    let status = response.status();
    let latency_ms = started.elapsed().as_millis() as u64;
    log_span.record("status", status.as_u16());
    log_span.record("latency_ms", latency_ms);
    if let Some(failure) = response
        .extensions()
        .get::<crate::facades::subsonic::SubsonicFailure>()
    {
        log_span.record("subsonic_code", failure.code);
    }
    if health {
        tracing::debug!("http request");
    } else {
        tracing::info!("http request");
    }

    let header_val =
        HeaderValue::from_str(request_id.as_str()).expect("request ID was already validated");

    response.headers_mut().insert("x-request-id", header_val);

    response
}

#[cfg(test)]
mod tests {
    use std::{io, sync::Mutex};

    use axum::{Router, http::StatusCode, middleware as axum_middleware, routing::get};
    use tower::ServiceExt;
    use tracing::instrument::WithSubscriber;
    use tracing_subscriber::prelude::*;

    use super::*;

    #[derive(Clone, Default)]
    struct Buffer(Arc<Mutex<Vec<u8>>>);

    impl io::Write for Buffer {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Buffer {
        type Writer = Self;

        fn make_writer(&'a self) -> Self::Writer {
            self.clone()
        }
    }

    async fn failed_auth() -> AxumResponse {
        crate::facades::subsonic::failed_response(
            whio_subsonic_api::models::error::Code::Variant40,
            "Wrong username or password.",
        )
    }

    async fn health() -> StatusCode {
        StatusCode::NO_CONTENT
    }

    #[tokio::test]
    async fn request_events_redact_credentials_include_subsonic_codes_and_hide_health_at_info() {
        let output = Buffer::default();
        let subscriber = tracing_subscriber::registry()
            .with(
                tracing_subscriber::fmt::layer()
                    .with_writer(output.clone())
                    .with_ansi(false),
            )
            .with(tracing_subscriber::EnvFilter::new("info"));
        let app = Router::new()
            .route("/rest/ping", get(failed_auth))
            .route("/health/live", get(health))
            .layer(axum_middleware::from_fn_with_state(
                Arc::new(ClientIpPolicy {
                    trusted_proxies: Vec::new(),
                }),
                request_id,
            ));

        let dispatch = tracing::Dispatch::new(subscriber);
        async {
            let failed = app
                .clone()
                .oneshot(
                    Request::builder()
                        .uri("/rest/ping?p=secret&c=Feishin%20Player")
                        .header("x-request-id", "650e8400-e29b-41d4-a716-446655440000")
                        .body(axum::body::Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            let failed_id = failed
                .headers()
                .get("x-request-id")
                .unwrap()
                .to_str()
                .unwrap();
            assert_eq!(failed_id, "650e8400-e29b-41d4-a716-446655440000");
            let invalid_client = app
                .clone()
                .oneshot(
                    Request::builder()
                        .uri("/rest/ping?c=not%2Fvalid")
                        .header("x-request-id", "660e8400-e29b-41d4-a716-446655440000")
                        .body(axum::body::Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(
                invalid_client.headers().get("x-request-id").unwrap(),
                "660e8400-e29b-41d4-a716-446655440000"
            );
            let unmatched = app
                .clone()
                .oneshot(
                    Request::builder()
                        .uri("/unknown/jwt-path")
                        .header("x-request-id", "another-secret-header")
                        .body(axum::body::Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            let unmatched_id = unmatched
                .headers()
                .get("x-request-id")
                .unwrap()
                .to_str()
                .unwrap();
            assert!(uuid::Uuid::parse_str(unmatched_id).is_ok());
            let live = app
                .oneshot(
                    Request::builder()
                        .uri("/health/live")
                        .header("x-request-id", "550e8400-e29b-41d4-a716-446655440000")
                        .body(axum::body::Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(
                live.headers().get("x-request-id").unwrap(),
                "550e8400-e29b-41d4-a716-446655440000"
            );
        }
        .with_subscriber(dispatch)
        .await;

        let log = String::from_utf8(output.0.lock().unwrap().clone()).unwrap();
        assert!(log.contains("/rest/ping"));
        assert!(!log.contains("another-secret-header"));
        assert!(log.contains("status=200"));
        assert!(log.contains("p=***"));
        assert!(log.contains("subsonic_code=40"));
        assert!(log.contains("subsonic_client=Feishin Player"));
        assert!(log.contains("650e8400-e29b-41d4-a716-446655440000"));
        assert!(!log.contains("secret"));
        assert!(!log.contains("jwt-path"));
        assert!(log.contains("route=<unmatched>"));
        assert!(!log.contains("client_ip=None"));
        assert!(!log.contains("subsonic_client=\"\""));
        assert!(!log.contains("health/live"));
        let unmatched_log = log
            .lines()
            .find(|line| line.contains("route=<unmatched>"))
            .unwrap();
        assert!(!unmatched_log.contains("subsonic_code"));
        assert!(!unmatched_log.contains("client_ip"));
        assert!(!unmatched_log.contains("subsonic_client"));
        assert!(!unmatched_log.contains("query"));
        let invalid_client_log = log
            .lines()
            .find(|line| line.contains("660e8400-e29b-41d4-a716-446655440000"))
            .unwrap();
        assert!(!invalid_client_log.contains("subsonic_client"));
    }
}
