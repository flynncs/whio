#![cfg(feature = "postgres-integration")]

use sqlx::PgPool;
use whio_core::identity::{
    provisioning,
    secrets::CredentialKey,
    upstream::{self, UpstreamStoreError},
};

fn key() -> CredentialKey {
    CredentialKey::from_base64("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA").expect("valid key")
}

async fn user(pool: &PgPool) -> String {
    provisioning::create_user(pool, "link-user", "Link User")
        .await
        .unwrap();
    "link-user".to_owned()
}

#[sqlx::test]
#[ignore = "requires PostgreSQL"]
async fn duplicate_server_registration_is_rejected(pool: PgPool) {
    let id = upstream::add_server(
        &pool,
        &key(),
        "https://music.example.test",
        "admin",
        "admin-password",
    )
    .await
    .unwrap();

    let servers = upstream::list_servers(&pool).await.unwrap();
    assert_eq!(servers.len(), 1);
    assert_eq!(servers[0].id, id);
    assert_eq!(servers[0].base_url, "https://music.example.test");

    let error = upstream::add_server(
        &pool,
        &key(),
        "https://MUSIC.example.test",
        "admin",
        "admin-password",
    )
    .await
    .unwrap_err();
    assert!(matches!(error, UpstreamStoreError::ServerAlreadyExists));
}

#[sqlx::test]
#[ignore = "requires PostgreSQL"]
async fn link_lifecycle_enforces_known_user_server_and_uniqueness(pool: PgPool) {
    let username = user(&pool).await;
    upstream::add_server(
        &pool,
        &key(),
        "https://music.example.test",
        "admin",
        "admin-password",
    )
    .await
    .unwrap();

    let error = upstream::store_link(
        &pool,
        &key(),
        "nobody",
        "https://music.example.test",
        "friend",
        "password",
    )
    .await
    .unwrap_err();
    assert!(matches!(error, UpstreamStoreError::UserNotFound));

    let error = upstream::store_link(
        &pool,
        &key(),
        &username,
        "https://unknown.example.test",
        "friend",
        "password",
    )
    .await
    .unwrap_err();
    assert!(matches!(error, UpstreamStoreError::ServerNotFound));

    upstream::store_link(
        &pool,
        &key(),
        &username,
        "https://music.example.test",
        "friend",
        "password",
    )
    .await
    .unwrap();

    let error = upstream::store_link(
        &pool,
        &key(),
        &username,
        "https://music.example.test",
        "friend",
        "password",
    )
    .await
    .unwrap_err();
    assert!(matches!(error, UpstreamStoreError::AlreadyLinked));

    assert!(
        upstream::remove_link(&pool, &username, "https://music.example.test")
            .await
            .unwrap()
    );
    assert!(
        !upstream::remove_link(&pool, &username, "https://music.example.test")
            .await
            .unwrap()
    );
}

#[sqlx::test]
#[ignore = "requires PostgreSQL"]
async fn relink_updates_the_password_and_clears_stale(pool: PgPool) {
    let username = user(&pool).await;
    upstream::add_server(
        &pool,
        &key(),
        "https://music.example.test",
        "admin",
        "admin-password",
    )
    .await
    .unwrap();
    upstream::store_link(
        &pool,
        &key(),
        &username,
        "https://music.example.test",
        "friend",
        "old-password",
    )
    .await
    .unwrap();

    let error = upstream::update_link(
        &pool,
        &key(),
        "nobody",
        "https://music.example.test",
        "friend",
        "new-password",
    )
    .await
    .unwrap_err();
    assert!(matches!(error, UpstreamStoreError::UserNotFound));

    let server_id = upstream::list_servers(&pool).await.unwrap()[0].id;
    upstream::mark_stale(&pool, &user_id(&pool, &username).await, &server_id)
        .await
        .unwrap();
    assert!(
        upstream::links_for_user(&pool, &user_id(&pool, &username).await)
            .await
            .unwrap()
            .is_empty()
    );

    assert!(
        upstream::update_link(
            &pool,
            &key(),
            &username,
            "https://music.example.test",
            "friend",
            "new-password",
        )
        .await
        .unwrap()
    );
    let links = upstream::links_for_user(&pool, &user_id(&pool, &username).await)
        .await
        .unwrap();
    assert_eq!(links.len(), 1);
    let password =
        upstream::decrypt_password(&key(), links[0].link_id, &links[0].encrypted_password).unwrap();
    assert_eq!(password, "new-password");
}

async fn user_id(pool: &PgPool, username: &str) -> uuid::Uuid {
    sqlx::query_scalar::<_, uuid::Uuid>("SELECT id FROM users WHERE username = $1")
        .bind(username)
        .fetch_one(pool)
        .await
        .unwrap()
}

#[sqlx::test]
#[ignore = "requires PostgreSQL"]
async fn fresh_links_list_and_stale_links_hide(pool: PgPool) {
    provisioning::create_user(&pool, "reader", "Reader")
        .await
        .unwrap();
    upstream::add_server(
        &pool,
        &key(),
        "https://music.example.test",
        "admin",
        "admin-password",
    )
    .await
    .unwrap();
    upstream::store_link(
        &pool,
        &key(),
        "reader",
        "https://music.example.test",
        "friend",
        "password",
    )
    .await
    .unwrap();

    let user_id =
        sqlx::query_scalar::<_, uuid::Uuid>("SELECT id FROM users WHERE username = 'reader'")
            .fetch_one(&pool)
            .await
            .unwrap();
    let links = upstream::links_for_user(&pool, &user_id).await.unwrap();
    assert_eq!(links.len(), 1);
    assert_eq!(links[0].username, "friend");
    assert_eq!(links[0].base_url, "https://music.example.test");
    assert!(!links[0].encrypted_password.is_empty());

    upstream::mark_stale(&pool, &user_id, &links[0].server_id)
        .await
        .unwrap();
    assert!(
        upstream::links_for_user(&pool, &user_id)
            .await
            .unwrap()
            .is_empty()
    );
}
