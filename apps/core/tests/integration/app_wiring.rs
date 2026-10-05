use std::{
    net::SocketAddr,
    sync::{Arc, Mutex},
    time::Duration,
};

use async_trait::async_trait;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};
use whio_core::{
    features::identity::destinations::DestinationValidator,
    features::metadata::{
        CapabilityError, CapabilitySnapshot, CapabilitySource, DocumentError, DocumentProvider,
        DocumentProviders, MetadataDocuments, MetadataProfile, Snapshot,
        selection::{MUSICBRAINZ, THEAUDIODB, YOUTUBE_MUSIC},
    },
    features::music_library::delivery::ListenSender,
    features::music_library::domain::{ProviderId, SourceIdentity, SourceScope},
    integrations::listenbrainz::ListenBrainzAdapter,
};
use whio_resolver_api::apis::configuration::Configuration;

fn identity(provider_id: &str, external_id: &str) -> SourceIdentity {
    SourceIdentity::new(
        ProviderId::new(provider_id.to_owned()).unwrap(),
        SourceScope::Global,
        external_id.to_owned(),
    )
    .unwrap()
}

fn profile() -> MetadataProfile {
    MetadataProfile::artist_display()
}

fn document_body(provider_id: &str) -> String {
    format!(
        r#"{{"address":{{"provider_id":"{provider_id}","external_id":"external"}},"retrieved_at":"2026-01-01T00:00:00Z","candidates":[],"errors":[]}}"#
    )
}

struct ResolverStub {
    address: SocketAddr,
    requests: Arc<Mutex<Vec<String>>>,
}

impl ResolverStub {
    async fn start(status: u16, body: String) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let recorded = Arc::clone(&requests);

        tokio::spawn(async move {
            while let Ok((mut socket, _)) = listener.accept().await {
                let recorded = Arc::clone(&recorded);
                let body = body.clone();

                tokio::spawn(async move {
                    let mut buffer = Vec::new();
                    let mut chunk = [0u8; 8192];

                    loop {
                        let read = socket.read(&mut chunk).await.unwrap_or(0);
                        if read == 0 {
                            break;
                        }
                        buffer.extend_from_slice(&chunk[..read]);

                        if let Some(end) = header_end(&buffer) {
                            let head = String::from_utf8_lossy(&buffer[..end]);
                            if buffer.len() >= end + content_length(&head) {
                                break;
                            }
                        }
                    }

                    let end = header_end(&buffer).unwrap_or(buffer.len());
                    let head = String::from_utf8_lossy(&buffer[..end]).to_string();
                    let mut parts = head.lines().next().unwrap_or_default().split_whitespace();
                    let method = parts.next().unwrap_or_default();
                    let path = parts.next().unwrap_or_default();
                    recorded.lock().unwrap().push(format!("{method} {path}"));

                    let response = format!(
                        "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\n\
                         Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    let _ = socket.write_all(response.as_bytes()).await;
                    let _ = socket.shutdown().await;
                });
            }
        });

        Self { address, requests }
    }

    fn configuration(&self) -> Arc<Configuration> {
        Arc::new(Configuration {
            base_path: format!("http://{}", self.address),
            ..Configuration::default()
        })
    }

    fn requests(&self) -> Vec<String> {
        self.requests.lock().unwrap().clone()
    }
}

fn header_end(buffer: &[u8]) -> Option<usize> {
    buffer
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|index| index + 4)
}

fn content_length(head: &str) -> usize {
    head.lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;

            name.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse().ok())
                .flatten()
        })
        .unwrap_or(0)
}

#[derive(Default)]
struct Recorder {
    calls: Mutex<Vec<String>>,
}

impl Recorder {
    fn record(&self, provider_id: &str) {
        self.calls.lock().unwrap().push(provider_id.to_owned());
    }

    fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }
}

struct Capabilities {
    inner: Arc<Recorder>,
    failing: bool,
}

