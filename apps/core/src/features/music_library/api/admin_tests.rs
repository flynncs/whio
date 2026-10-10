use std::sync::Arc;

use axum::{
    Router,
    body::{Body, to_bytes},
    extract::Request,
    http::{Request as HttpRequest, StatusCode},
    middleware::Next,
    response::Response,
};
use base64::Engine;
use base64::engine::general_purpose::STANDARD_NO_PAD;
use serde_json::{Value, json};
use sqlx::PgPool;
use tower::ServiceExt;
use uuid::Uuid;

use crate::features::identity::secrets::CredentialKey;
use crate::features::music_library::{
    api::admin::{self, AdminState},
    catalogue::LibraryService,
    domain::{PlaylistId, ProviderId, SourceIdentity, SourceScope, TrackId, TrackMetadata},
    playlists::{PlaylistEdit, PlaylistService},
    storage::contracts::{CatalogueWrite, LibraryRead, PlaylistStore, SourceLookup, StateStore},
    test_support::PostgresTrackRepository,
};
use crate::platform::request::{RequestContext, RequestId};

fn local_source(external_id: &str) -> SourceIdentity {
    SourceIdentity::new(
        ProviderId::new("local".to_owned()).unwrap(),
        SourceScope::Named("/music".to_owned()),
        external_id.to_owned(),
    )
    .unwrap()
}

fn metadata(title: &str) -> TrackMetadata {
    TrackMetadata::from_names(title.to_owned(), vec!["Artist".to_owned()], Some(1_000)).unwrap()
}

async fn context(mut request: Request, next: Next) -> Response {
    request
        .extensions_mut()
        .insert(RequestContext::new(RequestId::generate()));

    next.run(request).await
}

fn app(state: AdminState) -> Router {
    admin::routes()
        .with_state(state)
        .layer(axum::middleware::from_fn(context))
}

fn auth(username: &str, secret: &str) -> String {
    format!("?u={username}&v=1.16.1&c=integration-test&p={secret}")
}

fn with_auth(uri: &str, auth: &str) -> String {
    let separator = if uri.contains('?') { '&' } else { '?' };

    format!("{uri}{separator}{}", auth.trim_start_matches('?'))
}

async fn send(app: &Router, method: &str, uri: &str, body: Option<&Value>) -> Response {
    let builder = HttpRequest::builder().method(method).uri(uri);
    let request = match body {
        Some(body) => builder
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap(),
        None => builder.body(Body::empty()).unwrap(),
    };

    app.clone().oneshot(request).await.unwrap()
}

async fn body_of(response: Response) -> Value {
    let bytes = to_bytes(response.into_body(), 16 * 1024 * 1024)
        .await
        .unwrap();

    serde_json::from_slice(&bytes).unwrap()
}

struct Fixture {
    app: Router,
    pool: PgPool,
    store: Arc<PostgresTrackRepository>,
    service: Arc<PlaylistService>,
    owner: Uuid,
    target: Uuid,
    admin: Uuid,
    owner_auth: String,
    target_auth: String,
    admin_auth: String,
}

async fn fixture(pool: PgPool) -> Fixture {
    let key = CredentialKey::from_base64(&STANDARD_NO_PAD.encode([11u8; 32])).unwrap();
    let provision = crate::features::identity::provision(pool.clone());
    let owner = provision.create_user("admin-owner", "Owner").await.unwrap();
    let target = provision
        .create_user("admin-target", "Target")
        .await
        .unwrap();
    let admin = provision.create_user("admin-admin", "Admin").await.unwrap();
    provision.assign_admin("admin-admin").await.unwrap();
    let (_, owner_secret) = provision
        .mint_app_password(&key, "admin-owner", "integration")
        .await
        .unwrap();
    let (_, target_secret) = provision
        .mint_app_password(&key, "admin-target", "integration")
        .await
        .unwrap();
    let (_, admin_secret) = provision
        .mint_app_password(&key, "admin-admin", "integration")
        .await
        .unwrap();

    let store = Arc::new(PostgresTrackRepository::new(pool.clone()));
    let library = Arc::new(LibraryService::new(
        Arc::clone(&store) as Arc<dyn LibraryRead>,
        Arc::clone(&store) as Arc<dyn SourceLookup>,
        Arc::clone(&store) as Arc<dyn StateStore>,
    ));
    let service = Arc::new(PlaylistService::new(
        Arc::clone(&store) as Arc<dyn PlaylistStore>,
        library,
    ));
    let credential = Arc::new(crate::features::identity::credentials(
        pool.clone(),
        key,
        crate::features::identity::LoginThrottleConfig::default(),
    ));
    let app = app(AdminState {
        playlists: Arc::clone(&service),
        credential,
    });

    Fixture {
        app,
        pool,
        store,
        service,
        owner,
        target,
        admin,
        owner_auth: auth("admin-owner", &owner_secret),
        target_auth: auth("admin-target", &target_secret),
        admin_auth: auth("admin-admin", &admin_secret),
    }
}

