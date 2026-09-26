use super::PostgresTrackRepository;
use crate::features::music_library::domain::{
    AlbumListFilter, AlbumMetadata, AlbumOrder, AlbumTrack, PlaylistId, ProviderId, SourceIdentity,
    SourceScope, TrackId, TrackMetadata,
};
use crate::features::music_library::storage::contracts::{
    CatalogueWrite, LibraryRead, PlaylistStore, SourceLookup, StateStore,
};
use crate::features::music_library::{
    catalogue::LibraryService,
    playlists::{PlaylistEdit, PlaylistService},
};
use sqlx::PgPool;
use std::sync::Arc;
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

#[sqlx::test]

async fn a_playlisted_provider_track_becomes_browsable_for_its_owner(pool: PgPool) {
    let repository = PostgresTrackRepository::new(pool.clone());
    let owner = Uuid::now_v7();
    let stranger = Uuid::now_v7();
    insert_user(&pool, owner, "owner").await;
    insert_user(&pool, stranger, "stranger").await;

    let track = repository
        .get_or_create(source("vid-1"), metadata("Song"))
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
        .set_album_track(album.id(), track.id(), &AlbumTrack::unplaced())
        .await
        .unwrap();

    assert!(
        repository
            .list_albums(
                0,
                50,
                AlbumOrder::Title,
                AlbumListFilter::new(),
                Some(owner)
            )
            .await
            .unwrap()
            .is_empty(),
        "an unengaged provider album is not in browse"
    );
    assert_eq!(
        repository
            .count_albums(Some(owner), AlbumListFilter::new())
            .await
            .unwrap(),
        0
    );

    let playlist = repository
        .create_playlist(owner, "mix", None)
        .await
        .unwrap();
    repository
        .add_playlist_track(owner, playlist.id(), track.id())
        .await
        .unwrap();

    let albums = repository
        .list_albums(
            0,
            50,
            AlbumOrder::Title,
            AlbumListFilter::new(),
            Some(owner),
        )
        .await
        .unwrap();
    assert_eq!(albums.len(), 1, "the playlist entry engages the album");
    assert_eq!(albums[0].album().id(), album.id());
    assert_eq!(
        repository
            .count_albums(Some(owner), AlbumListFilter::new())
            .await
            .unwrap(),
        1,
        "the count agrees with the list"
    );

    let artists = repository.list_artists(0, 50, Some(owner)).await.unwrap();
    assert_eq!(artists.len(), 1);
    assert_eq!(artists[0].artist().name(), "Artist");
    assert_eq!(
        repository.count_artists(Some(owner)).await.unwrap(),
        1,
        "the credited artist is browsable too"
    );

    assert!(
        repository
            .list_albums(
                0,
                50,
                AlbumOrder::Title,
                AlbumListFilter::new(),
                Some(stranger)
            )
            .await
            .unwrap()
            .is_empty(),
        "another user's browse is unaffected"
    );
    assert_eq!(
        repository
            .count_albums(Some(stranger), AlbumListFilter::new())
            .await
            .unwrap(),
        0
    );
}

#[sqlx::test]
async fn playlist_edit_preserves_partial_success_and_original_index_order(pool: PgPool) {
    let store = Arc::new(PostgresTrackRepository::new(pool.clone()));
    let owner = Uuid::now_v7();
    insert_user(&pool, owner, "playlist-editor").await;
    let mut tracks = Vec::new();
    for name in ["First", "Second", "Third", "Fourth"] {
        let track = store
            .get_or_create(local_source(&format!("{name}.flac")), metadata(name))
            .await
            .unwrap();
        tracks.push(track.id().clone());
    }
    let library = Arc::new(LibraryService::new(
        Arc::clone(&store) as Arc<dyn LibraryRead>,
        Arc::clone(&store) as Arc<dyn SourceLookup>,
        Arc::clone(&store) as Arc<dyn StateStore>,
    ));
    let service = PlaylistService::new(Arc::clone(&store) as Arc<dyn PlaylistStore>, library);
    let playlist = service.create(owner, "Original", None).await.unwrap();
    for track in &tracks {
        assert!(
            service
                .add_track(owner, playlist.id(), track)
                .await
                .unwrap()
        );
    }
    let missing = TrackId::new();
    assert!(
        service
            .edit(
                owner,
                playlist.id(),
                PlaylistEdit {
                    name: Some("Edited"),
                    comment: None,
                    public: Some(true),
                    additions: &[missing],
                    removals: vec![0, 2, 99],
                },
            )
            .await
            .unwrap()
    );
    let detail = service.detail(owner, playlist.id()).await.unwrap().unwrap();
    assert_eq!(detail.playlist.name(), "Edited");
    assert!(detail.playlist.is_public());
    assert_eq!(
        store.playlist_tracks(playlist.id()).await.unwrap(),
        vec![tracks[1].clone(), tracks[3].clone()],
        "removals refer to the original positions, regardless of invalid additions or indices",
    );
}
