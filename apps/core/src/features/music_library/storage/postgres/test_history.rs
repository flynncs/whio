use super::PostgresTrackRepository;
use crate::features::music_library::domain::{
    ListenExtent, ListenOrder, ListenOrigin, ListenOutcome, ListenQuery, ListenSessionId,
    ListenSubmission, ListenWindow, PageLimit, ProviderId, SourceIdentity, SourceScope, Track,
    TrackId, TrackMetadata,
};
use crate::features::music_library::history::{ListenError, ListenService};
use crate::features::music_library::storage::contracts::{
    CatalogueWrite, LibraryRead, ListenStore, StateStore,
};
use chrono::{DateTime, Duration, Utc};
use sqlx::PgPool;
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
    TrackMetadata::from_names(title.to_owned(), vec!["Artist".to_owned()], Some(240_000)).unwrap()
}

fn at(value: &str) -> DateTime<Utc> {
    value.parse().unwrap()
}

fn query(window: ListenWindow, order: ListenOrder, limit: u32, offset: u32) -> ListenQuery {
    ListenQuery::new(window, order, PageLimit::new(limit).unwrap(), offset)
}

fn listen(
    principal: Uuid,
    origin: ListenOrigin,
    track: &Track,
    listen_at: DateTime<Utc>,
    session_id: Option<&str>,
    extent: Option<ListenExtent>,
) -> ListenSubmission {
    ListenSubmission::new(
        principal,
        origin,
        track.id().clone(),
        listen_at,
        session_id.map(|value| ListenSessionId::new(value.to_owned()).unwrap()),
        extent,
    )
}