async fn track(store: &Arc<PostgresTrackRepository>, name: &str) -> TrackId {
    store
        .get_or_create(local_source(&format!("{name}.flac")), metadata(name))
        .await
        .unwrap()
        .id()
        .clone()
}

impl Fixture {
    async fn shared_playlist(&self) -> (PlaylistId, TrackId) {
        let first = track(&self.store, "First").await;
        let playlist = self
            .service
            .create(self.owner, "road trip", None)
            .await
            .unwrap();
        assert!(
            self.service
                .add_track(self.owner, playlist.id(), &first)
                .await
                .unwrap()
        );
        self.service
            .update(self.owner, playlist.id(), "road trip", None, true)
            .await
            .unwrap()
            .unwrap();

        (playlist.id().clone(), first)
    }

    async fn transfer(&self, auth: &str, id: &PlaylistId, username: &str) -> Response {
        send(
            &self.app,
            "PUT",
            &with_auth(&format!("/api/v1/admin/playlists/{id}/owner"), auth),
            Some(&json!({ "username": username })),
        )
        .await
    }

    async fn edit(&self, auth: &str, id: &PlaylistId, body: &Value) -> Response {
        send(
            &self.app,
            "PATCH",
            &with_auth(&format!("/api/v1/admin/playlists/{id}"), auth),
            Some(body),
        )
        .await
    }
}

#[sqlx::test]
async fn an_admin_transfer_through_the_admin_surface_moves_the_playlist(pool: PgPool) {
    let fixture = fixture(pool).await;
    let (playlist_id, first) = fixture.shared_playlist().await;

    let response = fixture
        .transfer(&fixture.admin_auth, &playlist_id, "admin-target")
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_of(response).await;
    assert_eq!(body["owner"]["id"], fixture.target.to_string());
    assert_eq!(body["owner"]["username"], "admin-target");
    assert_eq!(body["id"], playlist_id.to_string());

    let owned = fixture
        .service
        .detail(fixture.target, &playlist_id)
        .await
        .unwrap()
        .expect("the new owner reads the playlist");
    assert!(owned.editable);
    assert_eq!(owned.playlist.owner_id(), fixture.target);
    assert!(
        fixture
            .service
            .edit(
                fixture.target,
                &playlist_id,
                PlaylistEdit {
                    name: Some("mine now"),
                    comment: None,
                    public: None,
                    additions: &[],
                    removals: Vec::new(),
                },
            )
            .await
            .unwrap()
    );

    let shared = fixture
        .service
        .detail(fixture.owner, &playlist_id)
        .await
        .unwrap()
        .expect("the public playlist stays readable by its former owner");
    assert!(!shared.editable);
    assert!(
        !fixture
            .service
            .add_track(fixture.owner, &playlist_id, &first)
            .await
            .unwrap()
    );
    let listed = fixture.service.list(fixture.admin).await.unwrap();
    assert_eq!(
        listed.len(),
        1,
        "the public playlist is listed for the admin who called the transfer"
    );
    assert_eq!(
        listed[0].owner_id(),
        fixture.target,
        "the transfer names the target, not the admin who called it"
    );
}

#[sqlx::test]
async fn a_non_admin_transfer_through_the_admin_surface_is_forbidden(pool: PgPool) {
    let fixture = fixture(pool).await;
    let (playlist_id, _) = fixture.shared_playlist().await;
    let before = fixture
        .store
        .find_playlist(&playlist_id)
        .await
        .unwrap()
        .unwrap();

    let response = fixture
        .transfer(&fixture.owner_auth, &playlist_id, "admin-target")
        .await;
    assert_eq!(
        response.status(),
        StatusCode::NOT_FOUND,
        "the directory does not name another user to a member"
    );
    let body = body_of(response).await;
    assert_eq!(body["code"], "not_found");
    assert_eq!(
        fixture
            .store
            .find_playlist(&playlist_id)
            .await
            .unwrap()
            .unwrap(),
        before
    );

    let response = fixture
        .transfer(&fixture.owner_auth, &playlist_id, "admin-owner")
        .await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    let body = body_of(response).await;
    assert_eq!(body["code"], "forbidden");
    assert_eq!(
        fixture
            .store
            .find_playlist(&playlist_id)
            .await
            .unwrap()
            .unwrap(),
        before
    );
}

