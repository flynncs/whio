use super::PostgresTrackRepository;
use crate::features::music_library::domain::{
    AlbumListFilter, AlbumMetadata, AlbumOrder, AlbumTrack, ArtistListFilter, ArtistSort,
    CommentPatch, MoveTarget, OccurrenceSelector, Playlist, PlaylistEntryChange,
    PlaylistEntryError, PlaylistEntryId, PlaylistId, PlaylistMetadataPatch, PlaylistOccurrence,
    PlaylistWrite, PlaylistWriteOutcome, ProviderId, SourceIdentity, SourceScope, TrackId,
    TrackMetadata,
};
use crate::features::music_library::storage::contracts::{
    CatalogueWrite, LibraryRead, PlaylistStore, SourceLookup, StateStore,
};
use crate::features::music_library::{
    catalogue::LibraryService,
    playlists::{PlaylistEdit, PlaylistError, PlaylistService},
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

fn service(store: &Arc<PostgresTrackRepository>) -> PlaylistService {
    let library = Arc::new(LibraryService::new(
        Arc::clone(store) as Arc<dyn LibraryRead>,
        Arc::clone(store) as Arc<dyn SourceLookup>,
        Arc::clone(store) as Arc<dyn StateStore>,
    ));

    PlaylistService::new(Arc::clone(store) as Arc<dyn PlaylistStore>, library)
}

async fn track(store: &Arc<PostgresTrackRepository>, name: &str) -> TrackId {
    store
        .get_or_create(local_source(&format!("{name}.flac")), metadata(name))
        .await
        .unwrap()
        .id()
        .clone()
}

async fn occurrences(
    service: &PlaylistService,
    principal: Uuid,
    playlist_id: &PlaylistId,
) -> Vec<PlaylistOccurrence> {
    service
        .entries(principal, playlist_id)
        .await
        .unwrap()
        .expect("the playlist is readable")
}

fn tracks(entries: &[PlaylistOccurrence]) -> Vec<TrackId> {
    entries
        .iter()
        .map(|entry| entry.track_id().clone())
        .collect()
}

fn positions(entries: &[PlaylistOccurrence]) -> Vec<u32> {
    entries.iter().map(PlaylistOccurrence::position).collect()
}

fn entry_ids(entries: &[PlaylistOccurrence]) -> Vec<PlaylistEntryId> {
    entries
        .iter()
        .map(|entry| entry.entry_id().clone())
        .collect()
}

fn edit<'a>(
    name: Option<&'a str>,
    additions: &'a [TrackId],
    removals: Vec<usize>,
) -> PlaylistEdit<'a> {
    PlaylistEdit {
        name,
        comment: None,
        public: None,
        additions,
        removals,
    }
}

fn applied(outcome: PlaylistWriteOutcome) -> Playlist {
    match outcome {
        PlaylistWriteOutcome::Applied(playlist) => playlist,
        outcome => panic!("expected an applied write, got {outcome:?}"),
    }
}

