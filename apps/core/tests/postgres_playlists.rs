#![cfg(feature = "postgres-integration")]

use sqlx::PgPool;
use uuid::Uuid;
use whio_core::tracks::{
    CatalogueWrite, PlaylistId, PlaylistStore, PostgresTrackRepository, ProviderId, SourceIdentity,
    SourceScope, TrackMetadata,
};

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
#[ignore = "requires PostgreSQL"]
async fn playlists_round_trip_and_stay_owned(pool: PgPool) {
    let repository = PostgresTrackRepository::new(pool.clone());
    let owner = Uuid::now_v7();
    let stranger = Uuid::now_v7();
    insert_user(&pool, owner, "flynn").await;
    insert_user(&pool, stranger, "other").await;

    let first = repository
        .get_or_create(local_source("first.flac"), metadata("First"))
        .await
        .unwrap();
    let second = repository
        .get_or_create(local_source("second.flac"), metadata("Second"))
        .await
        .unwrap();

    let playlist = repository
        .create_playlist(owner, "  Road trip  ", Some("windows down"))
        .await
        .unwrap();
    assert_eq!(playlist.name(), "Road trip");
    assert_eq!(playlist.comment(), Some("windows down"));
    assert_eq!(playlist.track_count(), 0);

    assert!(
        repository
            .add_playlist_track(owner, playlist.id(), second.id())
            .await
            .unwrap()
    );
    assert!(
        repository
            .add_playlist_track(owner, playlist.id(), first.id())
            .await
            .unwrap()
    );
    assert!(
        !repository
            .add_playlist_track(owner, playlist.id(), first.id())
            .await
            .unwrap(),
        "adding the same track twice is a no-op"
    );
    assert_eq!(
        repository.playlist_tracks(playlist.id()).await.unwrap(),
        vec![second.id().clone(), first.id().clone()]
    );

    let listed = repository.list_playlists(owner).await.unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].track_count(), 2);

    let updated = repository
        .update_playlist(owner, playlist.id(), "Road trip", Some("top down"), true)
        .await
        .unwrap()
        .expect("the owner updates the playlist");
    assert!(updated.is_public());
    assert_eq!(updated.track_count(), 2);

    assert!(
        repository
            .remove_playlist_track(owner, playlist.id(), second.id())
            .await
            .unwrap()
    );
    assert_eq!(
        repository.playlist_tracks(playlist.id()).await.unwrap(),
        vec![first.id().clone()]
    );

    assert!(
        repository
            .find_playlist(stranger, playlist.id())
            .await
            .unwrap()
            .is_none(),
        "a playlist is invisible to another user"
    );
    assert!(
        !repository
            .add_playlist_track(stranger, playlist.id(), second.id())
            .await
            .unwrap()
    );
    assert!(
        !repository
            .delete_playlist(stranger, playlist.id())
            .await
            .unwrap()
    );

    assert!(
        repository
            .delete_playlist(owner, playlist.id())
            .await
            .unwrap()
    );
    assert!(
        repository
            .find_playlist(owner, playlist.id())
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        repository
            .playlist_tracks(playlist.id())
            .await
            .unwrap()
            .is_empty(),
        "deleting the playlist removes its entries"
    );

    let unknown: PlaylistId = "01a07482-0f2d-7511-a4da-ac170d6f97bd".parse().unwrap();
    assert!(
        repository
            .find_playlist(owner, &unknown)
            .await
            .unwrap()
            .is_none()
    );
}
