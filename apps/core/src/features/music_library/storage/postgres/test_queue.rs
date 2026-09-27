use super::PostgresTrackRepository;
use crate::features::music_library::{
    domain::{
        AlbumListFilter, AlbumMetadata, AlbumOrder, AlbumTrack, ArtistListFilter, ArtistSort,
        ProviderId, RestoreQueueSnapshot, RestoreQueueValidationError, SourceIdentity, SourceScope,
        TrackId, TrackListFilter, TrackMetadata, TrackOrder,
    },
    library_state::test_support::seed_play,
    queue::{MAX_RESTORE_QUEUE_ENTRIES, RestoreQueueError, RestoreQueueSave, RestoreQueueService},
    storage::contracts::{CatalogueWrite, LibraryRead, RestoreQueueStore, Retention},
};
use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;
use std::sync::Arc;
use uuid::Uuid;

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

fn metadata_by(title: &str, artist: &str) -> TrackMetadata {
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

async fn track(store: &Arc<PostgresTrackRepository>, name: &str) -> TrackId {
    store
        .get_or_create(local_source(&format!("{name}.flac")), metadata(name))
        .await
        .unwrap()
        .id()
        .clone()
}

fn remote_source(external_id: &str) -> SourceIdentity {
    SourceIdentity::new(
        ProviderId::new("youtube_music".to_owned()).unwrap(),
        SourceScope::Global,
        external_id.to_owned(),
    )
    .unwrap()
}

async fn remote_track(
    store: &Arc<PostgresTrackRepository>,
    external_id: &str,
    title: &str,
    artist: &str,
) -> TrackId {
    store
        .get_or_create(remote_source(external_id), metadata_by(title, artist))
        .await
        .unwrap()
        .id()
        .clone()
}

async fn engaged_titles(store: &Arc<PostgresTrackRepository>, user: Uuid) -> Vec<String> {
    store
        .page_tracks(
            0,
            50,
            TrackOrder::default(),
            &TrackListFilter::default(),
            Some(user),
        )
        .await
        .unwrap()
        .items()
        .iter()
        .map(|view| view.track().title().to_owned())
        .collect()
}

async fn browsable_artists(store: &Arc<PostgresTrackRepository>, user: Uuid) -> Vec<String> {
    store
        .list_artists(
            0,
            50,
            ArtistSort::default(),
            &ArtistListFilter::default(),
            Some(user),
        )
        .await
        .unwrap()
        .iter()
        .map(|summary| summary.artist().name().to_owned())
        .collect()
}

async fn browsable_albums(store: &Arc<PostgresTrackRepository>, user: Uuid) -> Vec<String> {
    store
        .list_albums(
            0,
            50,
            AlbumOrder::Name,
            AlbumListFilter::default(),
            Some(user),
        )
        .await
        .unwrap()
        .iter()
        .map(|summary| summary.album().title().to_owned())
        .collect()
}

fn service(store: &Arc<PostgresTrackRepository>) -> RestoreQueueService {
    RestoreQueueService::new(Arc::clone(store) as Arc<dyn RestoreQueueStore>)
}

fn save(entries: Vec<TrackId>, current_index: Option<u32>, position_ms: u64) -> RestoreQueueSave {
    RestoreQueueSave {
        entries,
        current_index,
        position_ms,
    }
}

fn tracks(snapshot: &RestoreQueueSnapshot) -> Vec<TrackId> {
    snapshot
        .entries()
        .iter()
        .map(|entry| entry.track_id().clone())
        .collect()
}

fn positions(snapshot: &RestoreQueueSnapshot) -> Vec<u32> {
    snapshot
        .entries()
        .iter()
        .map(|entry| entry.position())
        .collect()
}

fn entry_ids(snapshot: &RestoreQueueSnapshot) -> Vec<Uuid> {
    snapshot
        .entries()
        .iter()
        .map(|entry| *entry.entry_id().as_uuid())
        .collect()
}

async fn stored_entry_count(pool: &PgPool, user: Uuid) -> i64 {
    sqlx::query_scalar::<_, i64>(
        "SELECT count(*) FROM restore_queue_entries WHERE queue_user_id = $1",
    )
    .bind(user)
    .fetch_one(pool)
    .await
    .unwrap()
}

#[sqlx::test]
async fn a_saved_snapshot_round_trips(pool: PgPool) {
    let store = Arc::new(PostgresTrackRepository::new(pool.clone()));
    let user = Uuid::now_v7();
    insert_user(&pool, user, "flynn").await;

    let first = track(&store, "first").await;
    let second = track(&store, "second").await;
    let third = track(&store, "third").await;
    let service = service(&store);

    let saved = service
        .save(
            user,
            save(
                vec![first.clone(), second.clone(), first.clone(), third.clone()],
                Some(2),
                4_200,
            ),
        )
        .await
        .unwrap();

    assert_eq!(saved.user_id(), user);
    assert_eq!(saved.changed_by(), user);
    assert_eq!(saved.changed_by_username(), "flynn");
    assert_eq!(saved.current_index(), Some(2));
    assert_eq!(saved.position_ms(), 4_200);
    assert_eq!(saved.revision(), 1);
    assert_eq!(
        tracks(&saved),
        vec![first.clone(), second.clone(), first.clone(), third.clone()]
    );
    assert_eq!(positions(&saved), vec![0, 1, 2, 3]);

    let mut identities = entry_ids(&saved);
    identities.sort();
    identities.dedup();
    assert_eq!(identities.len(), 4, "a repeated track keeps its own entry");

    let read = service
        .read(user)
        .await
        .unwrap()
        .expect("the snapshot is stored");
    assert_eq!(read, saved);
}

#[sqlx::test]
async fn an_absent_snapshot_is_not_an_empty_one(pool: PgPool) {
    let store = Arc::new(PostgresTrackRepository::new(pool.clone()));
    let user = Uuid::now_v7();
    insert_user(&pool, user, "flynn").await;
    let service = service(&store);

    assert!(service.read(user).await.unwrap().is_none());

    let saved = service.save(user, save(Vec::new(), None, 0)).await.unwrap();
    assert!(saved.is_empty());
    assert_eq!(saved.current_index(), None);
    assert_eq!(saved.revision(), 1);

    let read = service
        .read(user)
        .await
        .unwrap()
        .expect("an empty snapshot is still a snapshot");
    assert!(read.is_empty());
    assert_eq!(read, saved);

    assert!(service.clear(user).await.unwrap());
    assert!(
        service.read(user).await.unwrap().is_none(),
        "clearing returns the user to the absent state"
    );
    assert!(
        !service.clear(user).await.unwrap(),
        "clearing an absent snapshot removes nothing"
    );
}

#[sqlx::test]
async fn a_save_replaces_rather_than_merges(pool: PgPool) {
    let store = Arc::new(PostgresTrackRepository::new(pool.clone()));
    let user = Uuid::now_v7();
    insert_user(&pool, user, "flynn").await;

    let first = track(&store, "first").await;
    let second = track(&store, "second").await;
    let third = track(&store, "third").await;
    let service = service(&store);

    let saved = service
        .save(
            user,
            save(
                vec![first.clone(), second.clone(), first.clone(), third.clone()],
                Some(2),
                4_200,
            ),
        )
        .await
        .unwrap();
    let replaced = service
        .save(
            user,
            save(vec![third.clone(), second.clone()], Some(0), 900),
        )
        .await
        .unwrap();

    assert_eq!(replaced.revision(), 2);
    assert_eq!(tracks(&replaced), vec![third.clone(), second.clone()]);
    assert_eq!(positions(&replaced), vec![0, 1]);
    assert_eq!(stored_entry_count(&pool, user).await, 2);

    let read = service.read(user).await.unwrap().unwrap();
    assert_eq!(tracks(&read), vec![third.clone(), second.clone()]);
    assert_eq!(read.revision(), 2);
    assert_eq!(read.position_ms(), 900);
    assert!(
        read.entries()
            .iter()
            .all(|entry| entry.track_id() != &first),
        "nothing from the first save survives"
    );
    let before = entry_ids(&saved);
    assert!(
        read.entries()
            .iter()
            .all(|entry| !before.contains(entry.entry_id().as_uuid())),
        "a replacement mints its own entries"
    );
}

#[sqlx::test]
async fn the_bound_is_enforced_and_named(pool: PgPool) {
    let store = Arc::new(PostgresTrackRepository::new(pool.clone()));
    let user = Uuid::now_v7();
    insert_user(&pool, user, "flynn").await;

    let first = track(&store, "first").await;
    let service = service(&store);

    let at_bound = vec![first.clone(); MAX_RESTORE_QUEUE_ENTRIES];
    let saved = service
        .save(
            user,
            save(
                at_bound,
                Some(u32::try_from(MAX_RESTORE_QUEUE_ENTRIES - 1).unwrap()),
                10,
            ),
        )
        .await
        .unwrap();
    assert_eq!(saved.entries().len(), MAX_RESTORE_QUEUE_ENTRIES);
    assert_eq!(saved.revision(), 1);

    let over = vec![first.clone(); MAX_RESTORE_QUEUE_ENTRIES + 1];
    let error = service
        .save(user, save(over, Some(0), 0))
        .await
        .unwrap_err();
    let message = error.to_string();
    match error {
        RestoreQueueError::Invalid(RestoreQueueValidationError::TooManyEntries { max_entries }) => {
            assert_eq!(max_entries, MAX_RESTORE_QUEUE_ENTRIES);
        }
        other => panic!("expected the entry bound, got {other:?}"),
    }
    assert!(
        message.contains(&MAX_RESTORE_QUEUE_ENTRIES.to_string()),
        "the error names the bound, got {message}"
    );

    let read = service.read(user).await.unwrap().unwrap();
    assert_eq!(
        read, saved,
        "an over-bound save leaves the snapshot untouched"
    );
    assert_eq!(
        stored_entry_count(&pool, user).await,
        i64::try_from(MAX_RESTORE_QUEUE_ENTRIES).unwrap()
    );
}

#[sqlx::test]
async fn a_stale_revision_is_a_typed_conflict(pool: PgPool) {
    let store = Arc::new(PostgresTrackRepository::new(pool.clone()));
    let user = Uuid::now_v7();
    insert_user(&pool, user, "flynn").await;

    let first = track(&store, "first").await;
    let second = track(&store, "second").await;
    let third = track(&store, "third").await;
    let service = service(&store);

    let initial = service
        .save(user, save(vec![first.clone()], Some(0), 100))
        .await
        .unwrap();
    assert_eq!(initial.revision(), 1);

    let current = service
        .save_at_revision(user, save(vec![second.clone()], Some(0), 200), Some(1))
        .await
        .unwrap();
    assert_eq!(current.revision(), 2);

    let conflict = service
        .save_at_revision(user, save(vec![third.clone()], Some(0), 300), Some(1))
        .await
        .unwrap_err();
    assert!(matches!(
        conflict,
        RestoreQueueError::Conflict {
            current_revision: 2
        }
    ));
    let read = service.read(user).await.unwrap().unwrap();
    assert_eq!(read.revision(), 2);
    assert_eq!(tracks(&read), vec![second.clone()]);

    let overwritten = service
        .save(user, save(vec![third.clone()], Some(0), 400))
        .await
        .unwrap();
    assert_eq!(
        overwritten.revision(),
        3,
        "a save carrying no revision is last-write-wins"
    );

    assert!(matches!(
        service
            .save_at_revision(user, save(vec![first.clone()], Some(0), 500), Some(99))
            .await
            .unwrap_err(),
        RestoreQueueError::Conflict {
            current_revision: 3
        }
    ));

    let carried = service
        .save_at_revision(user, save(vec![first.clone()], Some(0), 600), Some(3))
        .await
        .unwrap();
    assert_eq!(carried.revision(), 4);
}

#[sqlx::test]
async fn a_save_is_atomic(pool: PgPool) {
    let store = Arc::new(PostgresTrackRepository::new(pool.clone()));
    let user = Uuid::now_v7();
    insert_user(&pool, user, "flynn").await;

    let first = track(&store, "first").await;
    let second = track(&store, "second").await;
    let service = service(&store);

    let saved = service
        .save(
            user,
            save(vec![first.clone(), second.clone()], Some(1), 5_000),
        )
        .await
        .unwrap();

    let unknown = TrackId::new();
    let error = service
        .save(user, save(vec![first.clone(), unknown.clone()], Some(0), 0))
        .await
        .unwrap_err();
    assert!(
        matches!(
            error,
            RestoreQueueError::Invalid(RestoreQueueValidationError::UnknownTrack { ref track_id })
                if track_id == &unknown
        ),
        "a failed entry insert is reported, got {error:?}"
    );

    let read = service.read(user).await.unwrap().unwrap();
    assert_eq!(read, saved, "a failed save leaves the snapshot as it was");
    assert_eq!(stored_entry_count(&pool, user).await, 2);
}

#[sqlx::test]
async fn the_snapshot_survives_a_restart(pool: PgPool) {
    let store = Arc::new(PostgresTrackRepository::new(pool.clone()));
    let user = Uuid::now_v7();
    insert_user(&pool, user, "flynn").await;

    let first = track(&store, "first").await;
    let second = track(&store, "second").await;
    let service = service(&store);

    let saved = service
        .save(
            user,
            save(vec![first.clone(), second.clone()], Some(1), 7_500),
        )
        .await
        .unwrap();

    let options = (*pool.connect_options()).clone();
    let restarted = PgPoolOptions::new()
        .max_connections(2)
        .connect_with(options)
        .await
        .unwrap();
    let restarted_service = RestoreQueueService::new(Arc::new(PostgresTrackRepository::new(
        restarted.clone(),
    )) as Arc<dyn RestoreQueueStore>);

    let read = restarted_service
        .read(user)
        .await
        .unwrap()
        .expect("the snapshot is in postgres, not in the pool");
    assert_eq!(read, saved);
    assert_eq!(tracks(&read), vec![first.clone(), second.clone()]);
    assert_eq!(read.position_ms(), 7_500);

    restarted.close().await;
}

#[sqlx::test]
async fn a_queued_track_is_a_retention_pin(pool: PgPool) {
    let store = Arc::new(PostgresTrackRepository::new(pool.clone()));
    let owner = Uuid::now_v7();
    let stranger = Uuid::now_v7();
    insert_user(&pool, owner, "flynn").await;
    insert_user(&pool, stranger, "other").await;

    let queued = remote_track(&store, "queued", "Queued", "Queue Only").await;
    let heard = remote_track(&store, "heard", "Heard", "Artist").await;
    let orphan = remote_track(&store, "orphan", "Orphan", "Artist").await;
    let service = service(&store);

    let album = store
        .resolve_or_create_album(&AlbumMetadata::new("Queued Album".to_owned()).unwrap())
        .await
        .unwrap();
    store
        .set_album_track(album.id(), &queued, &AlbumTrack::unplaced())
        .await
        .unwrap();

    seed_play(
        &pool,
        owner,
        &heard,
        "2026-09-01T00:00:00Z".parse().unwrap(),
    )
    .await;
    let saved = service
        .save(
            owner,
            save(vec![queued.clone(), heard.clone()], Some(1), 1_000),
        )
        .await
        .unwrap();

    let report = store.retention_report().await.unwrap();
    assert_eq!(report.evictable_tracks, 1, "only the track no queue names");
    assert_eq!(
        report.play_held_tracks, 0,
        "a played track its queue still names is engaged, not play-held"
    );
    assert_eq!(
        report.evictable_artists, 0,
        "an artist credited only on a queued track is engaged"
    );
    assert_eq!(
        report.evictable_albums, 0,
        "an album whose only member a queue names is engaged"
    );

    assert!(
        engaged_titles(&store, owner)
            .await
            .contains(&"Queued".to_owned())
    );
    assert!(
        !engaged_titles(&store, owner)
            .await
            .contains(&"Orphan".to_owned())
    );
    assert!(
        !engaged_titles(&store, stranger)
            .await
            .contains(&"Queued".to_owned()),
        "another user's queue does not engage the track for them"
    );
    assert!(
        browsable_artists(&store, owner)
            .await
            .contains(&"Queue Only".to_owned())
    );
    assert!(
        !browsable_artists(&store, stranger)
            .await
            .contains(&"Queue Only".to_owned())
    );
    assert!(
        browsable_albums(&store, owner)
            .await
            .contains(&"Queued Album".to_owned())
    );
    assert!(
        !browsable_albums(&store, stranger)
            .await
            .contains(&"Queued Album".to_owned())
    );

    let evicted = store.evict_unengaged().await.unwrap();
    assert_eq!(evicted.tracks, 1);
    assert!(store.find_track(&orphan).await.unwrap().is_none());
    assert!(
        store.find_track(&queued).await.unwrap().is_some(),
        "a track its queue names survives the eviction scan"
    );
    assert!(store.find_track(&heard).await.unwrap().is_some());
    assert_eq!(
        service.read(owner).await.unwrap().unwrap(),
        saved,
        "the queue entries are still there afterwards"
    );
    assert_eq!(stored_entry_count(&pool, owner).await, 2);
}

#[sqlx::test]
async fn the_queue_controls_no_output(pool: PgPool) {
    let store = Arc::new(PostgresTrackRepository::new(pool.clone()));
    let user = Uuid::now_v7();
    insert_user(&pool, user, "flynn").await;

    let first = track(&store, "first").await;
    let second = track(&store, "second").await;
    let service = service(&store);

    service
        .save(
            user,
            save(vec![first.clone(), second.clone()], Some(1), 1_000),
        )
        .await
        .unwrap();
    service.read(user).await.unwrap().unwrap();
    service.clear(user).await.unwrap();

    let plays =
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM track_play_stats WHERE user_id = $1")
            .bind(user)
            .fetch_one(&pool)
            .await
            .unwrap();
    let favorites =
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM track_favorites WHERE user_id = $1")
            .bind(user)
            .fetch_one(&pool)
            .await
            .unwrap();
    let ratings =
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM track_ratings WHERE user_id = $1")
            .bind(user)
            .fetch_one(&pool)
            .await
            .unwrap();

    assert_eq!(plays, 0, "a saved queue submits no listen");
    assert_eq!(favorites, 0);
    assert_eq!(ratings, 0);
    assert!(
        service.read(user).await.unwrap().is_none(),
        "the queue's only durable fact is its own snapshot"
    );
}
