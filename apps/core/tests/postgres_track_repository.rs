#![cfg(feature = "postgres-integration")]

use std::sync::Arc;

use sqlx::PgPool;
use tokio::sync::Barrier;
use uuid::Uuid;
use whio_core::tracks::{
    PostgresTrackRepository, ProviderId, SourceIdentity, SourceScope, TrackId, TrackMetadata,
    TrackRepository, TrackRepositoryError,
};

fn source(external_id: &str) -> SourceIdentity {
    SourceIdentity::new(
        ProviderId::new("youtube_music".to_owned()).unwrap(),
        SourceScope::Named("test-installation".to_owned()),
        external_id.to_owned(),
    )
    .unwrap()
}

fn metadata(title: &str, artists: &[&str], duration_ms: Option<u64>) -> TrackMetadata {
    TrackMetadata::new(
        title.to_owned(),
        artists.iter().map(|artist| (*artist).to_owned()).collect(),
        duration_ms,
    )
    .unwrap()
}

#[sqlx::test]
#[ignore = "requires PostgreSQL"]
async fn repeated_discovery_preserves_identity_and_updates_metadata(pool: PgPool) {
    let repository = PostgresTrackRepository::new(pool.clone());
    let source = source("round-trip-source");

    let first = repository
        .get_or_create(
            source.clone(),
            metadata("Old title", &["First", "Second"], Some(1_000)),
        )
        .await
        .unwrap();
    let updated = repository
        .get_or_create(
            source.clone(),
            metadata("New title", &["Replacement"], Some(2_000)),
        )
        .await
        .unwrap();

    assert_eq!(first.id(), updated.id());
    assert_eq!(updated.title(), "New title");
    assert_eq!(updated.artists(), ["Replacement"]);
    assert_eq!(updated.duration_ms(), Some(2_000));
    assert_eq!(
        repository.find_sources(updated.id()).await.unwrap(),
        vec![source]
    );

    let stored_track: (String, Option<i64>) =
        sqlx::query_as("SELECT title, duration_ms FROM tracks WHERE id = $1")
            .bind(updated.id().as_uuid())
            .fetch_one(&pool)
            .await
            .unwrap();
    let stored_credits: Vec<(i32, String)> = sqlx::query_as(
        "SELECT position, name FROM track_artist_credits WHERE track_id = $1 ORDER BY position",
    )
    .bind(updated.id().as_uuid())
    .fetch_all(&pool)
    .await
    .unwrap();

    assert_eq!(stored_track, ("New title".to_owned(), Some(2_000)));
    assert_eq!(stored_credits, vec![(0, "Replacement".to_owned())]);
}

#[sqlx::test]
#[ignore = "requires PostgreSQL"]
async fn simultaneous_discovery_produces_one_track_without_orphans(pool: PgPool) {
    const DISCOVERY_COUNT: usize = 8;

    let repository = Arc::new(PostgresTrackRepository::new(pool.clone()));
    let barrier = Arc::new(Barrier::new(DISCOVERY_COUNT));
    let mut tasks = Vec::with_capacity(DISCOVERY_COUNT);

    for observation in 0..DISCOVERY_COUNT {
        let repository = Arc::clone(&repository);
        let barrier = Arc::clone(&barrier);

        tasks.push(tokio::spawn(async move {
            barrier.wait().await;
            repository
                .get_or_create(
                    source("concurrent-source"),
                    metadata(&format!("Observation {observation}"), &["Artist"], None),
                )
                .await
        }));
    }

    let mut track_ids = Vec::with_capacity(DISCOVERY_COUNT);
    for task in tasks {
        track_ids.push(task.await.unwrap().unwrap().id().clone());
    }

    assert!(track_ids.iter().all(|track_id| track_id == &track_ids[0]));

    let track_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM tracks")
        .fetch_one(&pool)
        .await
        .unwrap();
    let source_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM track_sources")
        .fetch_one(&pool)
        .await
        .unwrap();
    let orphan_count: i64 = sqlx::query_scalar(
        r#"
        SELECT COUNT(*)
        FROM tracks AS tracks
        LEFT JOIN track_sources AS sources ON sources.track_id = tracks.id
        WHERE sources.track_id IS NULL
        "#,
    )
    .fetch_one(&pool)
    .await
    .unwrap();

    assert_eq!(track_count, 1);
    assert_eq!(source_count, 1);
    assert_eq!(orphan_count, 0);
}

#[sqlx::test]
#[ignore = "requires PostgreSQL"]
async fn malformed_stored_scope_is_rejected(pool: PgPool) {
    let track_id = TrackId::new();

    sqlx::query("INSERT INTO tracks (id, title) VALUES ($1, $2)")
        .bind(track_id.as_uuid())
        .bind("Title")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(
        r#"
        INSERT INTO track_sources (
            id,
            track_id,
            provider_id,
            source_scope,
            external_id
        )
        VALUES ($1, $2, $3, $4, $5)
        "#,
    )
    .bind(Uuid::now_v7())
    .bind(track_id.as_uuid())
    .bind("youtube_music")
    .bind("not-a-valid-scope")
    .bind("malformed-source")
    .execute(&pool)
    .await
    .unwrap();

    let repository = PostgresTrackRepository::new(pool);
    let error = repository.find_sources(&track_id).await.unwrap_err();

    assert!(matches!(error, TrackRepositoryError::InvalidData));
}
