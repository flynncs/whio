use crate::support;

use support::{NATIVE_AUTH_QUERY, StorePorts, credentials::credential_service, router, services};

mod native_artist_info {
    use super::*;

    use axum::{
        Router,
        body::{Body, to_bytes},
        http::{Request, StatusCode},
        response::Response,
    };
    use std::sync::Arc;

    use serde_json::Value;
    use sqlx::PgPool;
    use tower::ServiceExt;
    use uuid::Uuid;
    use whio_core::app::AppState;
    use whio_core::features::music_library::{
        catalogue::MAX_PAGE_LIMIT,
        discovery::DiscoveryService,
        domain::{ArtistCredit, ArtistId},
        test_support::ListenStore,
    };

    async fn send(app: &Router, method: &str, uri: &str) -> Response {
        app.clone()
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(uri)
                    .header("x-request-id", "request-123")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap()
    }

    async fn json_body(response: Response) -> Value {
        let bytes = to_bytes(response.into_body(), 16 * 1024 * 1024)
            .await
            .unwrap();

        serde_json::from_slice(&bytes).unwrap()
    }

    fn uri(path: &str, extra: &str) -> String {
        if extra.is_empty() {
            format!("{path}{NATIVE_AUTH_QUERY}")
        } else {
            format!("{path}{NATIVE_AUTH_QUERY}&{extra}")
        }
    }

    async fn artist(pool: &PgPool, name: &str, mbid: &str) -> ArtistId {
        let ports = StorePorts::new(pool.clone());
        let credit = ArtistCredit::new(name.to_owned(), Some(mbid.to_owned())).unwrap();

        ports
            .catalogue()
            .resolve_or_create_artist(&credit)
            .await
            .unwrap()
            .id()
            .clone()
    }

    fn app_for(pool: PgPool) -> Router {
        let (search, library, playback) = services(pool, Vec::new());

        router(credential_service(), search, library, playback)
    }

    fn app_with_discovery(pool: PgPool) -> Router {
        let (search, library, playback) = services(pool.clone(), Vec::new());
        let ports = StorePorts::new(pool);
        let discovery = Arc::new(DiscoveryService::new(
            ports.library(),
            ports.sources(),
            ports.state(),
            Arc::clone(&ports.store) as Arc<dyn ListenStore>,
        ));

        whio_core::app::router(
            AppState::new(credential_service(), search, library, playback)
                .with_discovery(discovery),
        )
    }

    #[sqlx::test]
    async fn native_artist_info_requires_authentication(pool: PgPool) {
        let app = app_for(pool);
        let artist_id = ArtistId::new();
        let response = send(
            &app,
            "GET",
            &format!("/api/v1/library/artists/{artist_id}/info"),
        )
        .await;

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        let body = json_body(response).await;
        assert_eq!(body["request_id"], "request-123");
    }

    #[sqlx::test]
    async fn native_artist_info_is_a_typed_503_without_a_configured_metadata_feature(pool: PgPool) {
        let app = app_for(pool.clone());
        let artist_id = artist(&pool, "Daft Punk", "mbid-daft-punk").await;

        for (method, path) in [
            ("GET", format!("/api/v1/library/artists/{artist_id}/info")),
            (
                "GET",
                format!("/api/v1/library/artists/{artist_id}/refresh"),
            ),
            (
                "POST",
                format!("/api/v1/library/artists/{artist_id}/refresh"),
            ),
        ] {
            let response = send(&app, method, &uri(&path, "")).await;

            assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
            let body = json_body(response).await;
            assert_eq!(body["code"], "metadata_disabled");
        }
    }

    #[sqlx::test]
    async fn native_artist_discovery_serves_the_shared_basis_from_the_constructed_router(
        pool: PgPool,
    ) {
        let app = app_with_discovery(pool.clone());
        let artist_id = artist(&pool, "Daft Punk", "mbid-daft-punk").await;

        for (path, extra, scope) in [
            (
                format!("/api/v1/library/artists/{artist_id}/related"),
                "",
                "shared track artist credits, no time dimension",
            ),
            (
                format!("/api/v1/library/artists/{artist_id}/top"),
                "",
                "owned tracks in title order",
            ),
            (
                format!("/api/v1/library/artists/{artist_id}/top"),
                "basis=personal",
                "all-time play counter",
            ),
        ] {
            let response = send(&app, "GET", &uri(&path, extra)).await;

            assert_ne!(
                response.status(),
                StatusCode::SERVICE_UNAVAILABLE,
                "{path}?{extra} must reach the configured discovery service"
            );
            assert_eq!(response.status(), StatusCode::OK);
            let body = json_body(response).await;
            assert_eq!(
                body["basis"],
                if scope == "all-time play counter" {
                    "personal"
                } else {
                    "library"
                }
            );
            assert_eq!(body["ranking_scope"], scope);
            assert_eq!(body["fallback"], true);
            assert_eq!(body["fallback_reason"], "no_local_basis");
            assert_eq!(body["items"], serde_json::json!([]));
        }
    }

