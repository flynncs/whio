#![cfg(feature = "postgres-integration")]

mod support;

use support::{
    NATIVE_AUTH_QUERY, StorePorts, app, credentials::database_credential_service, router, services,
};

mod native_library {
    use super::*;

    use axum::{
        Router,
        body::{Body, to_bytes},
        http::{Request, StatusCode},
        response::Response,
    };
    use chrono::{DateTime, Duration, SecondsFormat, Utc};
    use serde_json::{Value, json};
    use sqlx::PgPool;
    use std::collections::HashSet;
    use std::sync::Arc;
    use tower::ServiceExt;
    use uuid::Uuid;
    use whio_core::{
        features::identity::service::CredentialService,
        features::music_library::{
            domain::{
                AlbumId, AlbumListFilter, AlbumListScope, AlbumMetadata, AlbumTrack, ArtistCredit,
                ArtistId, ArtistListFilter, CreditRole, ProviderId, RoleSelector, RoleSet,
                SourceIdentity, SourceScope, StateTarget, TrackId, TrackMetadata,
            },
            library::LibraryId,
            test_support::{CatalogueWrite, PostgresTrackRepository, SourceFileFacts},
        },
    };

    fn source(provider: &str, external_id: &str) -> SourceIdentity {
        SourceIdentity::new(
            ProviderId::new(provider.to_owned()).unwrap(),
            if provider == "local" {
                SourceScope::Named("/music".to_owned())
            } else {
                SourceScope::Global
            },
            external_id.to_owned(),
        )
        .unwrap()
    }

    fn metadata(title: &str) -> TrackMetadata {
        TrackMetadata::from_names(title.to_owned(), vec!["Artist".to_owned()], Some(1_000)).unwrap()
    }

    fn undated(title: &str) -> TrackMetadata {
        TrackMetadata::from_names(title.to_owned(), vec!["Artist".to_owned()], None).unwrap()
    }

    fn request(uri: &str) -> Request<Body> {
        Request::builder()
            .method("GET")
            .uri(uri)
            .header("x-request-id", "request-123")
            .body(Body::empty())
            .unwrap()
    }

    async fn json_body(response: Response) -> Value {
        let bytes = to_bytes(response.into_body(), 16 * 1024 * 1024)
            .await
            .unwrap();

        serde_json::from_slice(&bytes).unwrap()
    }

