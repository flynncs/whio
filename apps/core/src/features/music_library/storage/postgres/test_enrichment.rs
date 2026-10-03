use std::{sync::Arc, time::Duration};

use sqlx::PgPool;
use uuid::Uuid;

use super::PostgresTrackRepository;
use crate::features::music_library::{
    catalogue::LibraryService,
    domain::{ArtistCredit, ArtistId, EnrichmentSubject, ProviderId, SourceIdentity, SourceScope},
    storage::contracts::{
        CatalogueWrite, EnrichmentGuard, LibraryRead, Retention, SourceLookup, StateStore,
    },
};

fn source(provider_id: &str, source_scope: SourceScope, external_id: &str) -> SourceIdentity {
    SourceIdentity::new(
        ProviderId::new(provider_id.to_owned()).unwrap(),
        source_scope,
        external_id.to_owned(),
    )
    .unwrap()
}

fn local_source(external_id: &str) -> SourceIdentity {
    source(
        "local",
        SourceScope::Named("/music".to_owned()),
        external_id,
    )
}

async fn artist(
    repository: &PostgresTrackRepository,
    name: &str,
    musicbrainz_artist_id: Option<&str>,
) -> ArtistId {
    repository
        .resolve_or_create_artist(
            &ArtistCredit::new(name.to_owned(), musicbrainz_artist_id.map(str::to_owned)).unwrap(),
        )
        .await
        .unwrap()
        .id()
        .clone()
}

async fn read_subject(
    pool: &PgPool,
    repository: &PostgresTrackRepository,
    artist_id: &ArtistId,
) -> Option<EnrichmentSubject> {
    let mut transaction = pool.begin().await.unwrap();
    let subject = repository
        .enrichment_subject(&mut transaction, artist_id)
        .await
        .unwrap();
    transaction.commit().await.unwrap();

    subject
}

fn library_service(store: &Arc<PostgresTrackRepository>) -> LibraryService {
    LibraryService::new(
        Arc::clone(store) as Arc<dyn LibraryRead>,
        Arc::clone(store) as Arc<dyn SourceLookup>,
        Arc::clone(store) as Arc<dyn StateStore>,
    )
}

#[sqlx::test]
async fn enrichment_subject_returns_sorted_identities_and_a_stable_revision(pool: PgPool) {
    let repository = PostgresTrackRepository::new(pool.clone());
    let artist_id = artist(&repository, "Daft Punk", Some("mbid-daft-punk")).await;

    repository
        .set_artist_source(
            &artist_id,
            &source("youtube_music", SourceScope::Global, "channel-2"),
        )
        .await
        .unwrap();
    repository
        .set_artist_source(&artist_id, &local_source("track-1"))
        .await
        .unwrap();
    repository
        .set_artist_source(
            &artist_id,
            &source("youtube_music", SourceScope::Global, "channel-1"),
        )
        .await
        .unwrap();

    let first = read_subject(&pool, &repository, &artist_id)
        .await
        .expect("the artist exists");
    let second = read_subject(&pool, &repository, &artist_id)
        .await
        .expect("the artist exists");

    assert_eq!(first.artist_id(), &artist_id);
    assert_eq!(first.name(), "Daft Punk");
    assert_eq!(first.musicbrainz_artist_id(), Some("mbid-daft-punk"));
    assert_eq!(first.identities(), second.identities());
    assert_eq!(first.identity_revision(), second.identity_revision());

    let expected = vec![
        local_source("track-1"),
        source("youtube_music", SourceScope::Global, "channel-1"),
        source("youtube_music", SourceScope::Global, "channel-2"),
    ];

    assert_eq!(first.identities(), expected.as_slice());
}

