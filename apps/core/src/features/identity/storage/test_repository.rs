use super::PostgresCredentialRepository;
use super::provisioning::PostgresProvisioningStore;
use super::repository::{CredentialRepository, CredentialStoreError};
use crate::features::identity::domain::{UserId, UserRole};
use crate::features::identity::provisioning::{
    ProvisioningError, ProvisioningService, ProvisioningStore,
};
use crate::features::identity::secrets::{
    self, CredentialKey, decrypt_secret, encrypt_secret, generate_app_secret,
};
use base64::Engine;
use base64::engine::general_purpose::STANDARD_NO_PAD;
use chrono::{DateTime, Duration, Utc};
use sqlx::PgPool;
use std::sync::Arc;
use uuid::Uuid;

fn test_key() -> CredentialKey {
    CredentialKey::from_base64(&STANDARD_NO_PAD.encode([11u8; 32])).expect("valid test key")
}

async fn insert_user(pool: &PgPool, id: Uuid, username: &str) {
    sqlx::query("INSERT INTO users (id, username, display_name) VALUES ($1, $2, $3)")
        .bind(id)
        .bind(username)
        .bind("Display Name")
        .execute(pool)
        .await
        .unwrap();
}

async fn insert_api_key(
    pool: &PgPool,
    id: Uuid,
    user_id: Uuid,
    digest: [u8; 32],
    revoked_at: Option<DateTime<Utc>>,
) {
    sqlx::query(
        r#"
        INSERT INTO api_credentials (id, user_id, label, lookup_digest, revoked_at)
        VALUES ($1, $2, 'test', $3, $4)
        "#,
    )
    .bind(id)
    .bind(user_id)
    .bind(digest.as_slice())
    .bind(revoked_at)
    .execute(pool)
    .await
    .unwrap();
}

async fn insert_app_password(
    pool: &PgPool,
    key: &CredentialKey,
    id: Uuid,
    user_id: Uuid,
    secret: &str,
    created_at: DateTime<Utc>,
    revoked_at: Option<DateTime<Utc>>,
) {
    let encrypted_secret = encrypt_secret(key, secret.as_bytes(), id.as_bytes()).expect("valid");
    sqlx::query(
        r#"
        INSERT INTO subsonic_app_passwords (id, user_id, label, encrypted_secret, created_at, revoked_at)
        VALUES ($1, $2, 'test', $3, $4, $5)
        "#,
    )
    .bind(id)
    .bind(user_id)
    .bind(encrypted_secret)
    .bind(created_at)
    .bind(revoked_at)
    .execute(pool)
    .await
    .unwrap();
}

#[sqlx::test]

async fn users_are_found_case_insensitively_and_stored_form_is_preserved(pool: PgPool) {
    let user_id = Uuid::now_v7();
    insert_user(&pool, user_id, "Flynn").await;
    let repository = PostgresCredentialRepository::new(pool);

    let found = repository
        .find_user_by_username("fLyNn")
        .await
        .unwrap()
        .expect("case-insensitive match");

    assert_eq!(found.id.as_uuid(), user_id);
    assert_eq!(found.username, "Flynn");

    assert!(
        repository
            .find_user_by_username("nobody")
            .await
            .unwrap()
            .is_none()
    );
}

#[sqlx::test]

async fn api_key_lookups_match_active_digests_only(pool: PgPool) {
    let user_id = Uuid::now_v7();
    insert_user(&pool, user_id, "flynn").await;

    let active_secret = generate_app_secret();
    let revoked_secret = generate_app_secret();
    insert_api_key(
        &pool,
        Uuid::now_v7(),
        user_id,
        secrets::lookup_digest(&active_secret),
        None,
    )
    .await;
    insert_api_key(
        &pool,
        Uuid::now_v7(),
        user_id,
        secrets::lookup_digest(&revoked_secret),
        Some(Utc::now()),
    )
    .await;

    let repository = PostgresCredentialRepository::new(pool);

    let found = repository
        .find_active_api_key_by_digest(secrets::lookup_digest(&active_secret))
        .await
        .unwrap()
        .expect("active digest resolves");
    assert_eq!(found.user_id, UserId::from_uuid(user_id));

    assert!(
        repository
            .find_active_api_key_by_digest(secrets::lookup_digest(&revoked_secret))
            .await
            .unwrap()
            .is_none(),
        "revoked credentials must not resolve"
    );
    assert!(
        repository
            .find_active_api_key_by_digest(secrets::lookup_digest(&generate_app_secret()))
            .await
            .unwrap()
            .is_none()
    );
}

#[sqlx::test]