    fn item_ids(body: &Value) -> Vec<String> {
        body["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|item| item["id"].as_str().unwrap().to_owned())
            .collect()
    }

    fn router_for(pool: PgPool, credential: Arc<CredentialService>) -> Router {
        let (search, library, playback) = services(pool, vec![]);

        router(credential, search, library, playback)
    }

    fn database_auth_query(secret: &str) -> String {
        format!("?u=flynn&v=1.16.1&c=integration-test&p={secret}")
    }

    async fn set_created_at(pool: &PgPool, track: &TrackId, at: DateTime<Utc>) {
        sqlx::query("UPDATE tracks SET created_at = $1 WHERE id = $2")
            .bind(at)
            .bind(track.as_uuid())
            .execute(pool)
            .await
            .unwrap();
    }

    #[sqlx::test]
    async fn native_track_page_requires_authentication(pool: PgPool) {
        let response = app(pool, vec![])
            .oneshot(request("/api/v1/library/tracks"))
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        let body = json_body(response).await;
        assert_eq!(body["code"], "authentication_failed");
        assert_eq!(body["request_id"], "request-123");
    }

    #[sqlx::test]
    async fn native_track_page_returns_items_total_and_matching_header(pool: PgPool) {
        let ports = StorePorts::new(pool.clone());
        let catalogue = ports.catalogue();
        let album = catalogue
            .resolve_or_create_album(&AlbumMetadata::new("Discovery".to_owned()).unwrap())
            .await
            .unwrap();
        let amber = catalogue
            .get_or_create(source("local", "path-amber"), metadata("amber"))
            .await
            .unwrap();
        catalogue
            .set_album_track(
                album.id(),
                amber.id(),
                &AlbumTrack::new(Some(1), Some(3), 2).unwrap(),
            )
            .await
            .unwrap();
        let bravo = catalogue
            .get_or_create(source("local", "path-bravo"), undated("bravo"))
            .await
            .unwrap();
        let cobalt = catalogue
            .get_or_create(source("local", "path-cobalt"), metadata("cobalt"))
            .await
            .unwrap();
        let delta = catalogue
            .get_or_create(source("local", "path-delta"), metadata("delta"))
            .await
            .unwrap();
        let echo = catalogue
            .get_or_create(source("local", "path-echo"), metadata("echo"))
            .await
            .unwrap();

        let response = app(pool, vec![])
            .oneshot(request(&format!(
                "/api/v1/library/tracks{NATIVE_AUTH_QUERY}"
            )))
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()["x-total-count"], "5");
        let body = json_body(response).await;
        assert_eq!(body["total"], json!(5));
        let mut envelope = body
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>();
        envelope.sort();
        assert_eq!(envelope, ["items", "total"]);
        let items = body["items"].as_array().unwrap();
        assert_eq!(items.len(), 5);

        assert_eq!(
            item_ids(&body),
            vec![
                amber.id().to_string(),
                bravo.id().to_string(),
                cobalt.id().to_string(),
                delta.id().to_string(),
                echo.id().to_string(),
            ]
        );
        let unique = item_ids(&body).into_iter().collect::<HashSet<_>>();
        assert_eq!(unique.len(), 5, "ids are unique");

        let titles = items
            .iter()
            .map(|item| item["title"].as_str().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(titles, ["amber", "bravo", "cobalt", "delta", "echo"]);

        let mut keys = items[0]
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>();
        keys.sort();
        assert_eq!(
            keys,
            [
                "album",
                "artists",
                "availability",
                "disc_number",
                "duration_ms",
                "id",
                "ownership",
                "play_count",
                "starred",
                "title",
                "track_number",
            ]
        );
        assert_eq!(
            items[0]["album"],
            json!({ "id": album.id().to_string(), "title": "Discovery" })
        );
        assert_eq!(items[0]["disc_number"], json!(1));
        assert_eq!(items[0]["track_number"], json!(3));
        assert_eq!(items[0]["duration_ms"], json!(1_000));
        assert_eq!(items[0]["artists"], json!(["Artist"]));
        assert_eq!(items[0]["starred"], json!(false));
        assert_eq!(items[0]["play_count"], json!(0));
        assert_eq!(items[0]["ownership"], json!("owned"));
        assert_eq!(items[0]["availability"], json!("owned"));

        assert!(items[1].get("duration_ms").is_none());
        assert!(items[1].get("album").is_none());
        assert!(items[1].get("disc_number").is_none());
        assert!(items[1].get("track_number").is_none());
        assert!(items[1].get("rating").is_none());
        assert_eq!(items[1]["starred"], json!(false));
        assert_eq!(items[1]["play_count"], json!(0));
    }

    #[sqlx::test]
    async fn native_track_page_rejects_bad_limit_order_and_scope(pool: PgPool) {
        for parameter in [
            "limit=0",
            "limit=501",
            "limit=abc",
            "offset=abc",
            "offset=-1",
            "order=bpm",
            "scope=all",
        ] {
            let response = app(pool.clone(), vec![])
                .oneshot(request(&format!(
                    "/api/v1/library/tracks{NATIVE_AUTH_QUERY}&{parameter}"
                )))
                .await
                .unwrap();

            assert_eq!(
                response.status(),
                StatusCode::BAD_REQUEST,
                "the status for {parameter}"
            );
            let body = to_bytes(response.into_body(), 4096).await.unwrap();
            assert_eq!(
                body.as_ref(),
                br#"{"code":"invalid_request","message":"The request is invalid","request_id":"request-123"}"#,
                "the body for {parameter}"
            );
        }
    }

    #[sqlx::test]
    async fn native_owned_recently_added_returns_only_owned_newest_first(pool: PgPool) {
        let (credential, secret) = database_credential_service(pool.clone()).await;
        let user: Uuid = sqlx::query_scalar("SELECT id FROM users WHERE lower(username) = 'flynn'")
            .fetch_one(&pool)
            .await
            .unwrap();
        let ports = StorePorts::new(pool.clone());
        let catalogue = ports.catalogue();
        let oldest = catalogue
            .get_or_create(source("local", "path-oldest"), metadata("oldest"))
            .await
            .unwrap();
        let middle = catalogue
            .get_or_create(source("local", "path-middle"), metadata("middle"))
            .await
            .unwrap();
        let newest = catalogue
            .get_or_create(source("local", "path-newest"), metadata("newest"))
            .await
            .unwrap();
        let engaged = catalogue
            .get_or_create(
                source("youtube_music", "vid-engaged"),
                metadata("engaged only"),
            )
            .await
            .unwrap();
        ports
            .state()
            .set_rating(user, StateTarget::Track(engaged.id()), Some(4))
            .await
            .unwrap();

        let now = Utc::now();
        set_created_at(&pool, oldest.id(), now - Duration::days(3)).await;
        set_created_at(&pool, middle.id(), now - Duration::days(2)).await;
        set_created_at(&pool, newest.id(), now - Duration::days(1)).await;
        set_created_at(&pool, engaged.id(), now).await;

        let auth = database_auth_query(&secret);
        let router = router_for(pool.clone(), credential);

        let response = router
            .clone()
            .oneshot(request(&format!(
                "/api/v1/library/tracks{auth}&scope=owned&order=recently_added"
            )))
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()["x-total-count"], "3");
        let body = json_body(response).await;
        assert_eq!(body["total"], json!(3));
        assert_eq!(
            item_ids(&body),
            vec![
                newest.id().to_string(),
                middle.id().to_string(),
                oldest.id().to_string(),
            ]
        );
        for item in body["items"].as_array().unwrap() {
            assert_eq!(item["ownership"], json!("owned"));
        }

        let response = router
            .oneshot(request(&format!(
                "/api/v1/library/tracks{auth}&order=recently_added"
            )))
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = json_body(response).await;
        assert_eq!(body["total"], json!(4));
        assert_eq!(
            item_ids(&body),
            vec![
                engaged.id().to_string(),
                newest.id().to_string(),
                middle.id().to_string(),
                oldest.id().to_string(),
            ],
            "the engaged-only track is visible under the default scope"
        );
    }

    #[sqlx::test]
    async fn native_deep_page_beyond_1000_tracks_returns_the_remainder(pool: PgPool) {
        let repository = PostgresTrackRepository::new(pool.clone());
        let mut fixture = Vec::new();

        for index in 0..1001u32 {
            let title = format!("item{:03}", index % 10);
            let track = repository
                .get_or_create(
                    source("local", &format!("path-{index:04}")),
                    metadata(&title),
                )
                .await
                .unwrap();

            fixture.push((title, track.id().clone()));
        }

        fixture.sort_by(|left, right| {
            left.0
                .cmp(&right.0)
                .then_with(|| left.1.as_uuid().cmp(right.1.as_uuid()))
        });
        let expected = fixture
            .into_iter()
            .map(|(_, id)| id.to_string())
            .collect::<Vec<_>>();
        let router = app(pool, vec![]);

        let first = router
            .clone()
            .oneshot(request(&format!(
                "/api/v1/library/tracks{NATIVE_AUTH_QUERY}&limit=500"
            )))
            .await
            .unwrap();
        assert_eq!(first.headers()["x-total-count"], "1001");
        let first = json_body(first).await;
        assert_eq!(first["total"], json!(1001));
        assert_eq!(first["items"].as_array().unwrap().len(), 500);
        assert_eq!(item_ids(&first), expected[0..500].to_vec());

        let default = router
            .clone()
            .oneshot(request(&format!(
                "/api/v1/library/tracks{NATIVE_AUTH_QUERY}"
            )))
            .await
            .unwrap();
        assert_eq!(default.headers()["x-total-count"], "1001");
        let default = json_body(default).await;
        assert_eq!(default["total"], json!(1001));
        assert_eq!(
            item_ids(&default),
            expected[0..50].to_vec(),
            "the default page is offset 0, limit 50, title order"
        );

        let deep = router
            .clone()
            .oneshot(request(&format!(
                "/api/v1/library/tracks{NATIVE_AUTH_QUERY}&limit=500&offset=1000"
            )))
            .await
            .unwrap();
        assert_eq!(deep.headers()["x-total-count"], "1001");
        let deep = json_body(deep).await;
        assert_eq!(deep["total"], first["total"]);
        assert_eq!(item_ids(&deep), expected[1000..1001].to_vec());

        let past_end = router
            .oneshot(request(&format!(
                "/api/v1/library/tracks{NATIVE_AUTH_QUERY}&offset=1001"
            )))
            .await
            .unwrap();
        assert_eq!(past_end.headers()["x-total-count"], "1001");
        let past_end = json_body(past_end).await;
        assert_eq!(past_end["total"], json!(1001));
        assert!(
            past_end["items"].as_array().unwrap().is_empty(),
            "an offset past the end returns an empty page with the exact total"
        );
    }

