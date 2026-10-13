use super::PostgresTrackRepository;
use crate::features::music_library::domain::{AlbumId, AlbumMetadata};
use crate::features::music_library::storage::contracts::CatalogueWrite;
use sqlx::PgPool;
use uuid::Uuid;

const PRE_0037: &str = r#"
DROP INDEX albums_text_idx;
ALTER TABLE albums DROP COLUMN provider_source_key;
CREATE UNIQUE INDEX albums_text_idx ON albums (album_artist_id, title_normalized)
    NULLS NOT DISTINCT WHERE musicbrainz_release_group_id IS NULL;
"#;

const PROVIDER_SOURCE_KEY_MIGRATION: &str =
    include_str!("../../../../../migrations/0037_album_provider_source_key.sql");

async fn seed_album(pool: &PgPool, id: Uuid, title: &str, artist_id: Uuid) {
    sqlx::query("INSERT INTO albums (id, title, album_artist_id) VALUES ($1, $2, $3)")
        .bind(id)
        .bind(title)
        .bind(artist_id)
        .execute(pool)
        .await
        .unwrap();
}

async fn seed_album_source(
    pool: &PgPool,
    album_id: Uuid,
    provider_id: &str,
    source_scope: &str,
    external_id: &str,
) {
    sqlx::query(
        "INSERT INTO album_sources (id, album_id, provider_id, source_scope, external_id) VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(Uuid::now_v7())
    .bind(album_id)
    .bind(provider_id)
    .bind(source_scope)
    .bind(external_id)
    .execute(pool)
    .await
    .unwrap();
}

async fn provider_source_key(pool: &PgPool, album_id: Uuid) -> Option<String> {
    sqlx::query_scalar("SELECT provider_source_key FROM albums WHERE id = $1")
        .bind(album_id)
        .fetch_one(pool)
        .await
        .unwrap()
}

#[sqlx::test]
async fn the_provider_source_key_migration_backfills_provider_only_rows(pool: PgPool) {
    let artist = Uuid::now_v7();
    sqlx::query("INSERT INTO artists (id, name) VALUES ($1, 'Hayley Williams')")
        .bind(artist)
        .execute(&pool)
        .await
        .unwrap();

    let provider_only = Uuid::now_v7();
    let library_only = Uuid::now_v7();
    let mixed = Uuid::now_v7();
    let two_sources = Uuid::now_v7();
    let scoped = Uuid::now_v7();
    let no_sources = Uuid::now_v7();
    for (id, title) in [
        (provider_only, "Provider Only"),
        (library_only, "Library Only"),
        (mixed, "Mixed"),
        (two_sources, "Two Sources"),
        (scoped, "Scoped"),
        (no_sources, "No Sources"),
    ] {
        seed_album(&pool, id, title, artist).await;
    }
    seed_album_source(&pool, provider_only, "youtube_music", "global", "MPREb_one").await;
    seed_album_source(&pool, library_only, "local", "named:/music", "one.flac").await;
    seed_album_source(&pool, mixed, "youtube_music", "global", "MPREb_mixed").await;
    seed_album_source(&pool, mixed, "local", "named:/music", "mixed.flac").await;
    seed_album_source(&pool, two_sources, "youtube_music", "global", "MPREb_old").await;
    seed_album_source(&pool, two_sources, "youtube_music", "global", "MPREb_new").await;
    seed_album_source(
        &pool,
        scoped,
        "youtube_music",
        "named:first",
        "MPREb_scoped",
    )
    .await;
    seed_album_source(
        &pool,
        scoped,
        "youtube_music",
        "named:second",
        "MPREb_scoped",
    )
    .await;
    for (external_id, created_at) in [
        ("MPREb_old", "2026-01-01T00:00:00Z"),
        ("MPREb_new", "2026-02-01T00:00:00Z"),
    ] {
        sqlx::query("UPDATE album_sources SET created_at = $1::TIMESTAMPTZ WHERE external_id = $2")
            .bind(created_at)
            .bind(external_id)
            .execute(&pool)
            .await
            .unwrap();
    }

    sqlx::raw_sql(PRE_0037).execute(&pool).await.unwrap();
    sqlx::raw_sql(PROVIDER_SOURCE_KEY_MIGRATION)
        .execute(&pool)
        .await
        .unwrap();

    assert_eq!(
        provider_source_key(&pool, provider_only).await,
        Some("youtube_music:MPREb_one".to_owned())
    );
    assert_eq!(provider_source_key(&pool, library_only).await, None);
    assert_eq!(provider_source_key(&pool, mixed).await, None);
    assert_eq!(
        provider_source_key(&pool, two_sources).await,
        Some("youtube_music:MPREb_old".to_owned())
    );
    assert_eq!(
        provider_source_key(&pool, scoped).await,
        Some("youtube_music:MPREb_scoped".to_owned())
    );
    assert_eq!(provider_source_key(&pool, no_sources).await, None);

    let repository = PostgresTrackRepository::new(pool.clone());
    let shared = AlbumMetadata::new("Shared Title".to_owned()).unwrap();
    repository
        .refresh_album_facts(&AlbumId::from_uuid(provider_only), &shared)
        .await
        .unwrap();
    repository
        .refresh_album_facts(&AlbumId::from_uuid(two_sources), &shared)
        .await
        .unwrap();

    assert_eq!(
        provider_source_key(&pool, provider_only).await,
        Some("youtube_music:MPREb_one".to_owned())
    );
    assert_eq!(
        provider_source_key(&pool, two_sources).await,
        Some("youtube_music:MPREb_old".to_owned())
    );
}