async fn app_password_listing_is_active_only_ordered_and_round_trips(pool: PgPool) {
    let key = test_key();

    let user_a = Uuid::now_v7();
    let user_b = Uuid::now_v7();
    insert_user(&pool, user_a, "a").await;
    insert_user(&pool, user_b, "b").await;

    let base = Utc::now();
    let first_id = Uuid::now_v7();
    let first_secret = generate_app_secret();
    let second_id = Uuid::now_v7();
    let second_secret = generate_app_secret();
    let revoked_id = Uuid::now_v7();
    let other_user_id = Uuid::now_v7();

    insert_app_password(&pool, &key, first_id, user_a, &first_secret, base, None).await;
    insert_app_password(
        &pool,
        &key,
        second_id,
        user_a,
        &second_secret,
        base + Duration::seconds(1),
        None,
    )
    .await;
    insert_app_password(
        &pool,
        &key,
        revoked_id,
        user_a,
        &generate_app_secret(),
        base + Duration::seconds(2),
        Some(base),
    )
    .await;
    insert_app_password(
        &pool,
        &key,
        other_user_id,
        user_b,
        &generate_app_secret(),
        base + Duration::seconds(3),
        None,
    )
    .await;

    let repository = PostgresCredentialRepository::new(pool);
    let listed = repository
        .list_active_app_passwords(UserId::from_uuid(user_a))
        .await
        .unwrap();

    assert_eq!(
        listed.iter().map(|c| c.id).collect::<Vec<_>>(),
        vec![first_id, second_id],
        "revoked rows and other users' rows are excluded, order follows created_at"
    );

    for (credential, expected) in listed.iter().zip([&first_secret, &second_secret]) {
        let recovered =
            decrypt_secret(&key, &credential.encrypted_secret, credential.id.as_bytes())
                .expect("round trip through storage preserves decryptability");
        assert_eq!(recovered, expected.as_bytes());
    }
}

#[sqlx::test]
async fn migration_backfills_existing_users_as_members(pool: PgPool) {
    let id = Uuid::now_v7();
    sqlx::query("INSERT INTO users (id, username, display_name) VALUES ($1, 'legacy', 'Legacy')")
        .bind(id)
        .execute(&pool)
        .await
        .unwrap();

    let stored: String = sqlx::query_scalar("SELECT role FROM users WHERE id = $1")
        .bind(id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(stored, "member");

    let repository = PostgresCredentialRepository::new(pool);
    let user = repository
        .find_user_by_username("legacy")
        .await
        .unwrap()
        .expect("row inserted before the role column existed");
    assert_eq!(user.role, UserRole::Member);
}

#[sqlx::test]
async fn role_round_trips_through_storage(pool: PgPool) {
    let id = Uuid::now_v7();
    insert_user(&pool, id, "flynn").await;

    let store = PostgresProvisioningStore::new(pool.clone());
    store
        .set_role(UserId::from_uuid(id), UserRole::Admin)
        .await
        .unwrap();

    let repository = PostgresCredentialRepository::new(pool.clone());
    let user = repository
        .find_user_by_username("flynn")
        .await
        .unwrap()
        .expect("user exists");
    assert_eq!(user.role, UserRole::Admin);

    let fresh_connection = PostgresCredentialRepository::new(pool);
    let listed = fresh_connection.list_users().await.unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].id, UserId::from_uuid(id));
    assert_eq!(listed[0].role, UserRole::Admin);
}

#[sqlx::test]
async fn assign_admin_is_idempotent_and_reports_the_role(pool: PgPool) {
    let service = ProvisioningService::new(Arc::new(PostgresProvisioningStore::new(pool.clone())));
    service.create_user("flynn", "Flynn").await.unwrap();

    assert_eq!(
        service.assign_admin("flynn").await.unwrap(),
        UserRole::Admin
    );
    assert_eq!(
        service.assign_admin("fLyNn").await.unwrap(),
        UserRole::Admin
    );
    assert!(matches!(
        service.assign_admin("nobody").await,
        Err(ProvisioningError::UserNotFound)
    ));

    let repository = PostgresCredentialRepository::new(pool);
    let listed = repository.list_users().await.unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].role, UserRole::Admin);
}

#[sqlx::test]
async fn unknown_stored_role_is_invalid_data(pool: PgPool) {
    let rejected = sqlx::query(
        "INSERT INTO users (id, username, display_name, role) VALUES ($1, 'root-user', 'Root', 'root')",
    )
    .bind(Uuid::now_v7())
    .execute(&pool)
    .await;
    assert!(
        rejected.is_err(),
        "the CHECK constraint must reject roles outside the allowlist"
    );

    assert_eq!(UserRole::parse("root"), None);
    assert_eq!(UserRole::parse("Admin"), None);

    sqlx::query("ALTER TABLE users DROP CONSTRAINT users_role_check")
        .execute(&pool)
        .await
        .unwrap();
    let id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO users (id, username, display_name, role) VALUES ($1, 'legacy-root', 'Root', 'root')",
    )
    .bind(id)
    .execute(&pool)
    .await
    .unwrap();

    let repository = PostgresCredentialRepository::new(pool);
    assert_eq!(
        repository
            .find_user_by_username("legacy-root")
            .await
            .unwrap_err(),
        CredentialStoreError::InvalidData
    );
    assert_eq!(
        repository.list_users().await.unwrap_err(),
        CredentialStoreError::InvalidData
    );
}