    #[sqlx::test]
    async fn native_unrated_track_omits_rating(pool: PgPool) {
        let (credential, secret) = database_credential_service(pool.clone()).await;
        let user: Uuid = sqlx::query_scalar("SELECT id FROM users WHERE lower(username) = 'flynn'")
            .fetch_one(&pool)
            .await
            .unwrap();
        let ports = StorePorts::new(pool.clone());
        let catalogue = ports.catalogue();
        let rated = catalogue
            .get_or_create(source("local", "path-rated"), metadata("rated"))
            .await
            .unwrap();
        let unrated = catalogue
            .get_or_create(source("local", "path-unrated"), metadata("unrated"))
            .await
            .unwrap();
        ports
            .state()
            .set_rating(user, StateTarget::Track(rated.id()), Some(5))
            .await
            .unwrap();

        let response = router_for(pool, credential)
            .oneshot(request(&format!(
                "/api/v1/library/tracks{}",
                database_auth_query(&secret)
            )))
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = json_body(response).await;
        assert_eq!(
            item_ids(&body),
            vec![rated.id().to_string(), unrated.id().to_string()]
        );

        let items = body["items"].as_array().unwrap();
        assert_eq!(items[0]["rating"], json!(5));
        assert!(items[1].get("rating").is_none());
        assert_eq!(items[1]["starred"], json!(false));
        assert_eq!(items[1]["play_count"], json!(0));
    }

    const LOCAL_ROOT: &str = "/music";
    const OTHER_ROOT: &str = "/elsewhere";

    struct Library {
        router: Router,
        auth: String,
    }

    impl Library {
        async fn new(pool: PgPool) -> Self {
            let (credential, secret) = database_credential_service(pool.clone()).await;

            Self {
                router: router_for(pool, credential),
                auth: database_auth_query(&secret),
            }
        }

        async fn get(&self, resource: &str, parameters: &str) -> Response {
            self.router
                .clone()
                .oneshot(request(&format!(
                    "/api/v1/library/{resource}{}&{parameters}",
                    self.auth
                )))
                .await
                .unwrap()
        }

        async fn page(&self, resource: &str, parameters: &str) -> Value {
            let response = self.get(resource, parameters).await;

            assert_eq!(
                response.status(),
                StatusCode::OK,
                "the status for {resource}?{parameters}"
            );
            let header = response.headers()["x-total-count"]
                .to_str()
                .unwrap()
                .parse::<u32>()
                .unwrap();
            let body = json_body(response).await;

            assert_eq!(
                body["total"],
                json!(header),
                "the total and x-total-count agree for {resource}?{parameters}"
            );

            body
        }

        async fn rejected(&self, resource: &str, parameters: &str) {
            let response = self.get(resource, parameters).await;

            assert_eq!(
                response.status(),
                StatusCode::BAD_REQUEST,
                "the status for {resource}?{parameters}"
            );
            let body = to_bytes(response.into_body(), 4096).await.unwrap();

            assert_eq!(
                body.as_ref(),
                br#"{"code":"invalid_request","message":"The request is invalid","request_id":"request-123"}"#,
                "the body for {resource}?{parameters}"
            );
        }
    }

    fn source_in(provider: &str, root: &str, external_id: &str) -> SourceIdentity {
        SourceIdentity::new(
            ProviderId::new(provider.to_owned()).unwrap(),
            if provider == "local" {
                SourceScope::Named(root.to_owned())
            } else {
                SourceScope::Global
            },
            external_id.to_owned(),
        )
        .unwrap()
    }

    fn metadata_by(title: &str, primary: &str) -> TrackMetadata {
        TrackMetadata::from_names(title.to_owned(), vec![primary.to_owned()], Some(1_000)).unwrap()
    }

    fn credit(name: &str, role: Option<CreditRole>) -> ArtistCredit {
        let credit = ArtistCredit::new(name.to_owned(), None).unwrap();

        match role {
            Some(role) => credit.with_role(role),
            None => credit,
        }
    }

    fn album(title: &str) -> AlbumMetadata {
        AlbumMetadata::new(title.to_owned()).unwrap()
    }

    fn local_facts(channels: Option<u8>, bitrate_kbps: Option<u32>) -> SourceFileFacts {
        SourceFileFacts {
            path: Some("Library/song.flac".to_owned()),
            size_bytes: 4_000,
            modified_at: None,
            missing_at: None,
            content_type: Some("audio/flac".to_owned()),
            suffix: Some("flac".to_owned()),
            container: Some("flac".to_owned()),
            codec: Some("flac".to_owned()),
            bitrate_kbps,
            sample_rate_hz: Some(44_100),
            bit_depth: Some(16),
            channels,
            facts_version: 1,
            acoustid_fingerprint: None,
        }
    }

    fn encoded(source: &SourceIdentity) -> String {
        match source.source_scope() {
            SourceScope::Global => "global".to_owned(),
            SourceScope::Named(root) => format!("named:{root}"),
        }
    }

    async fn user_id(pool: &PgPool) -> Uuid {
        sqlx::query_scalar("SELECT id FROM users WHERE lower(username) = 'flynn'")
            .fetch_one(pool)
            .await
            .unwrap()
    }

