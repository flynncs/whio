#![cfg(feature = "postgres-integration")]

use base64::Engine;
use base64::engine::general_purpose::STANDARD_NO_PAD;
use chrono::{DateTime, Duration, Utc};
use sqlx::PgPool;
use uuid::Uuid;
use whio_core::identity::domain::UserId;
use whio_core::identity::postgres::PostgresCredentialRepository;
use whio_core::identity::repository::CredentialRepository;
use whio_core::identity::secrets::{
    self, CredentialKey, decrypt_secret, encrypt_secret, generate_app_secret,
};

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
#[ignore = "requires PostgreSQL"]
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
#[ignore = "requires PostgreSQL"]
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
#[ignore = "requires PostgreSQL"]
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