#[async_trait]
impl CapabilitySource for Capabilities {
    async fn fetch(&self) -> Result<Snapshot, CapabilityError> {
        self.inner.record("capabilities");
        if self.failing {
            return Err(CapabilityError::Unavailable);
        }

        Ok(Snapshot::new(vec![
            YOUTUBE_MUSIC.to_owned(),
            MUSICBRAINZ.to_owned(),
        ]))
    }
}

fn youtube_registered(youtube: Arc<Configuration>) -> DocumentProviders {
    DocumentProviders::new(vec![Arc::new(
        whio_core::integrations::ytm::documents::ResolverDocuments::new(
            YOUTUBE_MUSIC.to_owned(),
            youtube,
        ),
    )])
    .unwrap()
}

fn native_musicbrainz() -> Arc<dyn DocumentProvider> {
    Arc::new(
        whio_core::integrations::musicbrainz::artist::MusicBrainzDocuments::new(
            &whio_core::integrations::musicbrainz::config::Config {
                user_agent: "app-wiring-test".to_owned(),
                wikimedia_user_agent: "app-wiring-test".to_owned(),
            },
        )
        .unwrap(),
    )
}

fn native_metadata_providers() -> Vec<Arc<dyn DocumentProvider>> {
    vec![
        native_musicbrainz(),
        Arc::new(
            whio_core::integrations::audiodb::AudioDbDocuments::new(
                &whio_core::integrations::audiodb::config::Config {
                    key: "app-wiring-test-key".to_owned(),
                },
            )
            .unwrap(),
        ),
    ]
}

#[tokio::test]
async fn a_youtube_music_request_reaches_only_the_youtube_server() {
    let youtube = ResolverStub::start(200, document_body(YOUTUBE_MUSIC)).await;
    let documents = youtube_registered(youtube.configuration());

    MetadataDocuments::fetch(
        &documents,
        &identity(YOUTUBE_MUSIC, "channel"),
        &profile(),
        "en",
        &[],
    )
    .await
    .unwrap();

    assert_eq!(youtube.requests(), vec!["POST /v1/metadata/artist/display"]);
}

#[tokio::test]
async fn an_unregistered_provider_is_unsupported_without_a_call() {
    let youtube = ResolverStub::start(200, document_body(YOUTUBE_MUSIC)).await;
    let documents = youtube_registered(youtube.configuration());
    let error = MetadataDocuments::fetch(
        &documents,
        &identity(MUSICBRAINZ, "mbid"),
        &profile(),
        "en",
        &[],
    )
    .await
    .unwrap_err();
    assert_eq!(error, DocumentError::Unsupported);
    assert!(youtube.requests().is_empty());
}

#[tokio::test]
async fn a_failed_youtube_advertisement_keeps_the_metadata_advertisement() {
    let youtube = ResolverStub::start(503, String::new()).await;
    let mut providers: Vec<Arc<dyn DocumentProvider>> = vec![Arc::new(
        whio_core::integrations::ytm::documents::ResolverDocuments::new(
            YOUTUBE_MUSIC.to_owned(),
            youtube.configuration(),
        ),
    )];
    let native = native_metadata_providers();
    for provider in &native {
        assert!(provider.advertised().await.unwrap());
    }
    providers.extend(native);
    let providers = DocumentProviders::new(providers).unwrap();
    let snapshot = CapabilitySource::fetch(&providers).await.unwrap();
    assert!(!snapshot.advertised(YOUTUBE_MUSIC));
    assert!(snapshot.advertised(MUSICBRAINZ));
    assert!(snapshot.advertised(THEAUDIODB));
    assert_eq!(youtube.requests(), vec!["GET /v1/capabilities"]);
}

#[tokio::test]
async fn a_provider_is_advertised_only_by_its_own_registration() {
    let providers = DocumentProviders::new(vec![native_musicbrainz()]).unwrap();
    assert_eq!(
        CapabilitySource::fetch(&providers).await.unwrap(),
        Snapshot::new([MUSICBRAINZ.to_owned()])
    );
}