fn service(store: &Arc<PostgresTrackRepository>) -> ListenService {
    ListenService::new(
        Arc::clone(store) as Arc<dyn ListenStore>,
        Arc::clone(store) as Arc<dyn LibraryRead>,
    )
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

async fn create_track(repository: &PostgresTrackRepository, external_id: &str) -> Track {
    repository
        .get_or_create(local_source(external_id), metadata(external_id))
        .await
        .unwrap()
}

async fn set_marker(pool: &PgPool, principal: Uuid, started_at: DateTime<Utc>) {
    sqlx::query("INSERT INTO listen_marks (user_id, started_at) VALUES ($1, $2)")
        .bind(principal)
        .bind(started_at)
        .execute(pool)
        .await
        .unwrap();
}

async fn marker(pool: &PgPool, principal: Uuid) -> Option<DateTime<Utc>> {
    sqlx::query_scalar("SELECT started_at FROM listen_marks WHERE user_id = $1")
        .bind(principal)
        .fetch_optional(pool)
        .await
        .unwrap()
}

async fn ledger_rows(pool: &PgPool, principal: Uuid, track_id: &TrackId) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM listens WHERE user_id = $1 AND track_id = $2")
        .bind(principal)
        .bind(track_id.as_uuid())
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn counter(pool: &PgPool, principal: Uuid, track_id: &TrackId) -> Option<i32> {
    sqlx::query_scalar(
        "SELECT play_count FROM track_play_stats WHERE user_id = $1 AND track_id = $2",
    )
    .bind(principal)
    .bind(track_id.as_uuid())
    .fetch_optional(pool)
    .await
    .unwrap()
}

#[sqlx::test]
async fn a_listen_records_its_extent(pool: PgPool) {
    let repository = Arc::new(PostgresTrackRepository::new(pool.clone()));
    let principal = Uuid::now_v7();
    insert_user(&pool, principal, "flynn").await;
    let track = create_track(&repository, "extent.flac").await;
    let service = service(&repository);

    let listen_at = at("2026-09-01T10:00:00Z");
    let outcome = service
        .submit_listen(&listen(
            principal,
            ListenOrigin::Native,
            &track,
            listen_at,
            Some("session-extent"),
            Some(ListenExtent::new(1_000, 240_000, true)),
        ))
        .await
        .unwrap();
    assert_eq!(outcome, ListenOutcome::Recorded);

    let page = service
        .history(
            principal,
            &query(ListenWindow::unbounded(), ListenOrder::NewestFirst, 50, 0),
        )
        .await
        .unwrap();
    assert_eq!(page.entries().len(), 1);

    let record = &page.entries()[0];
    assert_eq!(record.origin(), ListenOrigin::Native);
    assert_eq!(record.track_id(), track.id());
    assert_eq!(record.listen_at(), listen_at);
    assert_eq!(
        record.session_id().map(ListenSessionId::as_str),
        Some("session-extent")
    );

    let extent = record.extent().unwrap();
    assert_eq!(extent.start_position_ms(), 1_000);
    assert_eq!(extent.listened_ms(), 240_000);
    assert!(extent.qualified());
}

#[sqlx::test]
async fn a_short_listen_is_recorded_and_does_not_count(pool: PgPool) {
    let repository = Arc::new(PostgresTrackRepository::new(pool.clone()));
    let principal = Uuid::now_v7();
    insert_user(&pool, principal, "flynn").await;
    let track = create_track(&repository, "four-minutes.flac").await;
    let service = service(&repository);

    let outcome = service
        .submit_listen(&listen(
            principal,
            ListenOrigin::Native,
            &track,
            at("2026-09-02T10:00:00Z"),
            Some("session-skip"),
            Some(ListenExtent::new(0, 12_000, false)),
        ))
        .await
        .unwrap();
    assert_eq!(outcome, ListenOutcome::Recorded);

    let page = service
        .history(
            principal,
            &query(ListenWindow::unbounded(), ListenOrder::NewestFirst, 50, 0),
        )
        .await
        .unwrap();
    assert_eq!(page.entries().len(), 1);
    let extent = page.entries()[0].extent().unwrap();
    assert_eq!(extent.listened_ms(), 12_000);
    assert!(!extent.qualified());

    let count = service
        .count(principal, ListenWindow::unbounded(), Some(track.id()))
        .await
        .unwrap();
    assert_eq!(count.count(), 0);
    assert_eq!(count.coverage().since(), page.coverage().since());
    assert_eq!(counter(&pool, principal, track.id()).await, None);
}

#[sqlx::test]
async fn the_all_time_total_is_the_counter(pool: PgPool) {
    let repository = Arc::new(PostgresTrackRepository::new(pool.clone()));
    let principal = Uuid::now_v7();
    insert_user(&pool, principal, "flynn").await;
    let track = create_track(&repository, "baseline.flac").await;
    let service = service(&repository);

    sqlx::query(
        "INSERT INTO track_play_stats (user_id, track_id, play_count, last_played_at) \
         VALUES ($1, $2, 5, $3)",
    )
    .bind(principal)
    .bind(track.id().as_uuid())
    .bind(at("2026-08-01T10:00:00Z"))
    .execute(&pool)
    .await
    .unwrap();

    for day in 1..=3 {
        service
            .submit_listen(&listen(
                principal,
                ListenOrigin::Native,
                &track,
                at(&format!("2026-09-0{day}T10:00:00Z")),
                Some(&format!("session-{day}")),
                Some(ListenExtent::new(0, 240_000, true)),
            ))
            .await
            .unwrap();
    }

    let state = repository
        .find_track_state(track.id(), principal)
        .await
        .unwrap();
    let counter = counter(&pool, principal, track.id()).await.unwrap();
    let ledger = ledger_rows(&pool, principal, track.id()).await;

    assert_eq!(counter, 8);
    assert_eq!(ledger, 3);
    assert_eq!(state.play_count(), u32::try_from(counter).unwrap());
    assert_ne!(
        state.play_count(),
        u32::try_from(counter + ledger as i32).unwrap()
    );

    let unbounded = service
        .count(principal, ListenWindow::unbounded(), Some(track.id()))
        .await
        .unwrap();
    assert_eq!(unbounded.count(), u64::from(state.play_count()));

    let unbounded_principal = service
        .count(principal, ListenWindow::unbounded(), None)
        .await
        .unwrap();
    assert_eq!(unbounded_principal.count(), u64::from(state.play_count()));

    let windowed = service
        .count(
            principal,
            ListenWindow::new(Some(at("2026-09-01T00:00:00Z")), None),
            Some(track.id()),
        )
        .await
        .unwrap();
    assert_eq!(windowed.count(), u64::try_from(ledger).unwrap());

    let baseline_only = create_track(&repository, "baseline-only.flac").await;
    sqlx::query(
        "INSERT INTO track_play_stats (user_id, track_id, play_count, last_played_at) \
         VALUES ($1, $2, 4, $3)",
    )
    .bind(principal)
    .bind(baseline_only.id().as_uuid())
    .bind(at("2026-08-02T10:00:00Z"))
    .execute(&pool)
    .await
    .unwrap();

    let baseline_state = repository
        .find_track_state(baseline_only.id(), principal)
        .await
        .unwrap();
    let baseline_count = service
        .count(
            principal,
            ListenWindow::unbounded(),
            Some(baseline_only.id()),
        )
        .await
        .unwrap();
    assert_eq!(
        baseline_count.count(),
        u64::from(baseline_state.play_count())
    );
    assert_eq!(baseline_count.count(), 4);
    assert_eq!(ledger_rows(&pool, principal, baseline_only.id()).await, 0);
}

#[sqlx::test]
async fn an_exact_repeat_is_suppressed_entirely(pool: PgPool) {
    let repository = Arc::new(PostgresTrackRepository::new(pool.clone()));
    let principal = Uuid::now_v7();
    insert_user(&pool, principal, "flynn").await;
    let track = create_track(&repository, "repeat.flac").await;
    let service = service(&repository);

    let listen_at = at("2026-09-03T10:00:00Z");
    let repeat = listen(
        principal,
        ListenOrigin::SubsonicScrobble,
        &track,
        listen_at,
        None,
        Some(ListenExtent::new(0, 240_000, true)),
    );

    assert_eq!(
        service.submit_listen(&repeat).await.unwrap(),
        ListenOutcome::Recorded
    );
    assert_eq!(
        service.submit_listen(&repeat).await.unwrap(),
        ListenOutcome::Duplicate
    );
    assert_eq!(ledger_rows(&pool, principal, track.id()).await, 1);
    assert_eq!(counter(&pool, principal, track.id()).await, Some(1));

    let later = listen(
        principal,
        ListenOrigin::SubsonicScrobble,
        &track,
        listen_at + Duration::minutes(30),
        None,
        Some(ListenExtent::new(0, 240_000, true)),
    );
    assert_eq!(
        service.submit_listen(&later).await.unwrap(),
        ListenOutcome::Recorded
    );
    assert_eq!(ledger_rows(&pool, principal, track.id()).await, 2);
    assert_eq!(counter(&pool, principal, track.id()).await, Some(2));
}

#[sqlx::test]
async fn a_windowed_count_clamps_to_the_marker(pool: PgPool) {
    let repository = Arc::new(PostgresTrackRepository::new(pool.clone()));
    let principal = Uuid::now_v7();
    insert_user(&pool, principal, "flynn").await;
    let track = create_track(&repository, "marker.flac").await;
    let service = service(&repository);

    let marker = at("2026-09-10T00:00:00Z");
    set_marker(&pool, principal, marker).await;

    for (session, listen_at) in [
        ("session-before", marker - Duration::hours(1)),
        ("session-after", marker + Duration::hours(1)),
        ("session-later", marker + Duration::hours(2)),
    ] {
        service
            .submit_listen(&listen(
                principal,
                ListenOrigin::Native,
                &track,
                listen_at,
                Some(session),
                Some(ListenExtent::new(0, 240_000, true)),
            ))
            .await
            .unwrap();
    }

    let wide = ListenWindow::new(
        Some(marker - Duration::hours(2)),
        Some(marker + Duration::hours(3)),
    );
    let count = service
        .count(principal, wide, Some(track.id()))
        .await
        .unwrap();
    assert_eq!(count.count(), 2);
    assert_eq!(count.coverage().since(), Some(marker));

    let page = service
        .history(principal, &query(wide, ListenOrder::OldestFirst, 50, 0))
        .await
        .unwrap();
    assert_eq!(page.entries().len(), 2);
    assert_eq!(page.coverage().since(), Some(marker));

    let before = ListenWindow::new(
        Some(marker - Duration::hours(3)),
        Some(marker - Duration::hours(2)),
    );
    let uncovered = service
        .count(principal, before, Some(track.id()))
        .await
        .unwrap();
    assert_eq!(uncovered.count(), 0);
    assert_eq!(uncovered.coverage().since(), None);

    let uncovered_page = service
        .history(principal, &query(before, ListenOrder::OldestFirst, 50, 0))
        .await
        .unwrap();
    assert!(uncovered_page.entries().is_empty());
    assert_eq!(uncovered_page.coverage().since(), None);

    let empty = ListenWindow::new(
        Some(marker + Duration::hours(5)),
        Some(marker + Duration::hours(6)),
    );
    let covered_empty = service
        .count(principal, empty, Some(track.id()))
        .await
        .unwrap();
    assert_eq!(covered_empty.count(), 0);
    assert_eq!(covered_empty.coverage().since(), Some(marker));
}

#[sqlx::test]
async fn a_legacy_scrobble_leaves_the_extent_absent(pool: PgPool) {
    let repository = Arc::new(PostgresTrackRepository::new(pool.clone()));
    let principal = Uuid::now_v7();
    insert_user(&pool, principal, "flynn").await;
    let track = create_track(&repository, "legacy.flac").await;
    let service = service(&repository);

    let outcome = service
        .submit_listen(&listen(
            principal,
            ListenOrigin::SubsonicScrobble,
            &track,
            at("2026-09-04T10:00:00Z"),
            None,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(outcome, ListenOutcome::Recorded);

    let page = service
        .history(
            principal,
            &query(ListenWindow::unbounded(), ListenOrder::NewestFirst, 50, 0),
        )
        .await
        .unwrap();
    assert_eq!(page.entries().len(), 1);
    assert_eq!(page.entries()[0].origin(), ListenOrigin::SubsonicScrobble);
    assert!(page.entries()[0].session_id().is_none());
    assert!(page.entries()[0].extent().is_none());

    let count = service
        .count(principal, ListenWindow::unbounded(), Some(track.id()))
        .await
        .unwrap();
    assert_eq!(count.count(), 1);
    assert_eq!(counter(&pool, principal, track.id()).await, Some(1));
}

#[sqlx::test]
async fn distinct_legitimate_listens_stay_distinct(pool: PgPool) {
    let repository = Arc::new(PostgresTrackRepository::new(pool.clone()));
    let principal = Uuid::now_v7();
    insert_user(&pool, principal, "flynn").await;
    let track = create_track(&repository, "distinct.flac").await;
    let service = service(&repository);

    let first = at("2026-09-05T10:00:00Z");
    let second = first + Duration::minutes(5);

    for listen_at in [first, second] {
        let outcome = service
            .submit_listen(&listen(
                principal,
                ListenOrigin::ReportPlayback,
                &track,
                listen_at,
                Some("session-shared"),
                Some(ListenExtent::new(0, 240_000, true)),
            ))
            .await
            .unwrap();
        assert_eq!(outcome, ListenOutcome::Recorded);
    }

    assert_eq!(ledger_rows(&pool, principal, track.id()).await, 2);
    assert_eq!(counter(&pool, principal, track.id()).await, Some(2));

    let newest = service
        .history(
            principal,
            &query(ListenWindow::unbounded(), ListenOrder::NewestFirst, 50, 0),
        )
        .await
        .unwrap();
    assert_eq!(newest.entries().len(), 2);
    assert_eq!(newest.entries()[0].listen_at(), second);
    assert_eq!(newest.entries()[1].listen_at(), first);

    let oldest = service
        .history(
            principal,
            &query(ListenWindow::unbounded(), ListenOrder::OldestFirst, 1, 1),
        )
        .await
        .unwrap();
    assert_eq!(oldest.entries().len(), 1);
    assert_eq!(oldest.entries()[0].listen_at(), second);

    let windowed = service
        .history(
            principal,
            &query(
                ListenWindow::new(Some(second), None),
                ListenOrder::OldestFirst,
                50,
                0,
            ),
        )
        .await
        .unwrap();
    assert_eq!(windowed.entries().len(), 1);
    assert_eq!(windowed.entries()[0].listen_at(), second);
}

#[sqlx::test]
async fn the_write_is_atomic(pool: PgPool) {
    let repository = Arc::new(PostgresTrackRepository::new(pool.clone()));
    let principal = Uuid::now_v7();
    insert_user(&pool, principal, "flynn").await;
    let track = create_track(&repository, "atomic.flac").await;
    let service = service(&repository);

    let unknown = TrackId::new();
    let rejected = service
        .submit_listen(&ListenSubmission::new(
            principal,
            ListenOrigin::Native,
            unknown.clone(),
            at("2026-09-06T10:00:00Z"),
            None,
            Some(ListenExtent::new(0, 240_000, true)),
        ))
        .await;
    assert!(matches!(rejected, Err(ListenError::UnknownTrack)));
    assert_eq!(ledger_rows(&pool, principal, &unknown).await, 0);
    assert_eq!(counter(&pool, principal, &unknown).await, None);

    let failed = repository
        .submit_listen(&ListenSubmission::new(
            principal,
            ListenOrigin::Native,
            unknown.clone(),
            at("2026-09-06T11:00:00Z"),
            None,
            Some(ListenExtent::new(0, 240_000, true)),
        ))
        .await;
    assert!(failed.is_err());
    assert_eq!(ledger_rows(&pool, principal, &unknown).await, 0);
    assert_eq!(counter(&pool, principal, &unknown).await, None);
    assert!(marker(&pool, principal).await.is_none());

    sqlx::query(
        "INSERT INTO track_play_stats (user_id, track_id, play_count, last_played_at) \
         VALUES ($1, $2, $3, $4)",
    )
    .bind(principal)
    .bind(track.id().as_uuid())
    .bind(i32::MAX)
    .bind(at("2026-09-06T00:00:00Z"))
    .execute(&pool)
    .await
    .unwrap();

    let overflowed = repository
        .submit_listen(&listen(
            principal,
            ListenOrigin::Native,
            &track,
            at("2026-09-06T12:00:00Z"),
            None,
            Some(ListenExtent::new(0, 240_000, true)),
        ))
        .await;
    assert!(overflowed.is_err());
    assert_eq!(ledger_rows(&pool, principal, track.id()).await, 0);
    assert_eq!(counter(&pool, principal, track.id()).await, Some(i32::MAX));
}