    #[sqlx::test]
    async fn native_artist_discovery_is_a_typed_503_without_a_configured_discovery_service(
        pool: PgPool,
    ) {
        let app = app_for(pool.clone());
        let artist_id = artist(&pool, "Daft Punk", "mbid-daft-punk").await;

        for path in [
            format!("/api/v1/library/artists/{artist_id}/related"),
            format!("/api/v1/library/artists/{artist_id}/top"),
        ] {
            let response = send(&app, "GET", &uri(&path, "")).await;

            assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
            let body = json_body(response).await;
            assert_eq!(body["code"], "discovery_unavailable");
            assert!(body.get("items").is_none(), "not a served empty list");
        }
    }

    #[sqlx::test]
    async fn native_artist_info_reports_an_unknown_artist(pool: PgPool) {
        let app = app_for(pool);
        let unknown = Uuid::now_v7();

        for (method, path) in [
            ("GET", format!("/api/v1/library/artists/{unknown}/info")),
            ("GET", format!("/api/v1/library/artists/{unknown}/refresh")),
            ("GET", format!("/api/v1/library/artists/{unknown}/related")),
            ("GET", format!("/api/v1/library/artists/{unknown}/top")),
        ] {
            let response = send(&app, method, &uri(&path, "")).await;

            assert_eq!(response.status(), StatusCode::NOT_FOUND);
            let body = json_body(response).await;
            assert_eq!(body["code"], "not_found");
        }
    }

    #[sqlx::test]
    async fn native_artist_info_rejects_invalid_selectors(pool: PgPool) {
        let app = app_for(pool.clone());
        let artist_id = artist(&pool, "Daft Punk", "mbid-daft-punk").await;
        let info = format!("/api/v1/library/artists/{artist_id}/info");
        let related = format!("/api/v1/library/artists/{artist_id}/related");
        let top = format!("/api/v1/library/artists/{artist_id}/top");
        let over_limit = format!("limit={}", MAX_PAGE_LIMIT + 1);

        for (path, extra) in [
            (info.as_str(), "offset=1"),
            (info.as_str(), "profile=artist-other.v1"),
            (info.as_str(), "language=123"),
            (related.as_str(), "offset=1"),
            (related.as_str(), over_limit.as_str()),
            (top.as_str(), "basis=local"),
            (top.as_str(), "basis=banana"),
            (top.as_str(), over_limit.as_str()),
        ] {
            let response = send(&app, "GET", &uri(path, extra)).await;

            assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{path}?{extra}");
            let body = json_body(response).await;
            assert_eq!(body["code"], "invalid_request", "{path}?{extra}");
        }

        let response = send(&app, "GET", &uri(top.as_str(), "basis=server")).await;

        assert_eq!(response.status(), StatusCode::NOT_IMPLEMENTED);
        let body = json_body(response).await;
        assert_eq!(body["code"], "unsupported_basis");
    }

    mod native_refresh_enrolment {
        use super::*;

        use std::sync::Arc;

        use async_trait::async_trait;
        use chrono::Utc;
        use whio_core::app::AppState;
        use whio_core::features::metadata::capabilities::{
            CAPABILITY_TTL, CapabilityError, CapabilitySnapshot, CapabilitySource, Snapshot,
        };
        use whio_core::features::metadata::domain::FieldId;
        use whio_core::features::metadata::freshness::NoJitter;
        use whio_core::features::metadata::refresh::{
            DocumentError, DocumentOutcome, MetadataDocuments,
        };
        use whio_core::features::metadata::selection::FieldOrder;
        use whio_core::features::metadata::service::{MetadataProfile, MetadataService};
        use whio_core::features::metadata::{MetadataStore, PostgresMetadataStore};
        use whio_core::features::music_library::EnrichmentGuard;
        use whio_core::features::music_library::catalogue::LibraryService;
        use whio_core::features::music_library::discovery::DiscoveryService;
        use whio_core::features::music_library::domain::{
            ArtistId, ProviderId, SourceIdentity, SourceScope,
        };
        use whio_core::features::music_library::test_support::ListenStore;
        use whio_core::jobs::metadata_refresh::METADATA_REFRESH_KIND;

        const WIKIPEDIA: &str = "wikipedia";
        const MAX_METADATA_BYTES: u64 = 4 * 1024 * 1024;