#[tokio::test]
async fn resolver_error_codes_classify_before_status() {
    let cases = [
        (
            422,
            r#"{"code":"unsupported_provider"}"#,
            DocumentError::Unsupported,
        ),
        (
            502,
            r#"{"code":"resolution_failed"}"#,
            DocumentError::ResolutionFailed,
        ),
        (
            404,
            r#"{"code":"source_not_found"}"#,
            DocumentError::NotFound,
        ),
        (
            503,
            r#"{"code":"provider_unavailable"}"#,
            DocumentError::Unavailable { retry_after: None },
        ),
        (
            400,
            r#"{"code":"invalid_request"}"#,
            DocumentError::Malformed,
        ),
        (500, "", DocumentError::Unavailable { retry_after: None }),
        (418, "", DocumentError::Malformed),
    ];
    for (status, body, expected) in cases {
        let resolver = ResolverStub::start(status, body.to_owned()).await;
        let providers = DocumentProviders::new(vec![Arc::new(
            whio_core::integrations::ytm::documents::ResolverDocuments::new(
                YOUTUBE_MUSIC.to_owned(),
                resolver.configuration(),
            ),
        )])
        .unwrap();
        let actual = MetadataDocuments::fetch(
            &providers,
            &identity(YOUTUBE_MUSIC, "mbid"),
            &profile(),
            "en",
            &[],
        )
        .await
        .unwrap_err();
        assert_eq!(actual, expected, "status {status}");
    }
}

#[tokio::test]
async fn a_failed_capability_advertisement_yields_no_advertised_source() {
    let source = Arc::new(Capabilities {
        inner: Arc::new(Recorder::default()),
        failing: true,
    });
    let snapshot = CapabilitySnapshot::new(source, Duration::from_secs(300));

    assert!(
        snapshot
            .enabled_sources(&[MUSICBRAINZ.to_owned()], chrono::Utc::now())
            .await
            .is_empty()
    );
    assert!(!snapshot.advertised(MUSICBRAINZ, chrono::Utc::now()).await);
}

#[tokio::test]
async fn an_empty_capability_advertisement_yields_no_advertised_source() {
    let source = Arc::new(Capabilities {
        inner: Arc::new(Recorder::default()),
        failing: false,
    });
    let snapshot = CapabilitySnapshot::new(source, Duration::from_secs(300));
    let now = chrono::Utc::now();

    assert!(snapshot.advertised(YOUTUBE_MUSIC, now).await);
    assert!(snapshot.advertised(MUSICBRAINZ, now).await);
    assert!(!snapshot.advertised(THEAUDIODB, now).await);
}

#[tokio::test]
async fn capability_reads_are_deferred_until_a_consumer_asks() {
    let inner = Arc::new(Recorder::default());
    let snapshot = CapabilitySnapshot::new(
        Arc::new(Capabilities {
            inner: Arc::clone(&inner),
            failing: false,
        }),
        Duration::from_secs(300),
    );

    assert!(inner.calls().is_empty());
    snapshot.advertised(YOUTUBE_MUSIC, chrono::Utc::now()).await;
    assert_eq!(inner.calls(), vec!["capabilities"]);
}

#[test]
fn the_two_owner_ports_are_one_adapter_allocation() {
    let adapter = Arc::new(ListenBrainzAdapter::new().unwrap());
    let validator: Arc<dyn DestinationValidator> = Arc::clone(&adapter) as Arc<_>;
    let sender: Arc<dyn ListenSender> = Arc::clone(&adapter) as Arc<_>;

    assert_eq!(Arc::strong_count(&adapter), 3);
    assert_eq!(
        Arc::as_ptr(&validator) as *const (),
        Arc::as_ptr(&sender) as *const ()
    );
    assert!(Arc::ptr_eq(adapter.limiter(), adapter.limiter()));
}

#[cfg(feature = "postgres-integration")]
use crate::support;

#[cfg(feature = "postgres-integration")]
mod routes {
    use super::support;

    use axum::{
        Router,
        body::{Body, to_bytes},
        http::{Request, StatusCode},
        response::Response,
    };
    use serde_json::Value;
    use sqlx::PgPool;
    use tower::ServiceExt;

    async fn send(app: &Router, method: &str, uri: &str) -> Response {
        app.clone()
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(uri)
                    .header("x-request-id", "w4-wiring-route-check")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap()
    }

