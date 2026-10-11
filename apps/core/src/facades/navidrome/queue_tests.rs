use chrono::{DateTime, Utc};
use serde_json::json;
use uuid::Uuid;

use super::{
    dto::{Queue, Song},
    service,
};
use crate::features::music_library::domain::{
    RestoreQueueEntry, RestoreQueueEntryId, RestoreQueueSnapshot, TrackId,
};

fn song(id: &str, title: &str) -> Song {
    Song {
        id: id.to_owned(),
        title: title.to_owned(),
        artist: "Artist".to_owned(),
        album: Some("Album".to_owned()),
        album_id: Some("album".to_owned()),
        album_artist: Some("Album Artist".to_owned()),
        track_number: Some(1),
        disc_number: Some(1),
        cover_art: Some("cover".to_owned()),
        duration: Some(180.0),
        suffix: None,
        codec: None,
        size: None,
        bit_rate: None,
        sample_rate: None,
        bit_depth: None,
        channels: None,
        starred: false,
        starred_at: None,
        rating: 0,
        play_count: 0,
        play_date: None,
    }
}

fn snapshot(
    tracks: Vec<TrackId>,
    current_index: Option<u32>,
    position_ms: u64,
    changed_at: &str,
) -> RestoreQueueSnapshot {
    RestoreQueueSnapshot::new(
        Uuid::now_v7(),
        Uuid::now_v7(),
        "flynn".to_owned(),
        current_index,
        position_ms,
        3,
        DateTime::parse_from_rfc3339(changed_at)
            .expect("a valid timestamp")
            .with_timezone(&Utc),
        tracks
            .into_iter()
            .enumerate()
            .map(|(position, track)| {
                RestoreQueueEntry::new(RestoreQueueEntryId::new(), position as u32, track)
            })
            .collect(),
    )
}

#[test]
fn the_read_carries_the_fields_the_client_reads() {
    let first = TrackId::new();
    let second = TrackId::new();
    let snapshot = snapshot(
        vec![first.clone(), second.clone()],
        Some(1),
        4_200,
        "2026-09-27T10:00:00Z",
    );

    let queue = service::queue_from(
        &snapshot,
        vec![
            song(&first.to_string(), "First"),
            song(&second.to_string(), "Second"),
        ],
    );
    let body = serde_json::to_value(&queue).expect("a serialisable queue");

    let keys = body
        .as_object()
        .expect("a queue object")
        .keys()
        .map(String::as_str)
        .collect::<Vec<_>>();
    assert_eq!(
        keys,
        ["changedBy", "current", "items", "position", "updatedAt"],
        "the fields navidrome-controller.ts:765 destructures"
    );
    assert_eq!(body["changedBy"], "flynn");
    assert_eq!(body["current"], 1);
    assert_eq!(body["position"], 4_200);
    assert_eq!(body["updatedAt"], "2026-09-27T10:00:00+00:00");
    assert_eq!(body["items"][0]["id"], first.to_string());
    assert_eq!(body["items"][0]["title"], "First");
    assert_eq!(body["items"][1]["id"], second.to_string());
}

#[test]
fn no_queue_and_an_empty_queue_share_one_body() {
    assert_eq!(
        serde_json::to_value(Queue::empty()).expect("a serialisable queue"),
        json!({
            "changedBy": "",
            "current": 0,
            "items": [],
            "position": 0,
            "updatedAt": "",
        })
    );
}

#[cfg(feature = "postgres-integration")]
mod persistence {
    use super::*;
    use axum::{
        Router,
        body::{Body, to_bytes},
        http::{Request, StatusCode},
    };
    use serde_json::Value;
    use sqlx::PgPool;
    use std::sync::Arc;
    use tower::ServiceExt;

    use crate::{
        facades::SourceTags,
        facades::navidrome::{
            queue_router, session,
            state::{NavidromeQueueState, NavidromeState},
        },
        features::{
            identity::{
                domain::{ApiKeyCredential, AppPasswordCredential, User, UserId},
                secrets::CredentialKey,
                service::CredentialService,
                test_support::{CredentialRepository, CredentialStoreError},
            },
            music_library::{
                LibraryWiring, W4Dependencies,
                catalogue::DedupMode,
                construct,
                domain::{ProviderId, SourceIdentity, SourceScope, TrackMetadata},
                test_support::{CatalogueWrite, PostgresTrackRepository},
            },
        },
        platform::request::{RequestContext, RequestId},
    };