    async fn register_library(pool: &PgPool, label: &str, root: &str) -> LibraryId {
        let id = Uuid::now_v7();

        sqlx::query("INSERT INTO libraries (id, label) VALUES ($1, $2)")
            .bind(id)
            .bind(label)
            .execute(pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO library_roots (library_id, root) VALUES ($1, $2)")
            .bind(id)
            .bind(root)
            .execute(pool)
            .await
            .unwrap();

        LibraryId::from_uuid(id)
    }

    async fn credit_artist(pool: &PgPool, track: &TrackId, name: &str) -> ArtistId {
        let id: Uuid = sqlx::query_scalar(
            "SELECT artist_id FROM track_artist_credits \
             WHERE track_id = $1 AND name = $2 ORDER BY position LIMIT 1",
        )
        .bind(track.as_uuid())
        .bind(name)
        .fetch_one(pool)
        .await
        .unwrap();

        ArtistId::from_uuid(id)
    }

    async fn named_artist(pool: &PgPool, name: &str) -> ArtistId {
        let id: Uuid = sqlx::query_scalar("SELECT id FROM artists WHERE name = $1")
            .bind(name)
            .fetch_one(pool)
            .await
            .unwrap();

        ArtistId::from_uuid(id)
    }

    async fn album_artist_id_of(pool: &PgPool, album: &AlbumId) -> ArtistId {
        let id: Uuid = sqlx::query_scalar("SELECT album_artist_id FROM albums WHERE id = $1")
            .bind(album.as_uuid())
            .fetch_one(pool)
            .await
            .unwrap();

        ArtistId::from_uuid(id)
    }

    async fn age_source(pool: &PgPool, source: &SourceIdentity, days: i64) {
        sqlx::query(
            "UPDATE track_sources SET created_at = $4 \
             WHERE provider_id = $1 AND source_scope = $2 AND external_id = $3",
        )
        .bind(source.provider_id().as_str())
        .bind(encoded(source))
        .bind(source.external_id())
        .bind(Utc::now() + Duration::days(days))
        .execute(pool)
        .await
        .unwrap();
    }

    async fn age_album_source(pool: &PgPool, album: &AlbumId, days: i64) {
        sqlx::query("UPDATE album_sources SET created_at = $1 WHERE album_id = $2")
            .bind(Utc::now() + Duration::days(days))
            .bind(album.as_uuid())
            .execute(pool)
            .await
            .unwrap();
    }

    async fn set_missing(pool: &PgPool, source: &SourceIdentity, missing: bool) {
        sqlx::query(
            "UPDATE track_sources SET missing_at = CASE WHEN $4 THEN CURRENT_TIMESTAMP ELSE NULL END \
             WHERE provider_id = $1 AND source_scope = $2 AND external_id = $3",
        )
        .bind(source.provider_id().as_str())
        .bind(encoded(source))
        .bind(source.external_id())
        .bind(missing)
        .execute(pool)
        .await
        .unwrap();
    }

    fn titles(body: &Value) -> Vec<&str> {
        body["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|item| item["title"].as_str().unwrap())
            .collect()
    }

    fn names(body: &Value) -> Vec<&str> {
        body["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|item| item["name"].as_str().unwrap())
            .collect()
    }

    fn envelope_keys(body: &Value) -> Vec<&str> {
        let mut keys = body
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>();
        keys.sort();

        keys
    }

    struct SelectorFixture {
        alpha: TrackId,
        beta: TrackId,
        delta: TrackId,
        epsilon: TrackId,
        kept: AlbumId,
        other: AlbumId,
        music: LibraryId,
        elsewhere: LibraryId,
        composer_artist: ArtistId,
        album_artist: ArtistId,
    }

    async fn selector_fixture(pool: &PgPool) -> SelectorFixture {
        let ports = StorePorts::new(pool.clone());
        let catalogue = ports.catalogue();
        let user = user_id(pool).await;
        let kept = catalogue
            .resolve_or_create_album(
                &album("Kept")
                    .with_album_artist(credit("Album Artist", None))
                    .with_year(Some(1999))
                    .unwrap()
                    .with_compilation(Some(false)),
            )
            .await
            .unwrap();
        let other = catalogue
            .resolve_or_create_album(&album("Other").with_year(Some(2020)).unwrap())
            .await
            .unwrap();
        let various = catalogue
            .resolve_or_create_album(&album("Various").with_compilation(Some(true)))
            .await
            .unwrap();
        let alpha = catalogue
            .get_or_create(
                source("local", "alpha.flac"),
                metadata_by("alpha", "Composer One")
                    .with_genres(vec!["Rock".to_owned(), "Pop".to_owned()])
                    .with_credits(vec![credit("Composer One", Some(CreditRole::Composer))]),
            )
            .await
            .unwrap();
        let beta = catalogue
            .get_or_create(
                source_in("local", OTHER_ROOT, "beta.flac"),
                metadata_by("beta", "Narrator")
                    .with_genres(vec!["Jazz".to_owned()])
                    .with_credits(vec![credit("Narrator", Some(CreditRole::Performer))]),
            )
            .await
            .unwrap();
        let delta = catalogue
            .get_or_create(source("local", "delta.flac"), metadata("delta"))
            .await
            .unwrap();
        let epsilon = catalogue
            .get_or_create(source("local", "epsilon.flac"), metadata("epsilon"))
            .await
            .unwrap();

        catalogue
            .set_album_track(
                kept.id(),
                alpha.id(),
                &AlbumTrack::new(Some(1), Some(3), 2)
                    .unwrap()
                    .with_totals(Some(2), Some(12)),
            )
            .await
            .unwrap();
        catalogue
            .set_album_track(other.id(), beta.id(), &AlbumTrack::unplaced())
            .await
            .unwrap();
        catalogue
            .set_album_track(various.id(), epsilon.id(), &AlbumTrack::unplaced())
            .await
            .unwrap();
        catalogue
            .set_source_file_facts(
                &source("local", "alpha.flac"),
                &local_facts(Some(2), Some(320)),
            )
            .await
            .unwrap();
        catalogue
            .set_source_file_facts(
                &source_in("local", OTHER_ROOT, "beta.flac"),
                &local_facts(Some(6), Some(1_411)),
            )
            .await
            .unwrap();
        ports
            .state()
            .set_starred(user, StateTarget::Track(alpha.id()), true)
            .await
            .unwrap();
        ports
            .state()
            .set_rating(user, StateTarget::Track(alpha.id()), Some(4))
            .await
            .unwrap();
        ports
            .state()
            .record_play(user, beta.id(), Utc::now())
            .await
            .unwrap();
        set_missing(pool, &source_in("local", OTHER_ROOT, "beta.flac"), true).await;
        age_source(pool, &source("local", "alpha.flac"), -2).await;

        SelectorFixture {
            alpha: alpha.id().clone(),
            beta: beta.id().clone(),
            delta: delta.id().clone(),
            epsilon: epsilon.id().clone(),
            kept: kept.id().clone(),
            other: other.id().clone(),
            music: register_library(pool, "main", LOCAL_ROOT).await,
            elsewhere: register_library(pool, "elsewhere", OTHER_ROOT).await,
            composer_artist: credit_artist(pool, alpha.id(), "Composer One").await,
            album_artist: album_artist_id_of(pool, kept.id()).await,
        }
    }

    #[sqlx::test]
    async fn native_track_page_exposes_every_selector(pool: PgPool) {
        let library = Library::new(pool.clone()).await;
        let fixture = selector_fixture(&pool).await;
        let added_from =
            (Utc::now() - Duration::days(3)).to_rfc3339_opts(SecondsFormat::Secs, true);
        let added_to = (Utc::now() - Duration::days(1)).to_rfc3339_opts(SecondsFormat::Secs, true);
        let selectors = vec![
            ("title", "title=alpha".to_owned(), vec![&fixture.alpha]),
            (
                "blank title",
                "title=".to_owned(),
                vec![
                    &fixture.alpha,
                    &fixture.beta,
                    &fixture.delta,
                    &fixture.epsilon,
                ],
            ),
            (
                "album_id",
                format!("album_id={}", fixture.kept),
                vec![&fixture.alpha],
            ),
            (
                "id set",
                format!("album_id={},{}", fixture.kept, fixture.other),
                vec![&fixture.alpha, &fixture.beta],
            ),
            (
                "album_artist_id",
                format!("album_artist_id={}", fixture.album_artist),
                vec![&fixture.alpha],
            ),
            (
                "artists_id",
                format!("artists_id={}", fixture.composer_artist),
                vec![&fixture.alpha],
            ),
            ("role", "role=composer".to_owned(), vec![&fixture.alpha]),
            (
                "role set",
                "role=composer,performer".to_owned(),
                vec![&fixture.alpha, &fixture.beta],
            ),
            ("genre", "genre=rock".to_owned(), vec![&fixture.alpha]),
            (
                "genre set",
                "genre=rock,jazz".to_owned(),
                vec![&fixture.alpha, &fixture.beta],
            ),
            (
                "year range",
                "year_from=1990&year_to=2010".to_owned(),
                vec![&fixture.alpha],
            ),
            (
                "single-value range",
                "year_from=1999&year_to=1999".to_owned(),
                vec![&fixture.alpha],
            ),
            (
                "one-sided range",
                "year_from=2010".to_owned(),
                vec![&fixture.beta],
            ),
            (
                "compilation false",
                "compilation=false".to_owned(),
                vec![&fixture.alpha],
            ),
            (
                "compilation true",
                "compilation=true".to_owned(),
                vec![&fixture.epsilon],
            ),
            (
                "starred true",
                "starred=true".to_owned(),
                vec![&fixture.alpha],
            ),
            (
                "starred false",
                "starred=false".to_owned(),
                vec![&fixture.beta, &fixture.delta, &fixture.epsilon],
            ),
            (
                "has_rating true",
                "has_rating=true".to_owned(),
                vec![&fixture.alpha],
            ),
            (
                "has_rating false",
                "has_rating=false".to_owned(),
                vec![&fixture.beta, &fixture.delta, &fixture.epsilon],
            ),
            (
                "rating range",
                "rating_from=4&rating_to=5".to_owned(),
                vec![&fixture.alpha],
            ),
            (
                "play_count range",
                "play_count_from=1&play_count_to=1".to_owned(),
                vec![&fixture.beta],
            ),
            (
                "play_count one-sided",
                "play_count_from=1".to_owned(),
                vec![&fixture.beta],
            ),
            (
                "recently_played true",
                "recently_played=true".to_owned(),
                vec![&fixture.beta],
            ),
            (
                "recently_played false",
                "recently_played=false".to_owned(),
                vec![&fixture.alpha, &fixture.delta, &fixture.epsilon],
            ),
            (
                "date_added range",
                format!("date_added_from={added_from}&date_added_to={added_to}"),
                vec![&fixture.alpha],
            ),
            (
                "missing true",
                "missing=true".to_owned(),
                vec![&fixture.beta],
            ),
            (
                "missing false",
                "missing=false".to_owned(),
                vec![&fixture.alpha, &fixture.delta, &fixture.epsilon],
            ),
            (
                "path",
                "path=song.flac".to_owned(),
                vec![&fixture.alpha, &fixture.beta],
            ),
            (
                "duration range",
                "duration_from=500&duration_to=2000".to_owned(),
                vec![
                    &fixture.alpha,
                    &fixture.beta,
                    &fixture.delta,
                    &fixture.epsilon,
                ],
            ),
            (
                "track_number range",
                "track_number_from=3&track_number_to=3".to_owned(),
                vec![&fixture.alpha],
            ),
            (
                "disc_number range",
                "disc_number_from=1&disc_number_to=1".to_owned(),
                vec![&fixture.alpha],
            ),
            (
                "disc_total range",
                "disc_total_from=2&disc_total_to=2".to_owned(),
                vec![&fixture.alpha],
            ),
            (
                "track_total one-sided",
                "track_total_from=12".to_owned(),
                vec![&fixture.alpha],
            ),
            (
                "bitrate range",
                "bitrate_from=300&bitrate_to=400".to_owned(),
                vec![&fixture.alpha],
            ),
            (
                "bitrate one-sided",
                "bitrate_from=1000".to_owned(),
                vec![&fixture.beta],
            ),
            (
                "bitdepth range",
                "bitdepth_from=16&bitdepth_to=16".to_owned(),
                vec![&fixture.alpha, &fixture.beta],
            ),
            (
                "samplerate one-sided",
                "samplerate_from=44100".to_owned(),
                vec![&fixture.alpha, &fixture.beta],
            ),
            (
                "size one-sided",
                "size_from=1000".to_owned(),
                vec![&fixture.alpha, &fixture.beta],
            ),
            (
                "channels range",
                "channels_from=2&channels_to=2".to_owned(),
                vec![&fixture.alpha],
            ),
            (
                "channels one-sided",
                "channels_from=6".to_owned(),
                vec![&fixture.beta],
            ),
            (
                "codec",
                "codec=flac".to_owned(),
                vec![&fixture.alpha, &fixture.beta],
            ),
            (
                "filetype",
                "filetype=flac".to_owned(),
                vec![&fixture.alpha, &fixture.beta],
            ),
            (
                "library_id",
                format!("library_id={}", fixture.music),
                vec![&fixture.alpha, &fixture.delta, &fixture.epsilon],
            ),
            (
                "library_id elsewhere",
                format!("library_id={}", fixture.elsewhere),
                vec![&fixture.beta],
            ),
        ];

        for (name, parameters, expected) in selectors {
            let body = library.page("tracks", &parameters).await;
            let expected = expected.iter().map(|id| id.to_string()).collect::<Vec<_>>();

            assert_eq!(item_ids(&body), expected, "the {name} selector's items");
            assert_eq!(
                body["total"],
                json!(expected.len()),
                "the {name} selector's total"
            );
        }

        let alpha_page = library.page("tracks", "title=alpha").await;
        let item = &alpha_page["items"][0];

        assert_eq!(item["id"], json!(fixture.alpha.to_string()));
        assert_eq!(item["title"], json!("alpha"));
        assert_eq!(item["artists"], json!(["Composer One"]));
        assert_eq!(
            item["album"],
            json!({ "id": fixture.kept.to_string(), "title": "Kept" })
        );
        assert_eq!(item["disc_number"], json!(1));
        assert_eq!(item["track_number"], json!(3));
        assert_eq!(item["duration_ms"], json!(1_000));
        assert_eq!(item["starred"], json!(true));
        assert_eq!(item["rating"], json!(4));
        assert_eq!(item["play_count"], json!(0));
        assert_eq!(item["ownership"], json!("owned"));
        assert_eq!(item["availability"], json!("owned"));
    }

    #[sqlx::test]
    async fn native_unknown_parameter_is_rejected(pool: PgPool) {
        let library = Library::new(pool.clone()).await;
        let catalogue = StorePorts::new(pool.clone()).catalogue();
        let album_row = catalogue
            .resolve_or_create_album(&album("Kept"))
            .await
            .unwrap();
        let track = catalogue
            .get_or_create(
                source("local", "known.flac"),
                metadata_by("known", "Artist"),
            )
            .await
            .unwrap();
        catalogue
            .set_album_track(album_row.id(), track.id(), &AlbumTrack::unplaced())
            .await
            .unwrap();

        for resource in ["tracks", "albums", "artists"] {
            library.rejected(resource, "not_a_selector=1").await;
        }
        library.rejected("tracks", "year_from=abc").await;
        library
            .rejected("tracks", "year_from=2020&year_to=1990")
            .await;
        library.rejected("tracks", "starred=1").await;
        library.rejected("tracks", "role=banjo").await;
        library.rejected("tracks", "direction=up").await;
        library.rejected("tracks", "seed=seven").await;
        library.rejected("tracks", "seed=7&order=title").await;
        library.rejected("tracks", "artists_id=").await;
        library.rejected("tracks", "artists_id=abc,").await;
        library
            .rejected("tracks", "date_added_from=yesterday")
            .await;
        library.rejected("albums", "compilation=yes").await;
        library.rejected("albums", "seed=7&order=name").await;
        library.rejected("albums", "year_to=abc").await;
        library.rejected("albums", "scope=everything").await;
        library.rejected("artists", "scope=engaged").await;
        library.rejected("artists", "seed=1").await;
        library.rejected("artists", "album_id=abc").await;

        let known = library.page("tracks", "title=known").await;
        assert_eq!(known["total"], json!(1));
        let named = library.page("albums", "name=kept").await;
        assert_eq!(named["total"], json!(1));
        let lacks = library.page("tracks", "title=absent").await;
        assert_eq!(lacks["total"], json!(0));
    }

    #[sqlx::test]
    async fn native_random_order_echoes_the_effective_seed(pool: PgPool) {
        let library = Library::new(pool.clone()).await;
        let catalogue = StorePorts::new(pool.clone()).catalogue();

        for index in 0..12u32 {
            let title = format!("track{index:02}");
            catalogue
                .get_or_create(
                    source("local", &format!("path-{index:02}")),
                    metadata_by(&title, "Artist"),
                )
                .await
                .unwrap();
        }

        let all = 12;
        let derived = library.page("tracks", "order=random&limit=12").await;
        let seed = derived["seed"]
            .as_u64()
            .expect("order=random carries the effective seed");
        assert_eq!(derived["total"], json!(all));
        let permutation = item_ids(&derived);
        assert_eq!(permutation.len(), all);
        assert_eq!(
            permutation.iter().collect::<HashSet<_>>().len(),
            all,
            "every track appears once"
        );

        let again = library.page("tracks", "order=random&limit=12").await;
        assert_eq!(again["seed"], json!(seed), "the derived seed is stable");
        assert_eq!(item_ids(&again), permutation);

        let echoed = library
            .page("tracks", &format!("order=random&seed={seed}&limit=12"))
            .await;
        assert_eq!(echoed["seed"], json!(seed));
        assert_eq!(
            item_ids(&echoed),
            permutation,
            "the echoed seed reproduces the page"
        );

        let explicit = library
            .page("tracks", "order=random&seed=42&limit=12")
            .await;
        assert_eq!(explicit["seed"], json!(42));
        let repeat = library
            .page("tracks", "order=random&seed=42&limit=12")
            .await;
        assert_eq!(item_ids(&repeat), item_ids(&explicit));
        let other = library
            .page("tracks", "order=random&seed=43&limit=12")
            .await;
        assert_eq!(other["seed"], json!(43));
        assert_ne!(item_ids(&other), item_ids(&explicit));

        let head = library
            .page("tracks", "order=random&seed=42&limit=5&offset=0")
            .await;
        let tail = library
            .page("tracks", "order=random&seed=42&limit=5&offset=5")
            .await;
        assert_eq!(head["total"], json!(all));
        assert_eq!(tail["total"], json!(all));
        let mut concatenated = item_ids(&head);
        concatenated.extend(item_ids(&tail));
        assert_eq!(
            concatenated,
            item_ids(&explicit)[0..10].to_vec(),
            "pages are slices of one permutation with the full total"
        );

        let plain = library.page("tracks", "order=title&limit=1").await;
        assert!(
            plain.get("seed").is_none(),
            "a non-random order carries no seed"
        );
    }

    #[sqlx::test]
    async fn native_album_page_matches_the_count_and_the_scope(pool: PgPool) {
        let library = Library::new(pool.clone()).await;
        let user = user_id(&pool).await;
        let ports = StorePorts::new(pool.clone());
        let catalogue = ports.catalogue();
        let kept = catalogue
            .resolve_or_create_album(
                &album("Kept")
                    .with_album_artist(credit("Album Artist", None))
                    .with_year(Some(1999))
                    .unwrap()
                    .with_compilation(Some(false)),
            )
            .await
            .unwrap();
        let mut member = None;

        for external in ["kept-one.flac", "kept-two.flac"] {
            let track = catalogue
                .get_or_create(
                    source("local", external),
                    metadata_by(external, "Album Artist").with_genres(vec!["Rock".to_owned()]),
                )
                .await
                .unwrap();
            catalogue
                .set_album_track(kept.id(), track.id(), &AlbumTrack::unplaced())
                .await
                .unwrap();
            member = Some(track.id().clone());
        }

        let local_only = catalogue
            .resolve_or_create_album(&album("Local Only"))
            .await
            .unwrap();
        let local_track = catalogue
            .get_or_create(
                source("local", "local-only.flac"),
                metadata_by("local only", "Soloist")
                    .with_credits(vec![credit("Composer", Some(CreditRole::Composer))]),
            )
            .await
            .unwrap();
        catalogue
            .set_album_track(local_only.id(), local_track.id(), &AlbumTrack::unplaced())
            .await
            .unwrap();
        let provider_only = catalogue
            .resolve_or_create_album(&album("Provider Only"))
            .await
            .unwrap();
        let provider_track = catalogue
            .get_or_create(
                source("youtube_music", "vid-provider"),
                metadata_by("provider only", "Soloist"),
            )
            .await
            .unwrap();
        catalogue
            .set_album_track(
                provider_only.id(),
                provider_track.id(),
                &AlbumTrack::unplaced(),
            )
            .await
            .unwrap();
        catalogue
            .set_album_source(kept.id(), &source("local", "kept.flac"))
            .await
            .unwrap();
        catalogue
            .set_album_source(local_only.id(), &source("local", "local-only.flac"))
            .await
            .unwrap();
        age_album_source(&pool, kept.id(), -10).await;
        ports
            .state()
            .set_starred(user, StateTarget::Album(kept.id()), true)
            .await
            .unwrap();
        ports
            .state()
            .set_rating(user, StateTarget::Album(kept.id()), Some(4))
            .await
            .unwrap();
        ports
            .state()
            .record_play(user, member.as_ref().unwrap(), Utc::now())
            .await
            .unwrap();
        ports
            .state()
            .set_starred(user, StateTarget::Album(provider_only.id()), true)
            .await
            .unwrap();
        set_missing(&pool, &source("local", "local-only.flac"), true).await;
        let music = register_library(&pool, "main", LOCAL_ROOT).await;

        let engaged = library.page("albums", "order=name").await;
        assert_eq!(names(&engaged), ["Kept", "Local Only", "Provider Only"]);
        assert_eq!(envelope_keys(&engaged), ["items", "total"]);
        assert_eq!(
            engaged["total"],
            json!(
                ports
                    .library()
                    .count_albums(Some(user), AlbumListFilter::new())
                    .await
                    .unwrap()
            ),
            "the engaged total is the engaged count"
        );

        let owned = library.page("albums", "order=name&scope=owned").await;
        assert_eq!(
            names(&owned),
            ["Kept", "Local Only"],
            "the owned scope excludes the provider-only album"
        );
        assert_eq!(
            owned["total"],
            json!(
                ports
                    .library()
                    .count_albums(
                        Some(user),
                        AlbumListFilter::new().with_scope(AlbumListScope::Owned)
                    )
                    .await
                    .unwrap()
            ),
            "the owned total is the owned count"
        );

        let items = engaged["items"].as_array().unwrap();
        let kept_item = items.iter().find(|item| item["name"] == "Kept").unwrap();

        assert_eq!(kept_item["id"], json!(kept.id().to_string()));
        assert_eq!(
            kept_item["album_artist"],
            json!({ "id": album_artist_id_of(&pool, kept.id()).await.to_string(), "name": "Album Artist" })
        );
        assert_eq!(kept_item["year"], json!(1999));
        assert_eq!(kept_item["song_count"], json!(2));
        assert_eq!(kept_item["duration_ms"], json!(2_000));
        assert_eq!(kept_item["starred"], json!(true));
        assert_eq!(kept_item["rating"], json!(4));
        assert_eq!(kept_item["play_count"], json!(1));
        let mut keys = kept_item
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>();
        keys.sort();
        assert_eq!(
            keys,
            [
                "album_artist",
                "duration_ms",
                "id",
                "name",
                "play_count",
                "rating",
                "song_count",
                "starred",
                "year"
            ]
        );

        let provider_item = items
            .iter()
            .find(|item| item["name"] == "Provider Only")
            .unwrap();
        assert_eq!(provider_item["starred"], json!(true));
        assert_eq!(provider_item["play_count"], json!(0));
        assert!(provider_item.get("album_artist").is_none());
        assert!(provider_item.get("year").is_none());
        assert!(provider_item.get("rating").is_none());

        let album_artist = album_artist_id_of(&pool, kept.id()).await;
        assert_eq!(names(&library.page("albums", "name=kept").await), ["Kept"]);
        assert_eq!(
            names(
                &library
                    .page("albums", &format!("album_artist_id={album_artist}"))
                    .await
            ),
            ["Kept"]
        );
        assert_eq!(
            names(
                &library
                    .page("albums", &format!("artist_id={album_artist}"))
                    .await
            ),
            ["Kept"]
        );
        assert_eq!(names(&library.page("albums", "genre=rock").await), ["Kept"]);
        assert_eq!(
            names(&library.page("albums", "role=composer").await),
            ["Local Only"]
        );
        assert_eq!(
            library.page("albums", "year_from=1990&year_to=2000").await["total"],
            json!(1)
        );
        assert_eq!(
            library.page("albums", "year_from=1999").await["total"],
            json!(1)
        );
        assert_eq!(
            names(&library.page("albums", "compilation=false").await),
            ["Kept"]
        );
        assert_eq!(
            names(&library.page("albums", "starred=true").await),
            ["Kept", "Provider Only"]
        );
        assert_eq!(
            library.page("albums", "has_rating=true").await["total"],
            json!(1)
        );
        assert_eq!(
            library.page("albums", "rating_from=4&rating_to=5").await["total"],
            json!(1)
        );
        assert_eq!(
            names(&library.page("albums", "recently_played=true").await),
            ["Kept"]
        );
        assert_eq!(
            names(&library.page("albums", "missing=true").await),
            ["Local Only"]
        );
        assert_eq!(
            names(&library.page("albums", "missing=false").await),
            ["Kept", "Provider Only"]
        );
        assert_eq!(
            names(&library.page("albums", &format!("library_id={music}")).await),
            ["Kept", "Local Only"]
        );
    }

    #[sqlx::test]
    async fn native_artist_page_is_role_scoped(pool: PgPool) {
        let library = Library::new(pool.clone()).await;
        let user = user_id(&pool).await;
        let ports = StorePorts::new(pool.clone());
        let catalogue = ports.catalogue();
        let fronted_album = catalogue
            .resolve_or_create_album(
                &album("Fronted Album").with_album_artist(credit("Fronted", None)),
            )
            .await
            .unwrap();
        let composed_album = catalogue
            .resolve_or_create_album(&album("Composed"))
            .await
            .unwrap();
        let mut member = None;

        for external in ["fronted-one.flac", "fronted-two.flac"] {
            let track = catalogue
                .get_or_create(source("local", external), metadata_by(external, "Fronted"))
                .await
                .unwrap();
            catalogue
                .set_album_track(fronted_album.id(), track.id(), &AlbumTrack::unplaced())
                .await
                .unwrap();
            member = Some(track.id().clone());
        }

        let composed = catalogue
            .get_or_create(
                source("local", "composed.flac"),
                metadata_by("composed", "Soloist")
                    .with_credits(vec![credit("Composer", Some(CreditRole::Composer))]),
            )
            .await
            .unwrap();
        catalogue
            .set_album_track(composed_album.id(), composed.id(), &AlbumTrack::unplaced())
            .await
            .unwrap();
        let fronted = named_artist(&pool, "Fronted").await;
        ports
            .state()
            .set_starred(user, StateTarget::Artist(&fronted), true)
            .await
            .unwrap();
        ports
            .state()
            .set_rating(user, StateTarget::Artist(&fronted), Some(5))
            .await
            .unwrap();
        ports
            .state()
            .record_play(user, member.as_ref().unwrap(), Utc::now())
            .await
            .unwrap();

        let album_artists = library.page("artists", "role=albumartist&order=name").await;

        assert_eq!(names(&album_artists), ["Fronted"]);
        assert_eq!(envelope_keys(&album_artists), ["items", "total"]);
        let item = &album_artists["items"][0];
        assert_eq!(item["id"], json!(fronted.to_string()));
        assert_eq!(item["album_count"], json!(1));
        assert_eq!(item["song_count"], json!(2));
        assert_eq!(item["starred"], json!(true));
        assert_eq!(item["rating"], json!(5));
        assert_eq!(item["play_count"], json!(1));
        let mut keys = item
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>();
        keys.sort();
        assert_eq!(
            keys,
            [
                "album_count",
                "id",
                "name",
                "play_count",
                "rating",
                "song_count",
                "starred"
            ]
        );

        let count = ports
            .library()
            .count_artists(
                Some(user),
                &ArtistListFilter::new()
                    .with_role(Some(RoleSet::new([RoleSelector::AlbumArtist]).unwrap())),
            )
            .await
            .unwrap();
        assert_eq!(album_artists["total"], json!(count));
        assert_eq!(count, 1);

        let every = library.page("artists", "order=name").await;
        assert_eq!(names(&every), ["Composer", "Fronted", "Soloist"]);
        assert_eq!(every["total"], json!(3));
        assert_eq!(
            names(&library.page("artists", "role=composer").await),
            ["Composer"]
        );
        assert_eq!(
            names(&library.page("artists", "name=front").await),
            ["Fronted"]
        );
        assert_eq!(
            library.page("artists", "starred=true").await["total"],
            json!(1)
        );
        assert_eq!(
            library.page("artists", "has_rating=false").await["total"],
            json!(2)
        );
        assert_eq!(
            library.page("artists", "rating_from=5&rating_to=5").await["total"],
            json!(1)
        );
        library.rejected("artists", "scope=owned").await;
        library.rejected("artists", "seed=7").await;
    }

    #[sqlx::test]
    async fn native_direction_overrides_the_natural_order(pool: PgPool) {
        let library = Library::new(pool.clone()).await;
        let user = user_id(&pool).await;
        let ports = StorePorts::new(pool.clone());
        let catalogue = ports.catalogue();

        for (title, rating) in [("alpha", 5u8), ("bravo", 3), ("charlie", 1)] {
            let track = catalogue
                .get_or_create(
                    source("local", &format!("{title}.flac")),
                    metadata_by(title, "Artist"),
                )
                .await
                .unwrap();
            ports
                .state()
                .set_rating(user, StateTarget::Track(track.id()), Some(rating))
                .await
                .unwrap();
        }

        let natural = library.page("tracks", "order=title").await;
        assert_eq!(titles(&natural), ["alpha", "bravo", "charlie"]);
        let declared_ascending = library.page("tracks", "order=title&direction=asc").await;
        assert_eq!(
            declared_ascending["items"], natural["items"],
            "an absent direction is the sort's natural direction"
        );
        let descending = library.page("tracks", "order=title&direction=desc").await;
        assert_eq!(titles(&descending), ["charlie", "bravo", "alpha"]);
        assert_eq!(descending["total"], natural["total"]);

        let rating_natural = library.page("tracks", "order=rating").await;
        assert_eq!(
            titles(&rating_natural),
            ["alpha", "bravo", "charlie"],
            "rating's natural direction is descending"
        );
        let rating_descending = library.page("tracks", "order=rating&direction=desc").await;
        assert_eq!(rating_descending["items"], rating_natural["items"]);
        let rating_ascending = library.page("tracks", "order=rating&direction=asc").await;
        assert_eq!(titles(&rating_ascending), ["charlie", "bravo", "alpha"]);
        assert_eq!(rating_ascending["total"], rating_natural["total"]);
    }
}