#[sqlx::test]
async fn enrichment_subject_revision_follows_every_basis_member(pool: PgPool) {
    let repository = PostgresTrackRepository::new(pool.clone());
    let artist_id = artist(&repository, "Revision", Some("mbid-first")).await;

    repository
        .set_artist_source(
            &artist_id,
            &source("youtube_music", SourceScope::Global, "channel-1"),
        )
        .await
        .unwrap();

    let base = read_subject(&pool, &repository, &artist_id)
        .await
        .expect("the artist exists");

    sqlx::query("UPDATE artists SET musicbrainz_artist_id = $1 WHERE id = $2")
        .bind("mbid-second")
        .bind(artist_id.as_uuid())
        .execute(&pool)
        .await
        .unwrap();
    let identifier_changed = read_subject(&pool, &repository, &artist_id)
        .await
        .expect("the artist exists");

    sqlx::query("UPDATE artist_sources SET provider_id = 'local' WHERE artist_id = $1")
        .bind(artist_id.as_uuid())
        .execute(&pool)
        .await
        .unwrap();
    let provider_changed = read_subject(&pool, &repository, &artist_id)
        .await
        .expect("the artist exists");

    sqlx::query("UPDATE artist_sources SET source_scope = 'named:/music' WHERE artist_id = $1")
        .bind(artist_id.as_uuid())
        .execute(&pool)
        .await
        .unwrap();
    let scope_changed = read_subject(&pool, &repository, &artist_id)
        .await
        .expect("the artist exists");

    sqlx::query("UPDATE artist_sources SET external_id = 'track-9' WHERE artist_id = $1")
        .bind(artist_id.as_uuid())
        .execute(&pool)
        .await
        .unwrap();
    let external_id_changed = read_subject(&pool, &repository, &artist_id)
        .await
        .expect("the artist exists");

    repository
        .set_artist_source(
            &artist_id,
            &source("youtube_music", SourceScope::Global, "channel-1"),
        )
        .await
        .unwrap();
    let association_added = read_subject(&pool, &repository, &artist_id)
        .await
        .expect("the artist exists");

    for (mutated, previous, label) in [
        (&identifier_changed, &base, "musicbrainz_artist_id"),
        (&provider_changed, &identifier_changed, "provider_id"),
        (&scope_changed, &provider_changed, "source_scope"),
        (&external_id_changed, &scope_changed, "external_id"),
        (&association_added, &external_id_changed, "identities"),
    ] {
        assert_ne!(
            mutated.identity_revision(),
            previous.identity_revision(),
            "{label}"
        );
    }

    assert_eq!(association_added.identities().len(), 2);
    assert_eq!(
        association_added.musicbrainz_artist_id(),
        Some("mbid-second")
    );
}

#[sqlx::test]
async fn enrichment_subject_is_absent_for_an_unknown_artist(pool: PgPool) {
    let repository = PostgresTrackRepository::new(pool.clone());

    assert!(
        read_subject(&pool, &repository, &ArtistId::new())
            .await
            .is_none()
    );
}

#[sqlx::test]
async fn enrichment_subject_without_identifier_or_sources_still_has_a_revision(pool: PgPool) {
    let repository = PostgresTrackRepository::new(pool.clone());
    let artist_id = artist(&repository, "Unidentified", None).await;

    let subject = read_subject(&pool, &repository, &artist_id)
        .await
        .expect("the artist exists");

    assert_eq!(subject.musicbrainz_artist_id(), None);
    assert!(subject.identities().is_empty());
    assert!(!subject.identity_revision().is_empty());
}

#[sqlx::test]
async fn the_service_snapshot_matches_the_guard_snapshot(pool: PgPool) {
    let repository = Arc::new(PostgresTrackRepository::new(pool.clone()));
    let artist_id = artist(&repository, "Both Paths", Some("mbid-both-paths")).await;

    repository
        .set_artist_source(
            &artist_id,
            &source("youtube_music", SourceScope::Global, "channel-2"),
        )
        .await
        .unwrap();
    repository
        .set_artist_source(&artist_id, &local_source("track-1"))
        .await
        .unwrap();

    let service_snapshot = library_service(&repository)
        .enrichment_subject(&artist_id)
        .await
        .unwrap()
        .expect("the artist exists");
    let guard_snapshot = read_subject(&pool, &repository, &artist_id)
        .await
        .expect("the artist exists");

    assert_eq!(service_snapshot.name(), guard_snapshot.name());
    assert_eq!(
        service_snapshot.musicbrainz_artist_id(),
        guard_snapshot.musicbrainz_artist_id()
    );
    assert_eq!(service_snapshot.identities(), guard_snapshot.identities());
    assert_eq!(
        service_snapshot.identity_revision(),
        guard_snapshot.identity_revision()
    );
}

