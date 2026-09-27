use super::PostgresTrackRepository;
use crate::features::music_library::domain::{
    AlbumMetadata, AlbumTrack, ProviderId, SourceIdentity, SourceScope, StateTarget, TrackMetadata,
};
use crate::features::music_library::library_state::test_support::seed_play;
use crate::features::music_library::storage::contracts::{
    CatalogueWrite, LibraryRead, Retention, StateStore,
};
use sqlx::PgPool;
use uuid::Uuid;

fn source(external_id: &str) -> SourceIdentity {
    SourceIdentity::new(
        ProviderId::new("youtube_music".to_owned()).unwrap(),
        SourceScope::Global,
        external_id.to_owned(),
    )
    .unwrap()
}

fn local_source(external_id: &str) -> SourceIdentity {
    SourceIdentity::new(
        ProviderId::new("local".to_owned()).unwrap(),
        SourceScope::Named("/music".to_owned()),
        external_id.to_owned(),
    )
    .unwrap()
}

fn metadata(title: &str, artist: &str) -> TrackMetadata {
    TrackMetadata::from_names(title.to_owned(), vec![artist.to_owned()], Some(1_000)).unwrap()
}

async fn insert_user(pool: &PgPool, id: Uuid, username: &str) {
    sqlx::query("INSERT INTO users (id, username, display_name) VALUES ($1, $2, $3)")
        .bind(id)
        .bind(username)
        .bind(username)
        .execute(pool)
        .await
        .unwrap();
}

#[sqlx::test]

async fn retention_reports_and_evicts_only_unengaged_provider_rows(pool: PgPool) {
    let repository = PostgresTrackRepository::new(pool.clone());
    let user = Uuid::now_v7();
    insert_user(&pool, user, "flynn").await;

    let owned = repository
        .get_or_create(local_source("owned.flac"), metadata("Owned", "Owner"))
        .await
        .unwrap();
    let orphan = repository
        .get_or_create(source("orphan"), metadata("Orphan", "Ghost"))
        .await
        .unwrap();
    let starred = repository
        .get_or_create(source("starred"), metadata("Starred", "Star"))
        .await
        .unwrap();
    let played = repository
        .get_or_create(source("played"), metadata("Played", "Play"))
        .await
        .unwrap();
    let listed = repository
        .get_or_create(source("listed"), metadata("Listed", "List"))
        .await
        .unwrap();

    let played_at = "2026-09-01T00:00:00Z".parse().unwrap();
    repository
        .set_starred(user, StateTarget::Track(starred.id()), true)
        .await
        .unwrap();
    seed_play(&pool, user, played.id(), played_at).await;

    let playlist_id = Uuid::now_v7();
    sqlx::query("INSERT INTO playlists (id, user_id, name) VALUES ($1, $2, 'mix')")
        .bind(playlist_id)
        .bind(user)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO playlist_entries (playlist_id, position, track_id) VALUES ($1, 0, $2)",
    )
    .bind(playlist_id)
    .bind(listed.id().as_uuid())
    .execute(&pool)
    .await
    .unwrap();

    let album = repository
        .resolve_or_create_album(&AlbumMetadata::new("Lost".to_owned()).unwrap())
        .await
        .unwrap();
    repository
        .set_album_source(album.id(), &source("MPREb_lost"))
        .await
        .unwrap();
    repository
        .set_album_track(album.id(), orphan.id(), &AlbumTrack::unplaced())
        .await
        .unwrap();

    let report = repository.retention_report().await.unwrap();
    assert_eq!(report.evictable_tracks, 1, "only the unengaged track");
    assert_eq!(report.evictable_albums, 1);
    assert_eq!(report.evictable_artists, 1, "only the ghost artist");
    assert_eq!(report.play_held_tracks, 1);
    assert_eq!(report.play_held_albums, 0);
    assert_eq!(report.play_held_artists, 1);
    assert_eq!(report.oldest_played_at, Some(played_at));
    assert_eq!(report.newest_played_at, Some(played_at));

    let evicted = repository.evict_unengaged().await.unwrap();
    assert_eq!(evicted.tracks, 1);
    assert_eq!(evicted.albums, 1);
    assert_eq!(evicted.artists, 1);

    assert!(repository.find_track(orphan.id()).await.unwrap().is_none());
    for kept in [owned.id(), starred.id(), played.id(), listed.id()] {
        assert!(
            repository.find_track(kept).await.unwrap().is_some(),
            "an engaged or owned track survives"
        );
    }

    let after = repository.retention_report().await.unwrap();
    assert_eq!(after.evictable_tracks, 0);
    assert_eq!(after.evictable_albums, 0);
    assert_eq!(after.evictable_artists, 0);
    assert_eq!(after.play_held_tracks, 1);
}