    async fn json(response: Response) -> Value {
        let bytes = to_bytes(response.into_body(), 64 * 1024).await.unwrap();

        serde_json::from_slice(&bytes).unwrap()
    }

    #[sqlx::test]
    async fn the_e10_and_e11_routes_are_mounted_and_answer_their_typed_status(pool: PgPool) {
        let (search, library, playback) = support::services(pool, Vec::new());
        let app = support::router(support::credential_service(), search, library, playback);

        for (method, path) in [
            ("GET", "/api/v1/identity/destinations"),
            ("GET", "/api/v1/library/listen-deliveries"),
        ] {
            let response = send(&app, method, path).await;

            assert_eq!(
                response.status(),
                StatusCode::UNAUTHORIZED,
                "{method} {path}"
            );
        }

        let destinations = send(
            &app,
            "GET",
            &format!(
                "/api/v1/identity/destinations{}",
                support::NATIVE_AUTH_QUERY
            ),
        )
        .await;

        assert_eq!(destinations.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(
            json(destinations).await["code"],
            "destination_credentials_unavailable"
        );

        let deliveries = send(
            &app,
            "GET",
            &format!(
                "/api/v1/library/listen-deliveries{}",
                support::NATIVE_AUTH_QUERY
            ),
        )
        .await;

        assert_eq!(deliveries.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(json(deliveries).await["code"], "history_unavailable");
    }
}

#[cfg(feature = "postgres-integration")]
mod worker_startup {
    use super::support;

    use std::time::Duration;

    use sqlx::PgPool;

    #[sqlx::test]
    async fn the_worker_refuses_a_metadata_refresh_queue_without_a_document_provider(pool: PgPool) {
        let mut worker = support::worker::Worker::start_with(
            &pool,
            "whio.metadata.refresh",
            &[
                ("WHIO_MUSICBRAINZ_ENABLED", "false"),
                ("WHIO_AUDIODB_ENABLED", "false"),
                ("WHIO_LISTENBRAINZ_METADATA_ENABLED", "false"),
                ("WHIO_YOUTUBE_ENABLED", "false"),
            ],
        );
        let status = worker.await_exit(Duration::from_secs(30)).await;

        assert!(
            !status.success(),
            "a metadata refresh queue without a metadata document provider must not start:\n{}",
            worker.log()
        );
        assert!(
            worker.log().contains("WHIO_MUSICBRAINZ_ENABLED"),
            "the refusal must name the missing variable:\n{}",
            worker.log()
        );
    }
}

#[cfg(feature = "postgres-integration")]
mod postgres {
    use super::*;

    use sqlx::PgPool;
    use whio_core::features::music_library::{
        DeliveryStore, LibraryWiring, W4Dependencies,
        catalogue::DedupMode,
        delivery::{DeliveryService, ListenSource},
    };

    struct UnusedSender;

    #[async_trait]
    impl whio_core::features::music_library::delivery::ListenSender for UnusedSender {
        async fn submit(
            &self,
            _credential: &str,
            _listen: &whio_core::features::music_library::delivery::ScrobbleListen,
        ) -> Result<
            whio_core::features::music_library::delivery::SubmissionOutcome,
            whio_core::features::music_library::delivery::SubmissionError,
        > {
            Err(whio_core::features::music_library::delivery::SubmissionError::Unavailable)
        }
    }

    struct UnusedDestinations;

    #[async_trait]
    impl whio_core::features::music_library::delivery::DeliveryDestinations for UnusedDestinations {
        async fn enabled_destinations(
            &self,
            _user_id: uuid::Uuid,
        ) -> Result<
            Vec<(uuid::Uuid, String)>,
            whio_core::features::music_library::delivery::DeliveryError,
        > {
            Ok(Vec::new())
        }

        async fn enabled_credential(
            &self,
            _user_id: uuid::Uuid,
            _provider_id: &str,
        ) -> Result<
            Option<whio_core::features::identity::destinations::DestinationCredential>,
            whio_core::features::music_library::delivery::DeliveryError,
        > {
            Ok(None)
        }
    }

    fn delivery_dependencies(pool: PgPool) -> W4Dependencies {
        let store: Arc<dyn DeliveryStore> = Arc::new(
            whio_core::features::music_library::PostgresTrackRepository::new(pool.clone()),
        );
        let source: Arc<dyn ListenSource> = Arc::new(
            whio_core::features::music_library::PostgresTrackRepository::new(pool.clone()),
        );

        W4Dependencies {
            metadata: None,
            artwork_cache: None,
            delivery: Some(whio_core::features::music_library::DeliveryWiring {
                store,
                source,
                sender: Arc::new(UnusedSender),
                destinations: Arc::new(UnusedDestinations),
            }),
        }
    }

    fn wiring(pool: PgPool, w4: W4Dependencies) -> LibraryWiring {
        LibraryWiring {
            pool,
            providers: Vec::new(),
            enabled_providers: Vec::new(),
            dedup_mode: DedupMode::Off,
            media_roots: Vec::new(),
            artwork_host_suffixes: Vec::new(),
            w4,
        }
    }

    #[sqlx::test]
    async fn construction_without_any_w4_dependency_leaves_every_consumer_disabled(pool: PgPool) {
        let music = whio_core::features::music_library::construct(wiring(
            pool,
            W4Dependencies {
                metadata: None,
                artwork_cache: None,
                delivery: None,
            },
        ));

        assert!(music.delivery.is_none());
        assert!(music.metadata.is_none());
    }

    #[sqlx::test]
    async fn a_constructed_delivery_service_is_reachable_through_the_services(pool: PgPool) {
        let w4 = delivery_dependencies(pool.clone());
        let music = whio_core::features::music_library::construct(wiring(pool, w4));

        let delivery: &Arc<DeliveryService> = music.delivery.as_ref().expect("a delivery service");

        assert!(
            delivery
                .status(uuid::Uuid::now_v7(), None, 10)
                .await
                .is_ok()
        );
    }

    #[sqlx::test]
    async fn the_delivery_service_never_holds_the_store_that_holds_it(pool: PgPool) {
        let w4 = delivery_dependencies(pool.clone());
        let music = whio_core::features::music_library::construct(wiring(pool, w4));

        let delivery = Arc::clone(music.delivery.as_ref().expect("a delivery service"));
        drop(music);

        assert_eq!(
            Arc::strong_count(&delivery),
            1,
            "the store must not hold the delivery service that holds the store"
        );
    }

    #[sqlx::test]
    async fn a_metadata_wiring_with_an_unreachable_resolver_still_constructs(pool: PgPool) {
        let documents: Arc<dyn MetadataDocuments> =
            Arc::new(DocumentProviders::new(Vec::new()).unwrap());
        let capabilities = Arc::new(CapabilitySnapshot::new(
            Arc::new(Capabilities {
                inner: Arc::new(Recorder::default()),
                failing: true,
            }),
            Duration::from_secs(300),
        ));
        let store: Arc<dyn whio_core::features::metadata::MetadataStore> =
            Arc::new(whio_core::features::metadata::PostgresMetadataStore::new(
                pool.clone(),
                Arc::new(
                    whio_core::features::music_library::PostgresTrackRepository::new(pool.clone()),
                ),
                33_554_432,
            ));

        let music = whio_core::features::music_library::construct(LibraryWiring {
            pool,
            providers: Vec::new(),
            enabled_providers: Vec::new(),
            dedup_mode: DedupMode::Off,
            media_roots: Vec::new(),
            artwork_host_suffixes: Vec::new(),
            w4: W4Dependencies {
                metadata: Some(whio_core::features::music_library::MetadataWiring {
                    store,
                    documents,
                    capabilities,
                    order: whio_core::features::metadata::selection::FieldOrder::defaults(),
                    adapter_version: "v1".to_owned(),
                    jitter: Arc::new(whio_core::features::metadata::NoJitter),
                }),
                artwork_cache: None,
                delivery: None,
            },
        });

        assert!(music.metadata.is_some());
    }
}