        struct WikipediaCapabilities;

        #[async_trait]
        impl CapabilitySource for WikipediaCapabilities {
            async fn fetch(&self) -> Result<Snapshot, CapabilityError> {
                Ok(Snapshot::new(vec![WIKIPEDIA.to_owned()]))
            }
        }

        struct NoDocuments;

        #[async_trait]
        impl MetadataDocuments for NoDocuments {
            async fn fetch(
                &self,
                _source: &SourceIdentity,
                _profile: &MetadataProfile,
                _language: &str,
                _fields: &[FieldId],
            ) -> Result<DocumentOutcome, DocumentError> {
                Err(DocumentError::Unsupported)
            }
        }

        fn wikipedia_identity(external_id: &str) -> SourceIdentity {
            SourceIdentity::new(
                ProviderId::new(WIKIPEDIA.to_owned()).expect("a provider"),
                SourceScope::Global,
                external_id.to_owned(),
            )
            .expect("a source identity")
        }

        async fn non_terminal_jobs(pool: &PgPool) -> i64 {
            sqlx::query_scalar(
                "SELECT count(*) FROM awa.jobs \
             WHERE kind = $1 AND state::text IN ('scheduled', 'available', 'running')",
            )
            .bind(METADATA_REFRESH_KIND)
            .fetch_one(pool)
            .await
            .expect("the job count")
        }

        async fn refresh_app(pool: PgPool) -> (Router, ArtistId) {
            awa::migrations::run(&pool).await.expect("the awa schema");
            let artist_id = artist(&pool, "Daft Punk", "mbid-daft-punk").await;
            StorePorts::new(pool.clone())
                .catalogue()
                .set_artist_source(&artist_id, &wikipedia_identity("wiki-daft-punk"))
                .await
                .expect("the artist source");

            let ports = StorePorts::new(pool.clone());
            let (search, _) = ports.bare_catalogue();
            let playback = ports.streaming(Vec::new());
            let snapshot = Arc::new(CapabilitySnapshot::new(
                Arc::new(WikipediaCapabilities),
                CAPABILITY_TTL,
            ));
            snapshot
                .current(Utc::now())
                .await
                .expect("the capability snapshot warms");
            let store = Arc::new(PostgresMetadataStore::new(
                pool.clone(),
                Arc::clone(&ports.store) as Arc<dyn EnrichmentGuard>,
                MAX_METADATA_BYTES,
            ));
            let metadata = Arc::new(MetadataService::new(
                pool,
                Arc::clone(&store) as Arc<dyn MetadataStore>,
                Arc::new(NoDocuments) as Arc<dyn MetadataDocuments>,
                Arc::clone(&snapshot),
                FieldOrder::from_entries([(FieldId::BiographyContent, vec![WIKIPEDIA.to_owned()])]),
                "v1".to_owned(),
                Arc::new(NoJitter),
            ));
            let library = Arc::new(
                LibraryService::new(ports.library(), ports.sources(), ports.state())
                    .with_metadata(Arc::clone(&metadata)),
            );
            let discovery = Arc::new(DiscoveryService::new(
                ports.library(),
                ports.sources(),
                ports.state(),
                Arc::clone(&ports.store) as Arc<dyn ListenStore>,
            ));
            let state = AppState::new(credential_service(), search, library, playback)
                .with_discovery(discovery)
                .with_metadata_service(metadata);

            (whio_core::app::router(state), artist_id)
        }

        #[sqlx::test]
        async fn refresh_enrolment_is_one_coalesced_job_and_a_status_read_never_enrols(
            pool: PgPool,
        ) {
            let (app, artist_id) = refresh_app(pool.clone()).await;
            let path = format!("/api/v1/library/artists/{artist_id}/refresh");

            for _ in 0..2 {
                let response = send(&app, "GET", &uri(&path, "")).await;

                assert_eq!(response.status(), StatusCode::OK);
            }

            assert_eq!(
                non_terminal_jobs(&pool).await,
                0,
                "the status read never enrols work"
            );

            let response = send(&app, "POST", &uri(&path, "")).await;

            assert_eq!(response.status(), StatusCode::OK);
            assert_eq!(json_body(response).await["outcome"], "inserted");
            assert_eq!(non_terminal_jobs(&pool).await, 1);

            let response = send(&app, "POST", &uri(&path, "")).await;

            assert_eq!(response.status(), StatusCode::OK);
            assert_eq!(json_body(response).await["outcome"], "coalesced");
            assert_eq!(
                non_terminal_jobs(&pool).await,
                1,
                "the second manual refresh coalesces to the same job"
            );
        }
    }
}