#[sqlx::test]
async fn an_unknown_username_is_a_client_error_creating_nothing(pool: PgPool) {
    let fixture = fixture(pool).await;
    let (playlist_id, _) = fixture.shared_playlist().await;
    let before = fixture
        .store
        .find_playlist(&playlist_id)
        .await
        .unwrap()
        .unwrap();
    let users_before: i64 = sqlx::query_scalar("SELECT count(*) FROM users")
        .fetch_one(&fixture.pool)
        .await
        .unwrap();
    let playlists_before: i64 = sqlx::query_scalar("SELECT count(*) FROM playlists")
        .fetch_one(&fixture.pool)
        .await
        .unwrap();

    let response = fixture
        .transfer(&fixture.admin_auth, &playlist_id, "ghost")
        .await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let body = body_of(response).await;
    assert_eq!(body["code"], "not_found");

    let users_after: i64 = sqlx::query_scalar("SELECT count(*) FROM users")
        .fetch_one(&fixture.pool)
        .await
        .unwrap();
    let playlists_after: i64 = sqlx::query_scalar("SELECT count(*) FROM playlists")
        .fetch_one(&fixture.pool)
        .await
        .unwrap();
    assert_eq!(users_after, users_before);
    assert_eq!(playlists_after, playlists_before);
    assert_eq!(
        fixture
            .store
            .find_playlist(&playlist_id)
            .await
            .unwrap()
            .unwrap(),
        before
    );
}

#[sqlx::test]
async fn a_transfer_to_the_current_owner_is_reported_unchanged(pool: PgPool) {
    let fixture = fixture(pool).await;
    let (playlist_id, _) = fixture.shared_playlist().await;
    let before = fixture
        .store
        .find_playlist(&playlist_id)
        .await
        .unwrap()
        .unwrap();

    let response = fixture
        .transfer(&fixture.admin_auth, &playlist_id, "admin-owner")
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_of(response).await;
    assert_eq!(body["owner"]["id"], fixture.owner.to_string());
    assert_eq!(body["revision"], before.revision());
    assert_eq!(body["updated_at"], before.updated_at().to_rfc3339());
    assert_eq!(
        fixture
            .store
            .find_playlist(&playlist_id)
            .await
            .unwrap()
            .unwrap(),
        before
    );
}

#[sqlx::test]
async fn an_admin_edits_another_users_playlist_through_the_admin_surface(pool: PgPool) {
    let fixture = fixture(pool).await;
    let (playlist_id, _) = fixture.shared_playlist().await;
    let second = track(&fixture.store, "Second").await;
    let before = fixture
        .store
        .find_playlist(&playlist_id)
        .await
        .unwrap()
        .unwrap();

    let response = fixture
        .edit(
            &fixture.admin_auth,
            &playlist_id,
            &json!({ "name": "curated", "additions": [second.to_string()] }),
        )
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_of(response).await;
    assert_eq!(body["name"], "curated");
    assert_eq!(body["entry_count"], 2);
    assert_eq!(body["owner"]["id"], fixture.owner.to_string());
    assert_eq!(body["revision"], before.revision() + 1);

    let response = fixture
        .edit(
            &fixture.target_auth,
            &playlist_id,
            &json!({ "name": "hijack" }),
        )
        .await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    let body = body_of(response).await;
    assert_eq!(body["code"], "forbidden");
    assert_eq!(
        fixture
            .store
            .find_playlist(&playlist_id)
            .await
            .unwrap()
            .unwrap()
            .name(),
        "curated"
    );

    let response = fixture
        .edit(
            &fixture.admin_auth,
            &playlist_id,
            &json!({ "revision": before.revision(), "name": "stale" }),
        )
        .await;
    assert_eq!(response.status(), StatusCode::CONFLICT);
}

#[sqlx::test]
async fn an_admin_edit_of_an_unknown_playlist_is_not_found(pool: PgPool) {
    let fixture = fixture(pool).await;
    let missing = PlaylistId::from_uuid(Uuid::now_v7());

    let response = fixture
        .edit(&fixture.admin_auth, &missing, &json!({ "name": "ghost" }))
        .await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);

    let response = fixture
        .transfer(&fixture.admin_auth, &missing, "admin-target")
        .await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    assert_eq!(fixture.store.find_playlist(&missing).await.unwrap(), None);
}

#[sqlx::test]
async fn the_admin_surface_requires_authentication(pool: PgPool) {
    let fixture = fixture(pool).await;
    let (playlist_id, _) = fixture.shared_playlist().await;

    let response = send(
        &fixture.app,
        "PUT",
        &format!("/api/v1/admin/playlists/{playlist_id}/owner"),
        Some(&json!({ "username": "admin-target" })),
    )
    .await;

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[sqlx::test]
async fn a_malformed_transfer_body_is_rejected(pool: PgPool) {
    let fixture = fixture(pool).await;
    let (playlist_id, _) = fixture.shared_playlist().await;
    let before = fixture
        .store
        .find_playlist(&playlist_id)
        .await
        .unwrap()
        .unwrap();

    let response = fixture
        .transfer(&fixture.admin_auth, &playlist_id, "  ")
        .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);

    let response = send(
        &fixture.app,
        "PUT",
        &with_auth(
            &format!("/api/v1/admin/playlists/{playlist_id}/owner"),
            &fixture.admin_auth,
        ),
        Some(&json!({ "owner": "admin-target" })),
    )
    .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);

    assert_eq!(
        fixture
            .store
            .find_playlist(&playlist_id)
            .await
            .unwrap()
            .unwrap(),
        before
    );
}