    struct StubCredentials {
        principals: Vec<(UserId, Uuid)>,
    }

    #[async_trait::async_trait]
    impl CredentialRepository for StubCredentials {
        async fn find_user_by_username(
            &self,
            _username: &str,
        ) -> Result<Option<User>, CredentialStoreError> {
            Ok(None)
        }

        async fn list_users(&self) -> Result<Vec<User>, CredentialStoreError> {
            Ok(Vec::new())
        }

        async fn find_user_by_id(&self, _id: UserId) -> Result<Option<User>, CredentialStoreError> {
            Ok(None)
        }

        async fn find_active_api_key_by_digest(
            &self,
            _digest: [u8; 32],
        ) -> Result<Option<ApiKeyCredential>, CredentialStoreError> {
            Ok(None)
        }

        async fn list_active_app_passwords(
            &self,
            _user_id: UserId,
        ) -> Result<Vec<AppPasswordCredential>, CredentialStoreError> {
            Ok(Vec::new())
        }

        async fn find_active_app_password(
            &self,
            credential_id: Uuid,
        ) -> Result<Option<AppPasswordCredential>, CredentialStoreError> {
            Ok(self
                .principals
                .iter()
                .find(|(_, credential)| *credential == credential_id)
                .map(|(user, credential)| AppPasswordCredential {
                    id: *credential,
                    user_id: *user,
                    encrypted_secret: Vec::new(),
                }))
        }
    }

    struct Fixture {
        app: Router,
        tokens: Vec<String>,
        store: Arc<PostgresTrackRepository>,
    }

    fn key() -> CredentialKey {
        CredentialKey::from_base64("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA")
            .expect("valid key")
    }

    async fn insert_user(pool: &PgPool, id: Uuid, username: &str) {
        sqlx::query("INSERT INTO users (id, username, display_name) VALUES ($1, $2, $3)")
            .bind(id)
            .bind(username)
            .bind(username)
            .execute(pool)
            .await
            .expect("a stored user");
    }

    async fn principal(pool: &PgPool, username: &str) -> (UserId, Uuid) {
        let user = UserId::from_uuid(Uuid::now_v7());
        insert_user(pool, user.as_uuid(), username).await;
        (user, Uuid::now_v7())
    }

    fn fixture(pool: PgPool, principals: Vec<(UserId, Uuid)>) -> Fixture {
        let store = Arc::new(PostgresTrackRepository::new(pool.clone()));
        let music = construct(LibraryWiring {
            pool,
            providers: Vec::new(),
            enabled_providers: Vec::new(),
            dedup_mode: DedupMode::Off,
            search_response_budget: std::time::Duration::from_secs(3),
            media_roots: Vec::new(),
            artwork_host_suffixes: Vec::new(),
            w4: W4Dependencies {
                metadata: None,
                artwork_cache: None,
                delivery: None,
            },
        });
        let key = key();
        let tokens = principals
            .iter()
            .map(|(user, credential)| session::mint(*user, *credential, &key))
            .collect();
        let credential = Arc::new(CredentialService::new(
            Arc::new(StubCredentials { principals }),
            key,
            crate::features::identity::LoginThrottleConfig::default(),
        ));
        let state = NavidromeQueueState {
            navidrome: NavidromeState {
                credential: Arc::clone(&credential),
                libraries: Arc::clone(&music.library_registry),
                completion: Arc::clone(&music.completion),
                playlists: Arc::clone(&music.playlists),
                library: Arc::clone(&music.library),
                source_tags: SourceTags::Off,
            },
            queue: Arc::clone(&music.queue),
        };
        let app = queue_router::<NavidromeQueueState>()
            .layer(axum::Extension(RequestContext::new(RequestId::generate())))
            .with_state(state);

        Fixture { app, tokens, store }
    }

    async fn track(store: &PostgresTrackRepository, title: &str) -> TrackId {
        let source = SourceIdentity::new(
            ProviderId::new("local".to_owned()).expect("a valid provider"),
            SourceScope::Named("/music".to_owned()),
            format!("{title}.flac"),
        )
        .expect("a valid source");

        store
            .get_or_create(
                source,
                TrackMetadata::from_names(title.to_owned(), vec!["Artist".to_owned()], Some(1_000))
                    .expect("valid metadata"),
            )
            .await
            .expect("a stored track")
            .id()
            .clone()
    }