#[sqlx::test]
async fn a_share_lock_holds_the_artist_row_against_deletion(pool: PgPool) {
    let repository = PostgresTrackRepository::new(pool.clone());
    let artist_id = artist(&repository, "Guarded", Some("mbid-guarded")).await;

    repository
        .set_artist_source(&artist_id, &local_source("track-1"))
        .await
        .unwrap();

    let mut transaction = pool.begin().await.unwrap();
    let held = repository
        .enrichment_subject(&mut transaction, &artist_id)
        .await
        .unwrap();

    assert!(held.is_some());

    let (finished_tx, mut finished_rx) = tokio::sync::mpsc::channel::<Result<u64, sqlx::Error>>(1);
    let deleter_pool = pool.clone();
    let deleted = *artist_id.as_uuid();
    let deleter = tokio::spawn(async move {
        let outcome = sqlx::query("DELETE FROM artists WHERE id = $1")
            .bind(deleted)
            .execute(&deleter_pool)
            .await
            .map(|result| result.rows_affected());
        let _ = finished_tx.send(outcome).await;
    });

    let blocked = tokio::time::timeout(Duration::from_millis(2_000), finished_rx.recv()).await;

    assert!(
        blocked.is_err(),
        "the delete must wait for the shared artist lock"
    );

    transaction.commit().await.unwrap();

    let deleted = tokio::time::timeout(Duration::from_secs(15), finished_rx.recv())
        .await
        .expect("the delete finishes once the guard transaction commits")
        .expect("the deleter reports its outcome")
        .expect("the delete succeeds");

    assert_eq!(deleted, 1);
    deleter.await.unwrap();

    assert!(read_subject(&pool, &repository, &artist_id).await.is_none());
}

#[sqlx::test]
async fn an_added_source_association_invalidates_the_snapshot_revision(pool: PgPool) {
    let repository = Arc::new(PostgresTrackRepository::new(pool.clone()));
    let artist_id = artist(&repository, "Neutral", Some("mbid-neutral")).await;

    repository
        .set_artist_source(
            &artist_id,
            &source("youtube_music", SourceScope::Global, "channel-1"),
        )
        .await
        .unwrap();

    let snapshot = library_service(&repository)
        .enrichment_subject(&artist_id)
        .await
        .unwrap()
        .expect("the artist exists");

    repository
        .set_artist_source(&artist_id, &local_source("track-1"))
        .await
        .unwrap();

    let guarded = read_subject(&pool, &repository, &artist_id)
        .await
        .expect("the artist exists");

    assert_ne!(snapshot.identity_revision(), guarded.identity_revision());
    assert_eq!(guarded.identities().len(), 2);
}

#[sqlx::test]
async fn evict_unengaged_reports_retention_artist_ids(pool: PgPool) {
    let repository = PostgresTrackRepository::new(pool.clone());
    let ghost = artist(&repository, "Ghost Ids", Some("mbid-ghost-ids")).await;
    let kept = artist(&repository, "Kept Ids", Some("mbid-kept-ids")).await;
    let user = Uuid::now_v7();

    sqlx::query("INSERT INTO users (id, username, display_name) VALUES ($1, $2, $3)")
        .bind(user)
        .bind("keeper")
        .bind("keeper")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO artist_favorites (user_id, artist_id) VALUES ($1, $2)")
        .bind(user)
        .bind(kept.as_uuid())
        .execute(&pool)
        .await
        .unwrap();

    let evicted = repository.evict_unengaged().await.unwrap();

    assert_eq!(evicted.tracks, 0);
    assert_eq!(evicted.albums, 0);
    assert_eq!(evicted.artists, 1);
    assert_eq!(evicted.evicted_artist_ids, vec![*ghost.as_uuid()]);
    assert_eq!(evicted.artists, evicted.evicted_artist_ids.len() as u64);

    let after = repository.evict_unengaged().await.unwrap();

    assert_eq!(after.artists, 0);
    assert!(after.evicted_artist_ids.is_empty());
    assert!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM artists WHERE id = $1")
            .bind(kept.as_uuid())
            .fetch_one(&pool)
            .await
            .unwrap()
            == 1
    );
}