fn plain_write(
    principal: Uuid,
    playlist_id: &PlaylistId,
    entries: PlaylistEntryChange,
) -> PlaylistWrite {
    PlaylistWrite {
        principal,
        playlist_id: playlist_id.clone(),
        expected_revision: None,
        metadata: PlaylistMetadataPatch::default(),
        entries,
    }
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
    assert_eq!(playlist.owner_id(), owner);
    assert_eq!(playlist.owner_username(), "flynn");
    assert_eq!(playlist.revision(), 0);

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
        repository
            .add_playlist_track(owner, playlist.id(), first.id())
            .await
            .unwrap(),
        "a repeated track is a second occurrence"
    );
    assert_eq!(
        repository
            .playlist_occurrences(playlist.id())
            .await
            .unwrap()
            .iter()
            .map(|entry| entry.track_id().clone())
            .collect::<Vec<_>>(),
        vec![second.id().clone(), first.id().clone(), first.id().clone()]
    );

    let listed = repository.list_playlists(owner).await.unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].track_count(), 3);

    let updated = applied(
        repository
            .write_playlist(PlaylistWrite {
                principal: owner,
                playlist_id: playlist.id().clone(),
                expected_revision: None,
                metadata: PlaylistMetadataPatch {
                    name: Some("Road trip".to_owned()),
                    comment: CommentPatch::Set(Some("top down".to_owned())),
                    public: Some(true),
                },
                entries: PlaylistEntryChange::Unchanged,
            })
            .await
            .unwrap(),
    );
    assert!(updated.is_public());
    assert_eq!(updated.track_count(), 3);
    assert_eq!(updated.revision(), 4);

    let removed = applied(
        repository
            .write_playlist(plain_write(
                owner,
                playlist.id(),
                PlaylistEntryChange::Apply {
                    removals: vec![OccurrenceSelector::Entry(
                        repository
                            .playlist_occurrences(playlist.id())
                            .await
                            .unwrap()[0]
                            .entry_id()
                            .clone(),
                    )],
                    additions: Vec::new(),
                },
            ))
            .await
            .unwrap(),
    );
    assert_eq!(removed.track_count(), 2);
    assert_eq!(
        repository
            .playlist_occurrences(playlist.id())
            .await
            .unwrap()
            .iter()
            .map(|entry| entry.track_id().clone())
            .collect::<Vec<_>>(),
        vec![first.id().clone(), first.id().clone()]
    );

    assert!(
        repository
            .find_playlist(playlist.id())
            .await
            .unwrap()
            .is_some(),
        "a public playlist is readable by another principal"
    );
    assert!(
        !repository
            .add_playlist_track(stranger, playlist.id(), second.id())
            .await
            .unwrap(),
        "another principal cannot add to it"
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
            .find_playlist(playlist.id())
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        repository
            .playlist_occurrences(playlist.id())
            .await
            .unwrap()
            .is_empty(),
        "deleting the playlist removes its entries"
    );

    let unknown: PlaylistId = "01a07482-0f2d-7511-a4da-ac170d6f97bd".parse().unwrap();
    assert!(repository.find_playlist(&unknown).await.unwrap().is_none());
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
            .list_albums(0, 50, AlbumOrder::Name, AlbumListFilter::new(), Some(owner))
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
        .list_albums(0, 50, AlbumOrder::Name, AlbumListFilter::new(), Some(owner))
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

    let artists = repository
        .list_artists(
            0,
            50,
            ArtistSort::Name,
            &ArtistListFilter::default(),
            Some(owner),
        )
        .await
        .unwrap();
    assert_eq!(artists.len(), 1);
    assert_eq!(artists[0].artist().name(), "Artist");
    assert_eq!(
        repository
            .count_artists(Some(owner), &ArtistListFilter::default())
            .await
            .unwrap(),
        1,
        "the credited artist is browsable too"
    );

    assert!(
        repository
            .list_albums(
                0,
                50,
                AlbumOrder::Name,
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
async fn repeated_tracks_are_distinct_entries(pool: PgPool) {
    let store = Arc::new(PostgresTrackRepository::new(pool.clone()));
    let owner = Uuid::now_v7();
    insert_user(&pool, owner, "repeat-owner").await;
    let service = service(&store);
    let repeated = track(&store, "Repeated").await;
    let playlist = service.create(owner, "repeats", None).await.unwrap();

    assert!(
        service
            .add_track(owner, playlist.id(), &repeated)
            .await
            .unwrap()
    );
    assert!(
        service
            .add_track(owner, playlist.id(), &repeated)
            .await
            .unwrap()
    );

    let entries = occurrences(&service, owner, playlist.id()).await;
    assert_eq!(entries.len(), 2, "the same track twice is two entries");
    assert_ne!(entries[0].entry_id(), entries[1].entry_id());
    assert_eq!(entries[0].track_id(), &repeated);
    assert_eq!(entries[1].track_id(), &repeated);
    assert_eq!(positions(&entries), vec![0, 1]);
    assert_eq!(playlist.revision(), 0);

    let detail = service
        .detail(owner, playlist.id())
        .await
        .unwrap()
        .expect("the owner reads the playlist");
    assert_eq!(
        detail.entries.len(),
        2,
        "both occurrences appear in the read"
    );
    assert!(detail.editable);

    assert!(
        service
            .edit(owner, playlist.id(), edit(None, &[], vec![0]))
            .await
            .unwrap()
    );
    let remaining = occurrences(&service, owner, playlist.id()).await;
    assert_eq!(
        remaining.len(),
        1,
        "removing one occurrence leaves the other"
    );
    assert_eq!(remaining[0].entry_id(), entries[1].entry_id());
    assert_eq!(remaining[0].track_id(), &repeated);
}

#[sqlx::test]
async fn existing_id_create_replaces_and_preserves_metadata(pool: PgPool) {
    let store = Arc::new(PostgresTrackRepository::new(pool.clone()));
    let owner = Uuid::now_v7();
    insert_user(&pool, owner, "create-owner").await;
    let service = service(&store);
    let first = track(&store, "First").await;
    let second = track(&store, "Second").await;

    let playlist = service
        .create(owner, "named", Some("keep me"))
        .await
        .unwrap();
    let shared = service
        .update(owner, playlist.id(), "named", Some("keep me"), true)
        .await
        .unwrap()
        .expect("the owner updates the playlist");
    assert!(shared.is_public());
    assert!(
        service
            .add_track(owner, playlist.id(), &first)
            .await
            .unwrap()
    );
    assert!(
        service
            .add_track(owner, playlist.id(), &first)
            .await
            .unwrap()
    );

    let detail = service
        .create_or_append(
            owner,
            Some(playlist.id().clone()),
            "renamed",
            std::slice::from_ref(&second),
        )
        .await
        .unwrap()
        .expect("the existing playlist is replaced");

    assert_eq!(detail.playlist.name(), "renamed");
    assert_eq!(detail.playlist.comment(), Some("keep me"));
    assert!(detail.playlist.is_public());
    assert_eq!(
        tracks(&occurrences(&service, owner, playlist.id()).await),
        vec![second.clone()],
        "the old entries are replaced rather than appended to"
    );
    assert_eq!(detail.entries.len(), 1);
}

#[sqlx::test]
async fn failed_add_rolls_the_edit_back(pool: PgPool) {
    let store = Arc::new(PostgresTrackRepository::new(pool.clone()));
    let owner = Uuid::now_v7();
    insert_user(&pool, owner, "atomic-owner").await;
    let service = service(&store);
    let first = track(&store, "First").await;
    let second = track(&store, "Second").await;

    let playlist = service
        .create(owner, "atomic", Some("original"))
        .await
        .unwrap();
    assert!(
        service
            .add_track(owner, playlist.id(), &first)
            .await
            .unwrap()
    );
    assert!(
        service
            .add_track(owner, playlist.id(), &second)
            .await
            .unwrap()
    );

    let before = service.detail(owner, playlist.id()).await.unwrap().unwrap();
    let missing = TrackId::new();

    let failed = service
        .edit(
            owner,
            playlist.id(),
            PlaylistEdit {
                name: Some("changed"),
                comment: Some("changed"),
                public: Some(true),
                additions: std::slice::from_ref(&missing),
                removals: vec![0],
            },
        )
        .await;

    match failed {
        Err(PlaylistError::Entry(PlaylistEntryError::UnknownTrack { track_id })) => {
            assert_eq!(track_id, missing);
        }
        other => panic!("expected the failed addition to be returned, got {other:?}"),
    }

    let after = service.detail(owner, playlist.id()).await.unwrap().unwrap();
    assert_eq!(
        after.playlist, before.playlist,
        "the metadata patch and the revision roll back with the failed addition"
    );
    assert_eq!(after.playlist.name(), "atomic");
    assert_eq!(after.playlist.comment(), Some("original"));
    assert!(!after.playlist.is_public());
    assert_eq!(
        after.entries.len(),
        before.entries.len(),
        "the removals roll back with the failed addition"
    );
    assert_eq!(
        positions(&occurrences(&service, owner, playlist.id()).await),
        vec![0, 1]
    );
}

#[sqlx::test]
async fn remove_before_add_cannot_remove_a_new_occurrence(pool: PgPool) {
    let store = Arc::new(PostgresTrackRepository::new(pool.clone()));
    let owner = Uuid::now_v7();
    insert_user(&pool, owner, "order-owner").await;
    let service = service(&store);
    let first = track(&store, "First").await;
    let second = track(&store, "Second").await;

    let playlist = service.create(owner, "order", None).await.unwrap();
    assert!(
        service
            .add_track(owner, playlist.id(), &first)
            .await
            .unwrap()
    );
    assert!(
        service
            .add_track(owner, playlist.id(), &second)
            .await
            .unwrap()
    );
    let before = occurrences(&service, owner, playlist.id()).await;

    assert!(
        service
            .edit(
                owner,
                playlist.id(),
                edit(None, std::slice::from_ref(&first), vec![0])
            )
            .await
            .unwrap()
    );

    let after = occurrences(&service, owner, playlist.id()).await;
    assert_eq!(
        tracks(&after),
        vec![second.clone(), first.clone()],
        "the removed occurrence is gone and the added one is present"
    );
    assert_eq!(positions(&after), vec![0, 1]);
    assert!(
        !entry_ids(&after).contains(&before[0].entry_id().clone()),
        "the removal named the original occurrence, not the added one"
    );
    assert_ne!(after[1].entry_id(), before[0].entry_id());

    assert!(
        service
            .add_track(owner, playlist.id(), &first)
            .await
            .unwrap()
    );
    let duplicated = occurrences(&service, owner, playlist.id()).await;
    assert_eq!(positions(&duplicated), vec![0, 1, 2]);
    assert_eq!(
        tracks(&duplicated),
        vec![second.clone(), first.clone(), first.clone()]
    );

    assert!(
        service
            .edit(owner, playlist.id(), edit(None, &[], vec![1]))
            .await
            .unwrap()
    );
    let trimmed = occurrences(&service, owner, playlist.id()).await;
    assert_eq!(
        tracks(&trimmed),
        vec![second.clone(), first.clone()],
        "removing one occurrence of a repeated track leaves the other"
    );
    assert_eq!(trimmed[1].entry_id(), duplicated[2].entry_id());
}

#[sqlx::test]
async fn move_reorders_contiguously_and_zero_based(pool: PgPool) {
    let store = Arc::new(PostgresTrackRepository::new(pool.clone()));
    let owner = Uuid::now_v7();
    insert_user(&pool, owner, "move-owner").await;
    let service = service(&store);
    let mut ids = Vec::new();
    for name in ["First", "Second", "Third", "Fourth"] {
        let id = track(&store, name).await;
        ids.push(id);
    }

    let playlist = service.create(owner, "moves", None).await.unwrap();
    for track_id in &ids {
        assert!(
            service
                .add_track(owner, playlist.id(), track_id)
                .await
                .unwrap()
        );
    }
    let started = occurrences(&service, owner, playlist.id()).await;

    assert!(
        service
            .move_entry(
                owner,
                playlist.id(),
                OccurrenceSelector::Index(0),
                MoveTarget::End,
                None,
            )
            .await
            .unwrap()
    );
    let moved = occurrences(&service, owner, playlist.id()).await;
    assert_eq!(
        tracks(&moved),
        vec![
            ids[1].clone(),
            ids[2].clone(),
            ids[3].clone(),
            ids[0].clone()
        ],
        "the first entry moves to the end"
    );
    assert_eq!(positions(&moved), vec![0, 1, 2, 3]);
    assert_eq!(
        moved[3].entry_id(),
        started[0].entry_id(),
        "the moved entry keeps its identity"
    );

    assert!(
        service
            .move_entry(
                owner,
                playlist.id(),
                OccurrenceSelector::Index(3),
                MoveTarget::Start,
                None,
            )
            .await
            .unwrap()
    );
    let fronted = occurrences(&service, owner, playlist.id()).await;
    assert_eq!(
        tracks(&fronted),
        vec![
            ids[0].clone(),
            ids[1].clone(),
            ids[2].clone(),
            ids[3].clone()
        ],
        "the last entry moves to the front"
    );
    assert_eq!(positions(&fronted), vec![0, 1, 2, 3]);

    assert!(
        service
            .move_entry(
                owner,
                playlist.id(),
                OccurrenceSelector::Index(1),
                MoveTarget::Before(3),
                None,
            )
            .await
            .unwrap()
    );
    let lowered = occurrences(&service, owner, playlist.id()).await;
    assert_eq!(
        tracks(&lowered),
        vec![
            ids[0].clone(),
            ids[2].clone(),
            ids[1].clone(),
            ids[3].clone()
        ],
        "a middle entry moves towards the end against the pre-move order"
    );
    assert_eq!(positions(&lowered), vec![0, 1, 2, 3]);

    assert!(
        service
            .move_entry(
                owner,
                playlist.id(),
                OccurrenceSelector::Index(2),
                MoveTarget::Before(0),
                None,
            )
            .await
            .unwrap()
    );
    let raised = occurrences(&service, owner, playlist.id()).await;
    assert_eq!(
        tracks(&raised),
        vec![
            ids[1].clone(),
            ids[0].clone(),
            ids[2].clone(),
            ids[3].clone()
        ],
        "a middle entry moves towards the front against the pre-move order"
    );
    assert_eq!(positions(&raised), vec![0, 1, 2, 3]);
    assert_eq!(
        raised[0].entry_id(),
        lowered[2].entry_id(),
        "the moved entry keeps its identity through both directions"
    );

    let beyond = service
        .move_entry(
            owner,
            playlist.id(),
            OccurrenceSelector::Index(9),
            MoveTarget::End,
            None,
        )
        .await;
    match beyond {
        Err(PlaylistError::Entry(PlaylistEntryError::IndexOutOfRange { index })) => {
            assert_eq!(index, 9);
        }
        other => panic!("expected an out-of-range move to be reported, got {other:?}"),
    }
    assert_eq!(
        occurrences(&service, owner, playlist.id()).await,
        raised,
        "a rejected move changes nothing"
    );
}

#[sqlx::test]
async fn revision_conflict_is_reported(pool: PgPool) {
    let store = Arc::new(PostgresTrackRepository::new(pool.clone()));
    let owner = Uuid::now_v7();
    insert_user(&pool, owner, "revision-owner").await;
    let service = service(&store);
    let first = track(&store, "First").await;
    let second = track(&store, "Second").await;

    let playlist = service.create(owner, "revisions", None).await.unwrap();
    let read = service.detail(owner, playlist.id()).await.unwrap().unwrap();
    let stale = read.playlist.revision();
    assert_eq!(stale, 0);

    assert!(
        service
            .edit_at_revision(
                owner,
                playlist.id(),
                Some(stale),
                edit(Some("renamed"), std::slice::from_ref(&first), Vec::new()),
            )
            .await
            .unwrap()
    );
    let after = service.detail(owner, playlist.id()).await.unwrap().unwrap();
    let current = after.playlist.revision();
    assert_eq!(current, stale + 1);
    assert_eq!(after.playlist.name(), "renamed");

    let failed = service
        .edit_at_revision(
            owner,
            playlist.id(),
            Some(stale),
            edit(Some("stale"), std::slice::from_ref(&second), Vec::new()),
        )
        .await;
    match failed {
        Err(PlaylistError::Conflict { current_revision }) => assert_eq!(current_revision, current),
        other => panic!("expected the stale revision to conflict, got {other:?}"),
    }

    let unchanged = service.detail(owner, playlist.id()).await.unwrap().unwrap();
    assert_eq!(unchanged.playlist, after.playlist);
    assert_eq!(
        unchanged.entries.len(),
        1,
        "the conflicting write changed nothing"
    );

    assert!(
        service
            .edit(
                owner,
                playlist.id(),
                edit(Some("last-write-wins"), &[], Vec::new())
            )
            .await
            .unwrap()
    );
    let last = service.detail(owner, playlist.id()).await.unwrap().unwrap();
    assert_eq!(last.playlist.name(), "last-write-wins");
    assert_eq!(last.playlist.revision(), current + 1);
}

#[sqlx::test]
async fn public_is_readable_and_read_only_for_a_non_owner(pool: PgPool) {
    let store = Arc::new(PostgresTrackRepository::new(pool.clone()));
    let owner = Uuid::now_v7();
    let stranger = Uuid::now_v7();
    insert_user(&pool, owner, "public-owner").await;
    insert_user(&pool, stranger, "public-stranger").await;
    let service = service(&store);
    let first = track(&store, "First").await;
    let second = track(&store, "Second").await;

    let private = service.create(owner, "private", None).await.unwrap();
    assert!(
        service
            .add_track(owner, private.id(), &first)
            .await
            .unwrap()
    );
    let shared = service.create(owner, "shared", None).await.unwrap();
    assert!(service.add_track(owner, shared.id(), &first).await.unwrap());
    service
        .update(owner, shared.id(), "shared", None, true)
        .await
        .unwrap()
        .unwrap();

    assert!(
        service
            .detail(stranger, private.id())
            .await
            .unwrap()
            .is_none(),
        "a private playlist is invisible to a non-owner"
    );
    assert!(
        service
            .entries(stranger, private.id())
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        service.list(stranger).await.unwrap().len(),
        1,
        "a non-owner lists only the public playlist"
    );

    let readable = service
        .detail(stranger, shared.id())
        .await
        .unwrap()
        .expect("an authenticated non-owner reads a public playlist");
    assert_eq!(readable.entries.len(), 1);
    assert!(!readable.editable);
    assert_eq!(occurrences(&service, stranger, shared.id()).await.len(), 1);
    assert_eq!(
        service.list(stranger).await.unwrap()[0].name(),
        "shared",
        "the public playlist is listed for the non-owner"
    );

    assert!(
        !service
            .edit(
                stranger,
                shared.id(),
                edit(Some("hijack"), std::slice::from_ref(&second), vec![0]),
            )
            .await
            .unwrap(),
        "a public playlist is read-only for a non-owner"
    );
    assert!(
        !service
            .add_track(stranger, shared.id(), &second)
            .await
            .unwrap()
    );
    assert!(
        !service
            .move_entry(
                stranger,
                shared.id(),
                OccurrenceSelector::Index(0),
                MoveTarget::End,
                None,
            )
            .await
            .unwrap()
    );
    assert!(
        !service
            .update(stranger, shared.id(), "hijack", None, true)
            .await
            .unwrap()
            .is_some()
    );
    assert!(!service.delete(stranger, shared.id()).await.unwrap());

    let after = service.detail(owner, shared.id()).await.unwrap().unwrap();
    assert_eq!(after.playlist.name(), "shared");
    assert_eq!(after.entries.len(), 1);
    assert_eq!(
        tracks(&occurrences(&service, owner, shared.id()).await),
        vec![first.clone()]
    );

    assert!(
        service
            .edit(owner, shared.id(), edit(Some("mine"), &[], Vec::new()))
            .await
            .unwrap(),
        "the owner can still edit"
    );
}

#[sqlx::test]
async fn owner_is_reported_and_editability_is_explicit(pool: PgPool) {
    let store = Arc::new(PostgresTrackRepository::new(pool.clone()));
    let owner = Uuid::now_v7();
    let stranger = Uuid::now_v7();
    insert_user(&pool, owner, "real-owner").await;
    insert_user(&pool, stranger, "reader").await;
    let service = service(&store);
    let first = track(&store, "First").await;

    let playlist = service.create(owner, "attributed", None).await.unwrap();
    assert!(
        service
            .add_track(owner, playlist.id(), &first)
            .await
            .unwrap()
    );
    service
        .update(owner, playlist.id(), "attributed", None, true)
        .await
        .unwrap()
        .unwrap();

    let read = service
        .detail(stranger, playlist.id())
        .await
        .unwrap()
        .expect("the reader sees the public playlist");
    assert_eq!(
        read.playlist.owner_id(),
        owner,
        "the read reports the owner, not the caller"
    );
    assert_eq!(read.playlist.owner_username(), "real-owner");
    assert!(!read.editable, "editability is explicit for the non-owner");

    assert!(
        !service
            .edit(
                stranger,
                playlist.id(),
                edit(Some("hijack"), &[], Vec::new())
            )
            .await
            .unwrap()
    );

    let owned = service.detail(owner, playlist.id()).await.unwrap().unwrap();
    assert_eq!(owned.playlist.owner_id(), owner);
    assert_eq!(owned.playlist.owner_username(), "real-owner");
    assert!(owned.editable);

    let created = service
        .create(stranger, "reader-owned", None)
        .await
        .unwrap();
    assert_eq!(
        created.owner_id(),
        stranger,
        "create stamps the caller as the owner"
    );
    assert_eq!(created.owner_username(), "reader");
    assert!(
        service
            .edit(
                owner,
                playlist.id(),
                edit(Some("attributed"), &[], Vec::new())
            )
            .await
            .unwrap()
    );
}

#[sqlx::test]
async fn concurrent_edits_serialise(pool: PgPool) {
    let store = Arc::new(PostgresTrackRepository::new(pool.clone()));
    let owner = Uuid::now_v7();
    insert_user(&pool, owner, "concurrent-owner").await;
    let service = service(&store);
    let first = track(&store, "First").await;
    let second = track(&store, "Second").await;

    let playlist = service.create(owner, "concurrent", None).await.unwrap();
    let initial = service
        .detail(owner, playlist.id())
        .await
        .unwrap()
        .unwrap()
        .playlist
        .revision();

    let left_additions = [first.clone()];
    let right_additions = [second.clone()];
    let left = service.edit(
        owner,
        playlist.id(),
        edit(None, &left_additions, Vec::new()),
    );
    let right = service.edit(
        owner,
        playlist.id(),
        edit(None, &right_additions, Vec::new()),
    );
    let (left, right) = tokio::join!(left, right);
    assert!(left.unwrap());
    assert!(right.unwrap());

    let entries = occurrences(&service, owner, playlist.id()).await;
    assert_eq!(positions(&entries), vec![0, 1], "positions stay unique");
    let mut added = tracks(&entries);
    added.sort_by_key(TrackId::to_string);
    let mut expected = vec![first.clone(), second.clone()];
    expected.sort_by_key(TrackId::to_string);
    assert_eq!(added, expected, "both edits are present");
    assert_eq!(
        service
            .detail(owner, playlist.id())
            .await
            .unwrap()
            .unwrap()
            .playlist
            .revision(),
        initial + 2,
        "the revision reflects both edits"
    );

    let third = track(&store, "Third").await;
    let fourth = track(&store, "Fourth").await;
    let expected = initial + 2;
    let left_additions = [third];
    let right_additions = [fourth];
    let left = service.edit_at_revision(
        owner,
        playlist.id(),
        Some(expected),
        edit(None, &left_additions, Vec::new()),
    );
    let right = service.edit_at_revision(
        owner,
        playlist.id(),
        Some(expected),
        edit(None, &right_additions, Vec::new()),
    );
    let (left, right) = tokio::join!(left, right);

    match (left, right) {
        (Ok(true), Err(PlaylistError::Conflict { current_revision }))
        | (Err(PlaylistError::Conflict { current_revision }), Ok(true)) => {
            assert_eq!(current_revision, expected + 1);
        }
        other => panic!("exactly one conditional edit reports the conflict, got {other:?}"),
    }

    let serialised = service.detail(owner, playlist.id()).await.unwrap().unwrap();
    assert_eq!(serialised.playlist.revision(), expected + 1);
    let entries = occurrences(&service, owner, playlist.id()).await;
    assert_eq!(entries.len(), 3, "only the applied conditional edit landed");
    assert_eq!(positions(&entries), vec![0, 1, 2]);
}
