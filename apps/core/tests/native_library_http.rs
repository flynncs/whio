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
    use chrono::{DateTime, Duration, Utc};
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
                AlbumMetadata, AlbumTrack, ProviderId, SourceIdentity, SourceScope, StateTarget,
                TrackId, TrackMetadata,
            },
            test_support::{CatalogueWrite, PostgresTrackRepository},
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
            "order=album",
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
}