    async fn request(
        app: &Router,
        method: &str,
        token: Option<&str>,
        body: Option<Value>,
    ) -> (StatusCode, Value) {
        let mut builder = Request::builder().method(method).uri("/api/queue");
        if let Some(token) = token {
            builder = builder.header("x-nd-authorization", format!("Bearer {token}"));
        }
        let request = match body {
            Some(body) => builder
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .expect("a request"),
            None => builder.body(Body::empty()).expect("a request"),
        };

        let response = app.clone().oneshot(request).await.expect("a response");
        let status = response.status();
        let bytes = to_bytes(response.into_body(), 8 * 1024 * 1024)
            .await
            .expect("a body");

        (status, serde_json::from_slice(&bytes).expect("a json body"))
    }

    async fn read(app: &Router, token: &str) -> (StatusCode, Value) {
        request(app, "GET", Some(token), None).await
    }

    async fn save(app: &Router, token: &str, body: Value) -> (StatusCode, Value) {
        request(app, "POST", Some(token), Some(body)).await
    }

    fn ids(body: &Value) -> Vec<String> {
        body["items"]
            .as_array()
            .expect("an item list")
            .iter()
            .map(|item| item["id"].as_str().expect("a track id").to_owned())
            .collect()
    }

    fn no_queue() -> Value {
        json!({
            "changedBy": "",
            "current": 0,
            "items": [],
            "position": 0,
            "updatedAt": "",
        })
    }

    #[sqlx::test]
    async fn a_saved_queue_reads_back_in_the_saved_order(pool: PgPool) {
        let (user, credential) = principal(&pool, "flynn").await;
        let fixture = fixture(pool, vec![(user, credential)]);
        let first = track(&fixture.store, "first").await;
        let second = track(&fixture.store, "second").await;
        let third = track(&fixture.store, "third").await;

        let (status, _) = save(
            &fixture.app,
            &fixture.tokens[0],
            json!({
                "current": 1,
                "ids": [second.to_string(), first.to_string(), third.to_string()],
                "position": 4_200,
            }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "the client reads the status only");

        let (status, body) = read(&fixture.app, &fixture.tokens[0]).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            ids(&body),
            vec![second.to_string(), first.to_string(), third.to_string()]
        );
        assert_eq!(body["items"][0]["title"], "second");
        assert_eq!(body["current"], 1);
        assert_eq!(body["position"], 4_200);
        assert_eq!(body["changedBy"], "flynn");
    }

    #[sqlx::test]
    async fn a_second_principal_reads_empty_and_leaves_the_first_queue_alone(pool: PgPool) {
        let (first_user, first_credential) = principal(&pool, "flynn").await;
        let (second_user, second_credential) = principal(&pool, "sam").await;
        let fixture = fixture(
            pool,
            vec![
                (first_user, first_credential),
                (second_user, second_credential),
            ],
        );
        let only = track(&fixture.store, "only").await;

        let (status, _) = save(
            &fixture.app,
            &fixture.tokens[0],
            json!({"current": 0, "ids": [only.to_string()], "position": 1_000}),
        )
        .await;
        assert_eq!(status, StatusCode::OK);

        let (status, body) = read(&fixture.app, &fixture.tokens[1]).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, no_queue());

        let (status, body) = read(&fixture.app, &fixture.tokens[0]).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(ids(&body), vec![only.to_string()]);
        assert_eq!(body["position"], 1_000);
        assert_eq!(body["changedBy"], "flynn");
    }

    #[sqlx::test]
    async fn a_repeated_track_keeps_every_occurrence(pool: PgPool) {
        let (user, credential) = principal(&pool, "flynn").await;
        let fixture = fixture(pool, vec![(user, credential)]);
        let alpha = track(&fixture.store, "alpha").await;
        let bravo = track(&fixture.store, "bravo").await;

        let (status, _) = save(
            &fixture.app,
            &fixture.tokens[0],
            json!({
                "current": 2,
                "ids": [alpha.to_string(), bravo.to_string(), alpha.to_string()],
                "position": 1_000,
            }),
        )
        .await;
        assert_eq!(status, StatusCode::OK);

        let (status, body) = read(&fixture.app, &fixture.tokens[0]).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            ids(&body),
            vec![alpha.to_string(), bravo.to_string(), alpha.to_string()]
        );
        assert_eq!(
            body["items"].as_array().expect("an item list").len(),
            3,
            "every occurrence is a separate item"
        );
        assert_eq!(
            body["current"], 2,
            "the index still names the third occurrence"
        );
        assert_eq!(body["items"][2]["id"], alpha.to_string());
        assert_eq!(body["position"], 1_000);
    }

    #[sqlx::test]
    async fn a_position_the_store_cannot_carry_is_rejected(pool: PgPool) {
        let (user, credential) = principal(&pool, "flynn").await;
        let fixture = fixture(pool, vec![(user, credential)]);
        let only = track(&fixture.store, "only").await;

        let too_large = i64::MAX as u64 + 1;
        let (status, body) = save(
            &fixture.app,
            &fixture.tokens[0],
            json!({
                "current": 0,
                "ids": [only.to_string()],
                "position": too_large,
            }),
        )
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(body["error"], "Invalid request.");

        let (status, body) = read(&fixture.app, &fixture.tokens[0]).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, no_queue(), "the refused save wrote nothing");

        let boundary = i64::MAX as u64;
        let (status, _) = save(
            &fixture.app,
            &fixture.tokens[0],
            json!({
                "current": 0,
                "ids": [only.to_string()],
                "position": boundary,
            }),
        )
        .await;
        assert_eq!(status, StatusCode::OK);

        let (status, body) = read(&fixture.app, &fixture.tokens[0]).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(ids(&body), vec![only.to_string()]);
        assert_eq!(body["position"], boundary);
    }

    #[sqlx::test]
    async fn an_unknown_track_id_is_rejected_without_writing(pool: PgPool) {
        let (user, credential) = principal(&pool, "flynn").await;
        let fixture = fixture(pool, vec![(user, credential)]);
        let known = track(&fixture.store, "known").await;

        let (status, _) = save(
            &fixture.app,
            &fixture.tokens[0],
            json!({"current": 0, "ids": [known.to_string()], "position": 500}),
        )
        .await;
        assert_eq!(status, StatusCode::OK);

        let unknown = TrackId::new();
        let (status, body) = save(
            &fixture.app,
            &fixture.tokens[0],
            json!({
                "current": 0,
                "ids": [known.to_string(), unknown.to_string()],
                "position": 900,
            }),
        )
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(body["error"], "Invalid request.");

        let (status, body) = read(&fixture.app, &fixture.tokens[0]).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(ids(&body), vec![known.to_string()]);
        assert_eq!(body["position"], 500);
    }

    #[sqlx::test]
    async fn an_empty_save_and_an_absent_queue_answer_alike(pool: PgPool) {
        let (user, credential) = principal(&pool, "flynn").await;
        let (other, other_credential) = principal(&pool, "sam").await;
        let fixture = fixture(pool, vec![(user, credential), (other, other_credential)]);
        let only = track(&fixture.store, "only").await;

        let (status, _) = save(
            &fixture.app,
            &fixture.tokens[0],
            json!({"current": 0, "ids": [only.to_string()], "position": 700}),
        )
        .await;
        assert_eq!(status, StatusCode::OK);

        let (status, _) = save(&fixture.app, &fixture.tokens[0], json!({})).await;
        assert_eq!(status, StatusCode::OK);

        let (status, saved_empty) = read(&fixture.app, &fixture.tokens[0]).await;
        assert_eq!(status, StatusCode::OK);
        let (status, never_saved) = read(&fixture.app, &fixture.tokens[1]).await;
        assert_eq!(status, StatusCode::OK);

        assert_eq!(saved_empty, no_queue());
        assert_eq!(never_saved, no_queue());
    }

    #[sqlx::test]
    async fn a_forged_token_is_refused(pool: PgPool) {
        let (user, credential) = principal(&pool, "flynn").await;
        let fixture = fixture(pool, vec![(user, credential)]);

        let (status, body) = read(&fixture.app, "forged").await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(body["error"], "Not authenticated");

        let (status, _) = request(&fixture.app, "GET", None, None).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }
}
