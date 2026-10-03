#![cfg(feature = "postgres-integration")]

use std::time::Duration;

use sqlx::{Acquire, PgPool};
use uuid::Uuid;
use whio_core::{
    app::database::Database,
    jobs::probe::{PROBE_KIND, PROBE_QUEUE},
    platform::jobs::Enqueued,
};

mod support;

use support::worker::{Worker, database_url};

async fn project_ledger(pool: &PgPool) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM _sqlx_migrations")
        .fetch_one(pool)
        .await
        .expect("the project migration ledger")
}

async fn awa_ledger(pool: &PgPool) -> Option<i32> {
    sqlx::query_scalar("SELECT max(version) FROM awa.schema_version")
        .fetch_one(pool)
        .await
        .expect("the awa migration ledger")
}

async fn tables(pool: &PgPool, schema: &str) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM pg_tables WHERE schemaname = $1")
        .bind(schema)
        .fetch_one(pool)
        .await
        .expect("the table count")
}

#[sqlx::test(migrations = false)]
async fn both_migration_sets_apply(pool: PgPool) {
    assert_eq!(tables(&pool, "awa").await, 0);

    let database = Database::connect(&database_url(&pool))
        .await
        .expect("both migration sets to apply");

    assert!(tables(&pool, "public").await > 0);
    assert!(tables(&pool, "awa").await > 0);

    let first = (project_ledger(&pool).await, awa_ledger(&pool).await);
    assert_eq!(first.1, Some(awa::migrations::CURRENT_VERSION));

    drop(database);

    Database::connect(&database_url(&pool))
        .await
        .expect("a second start to be a no-op");

    assert_eq!(
        (project_ledger(&pool).await, awa_ledger(&pool).await),
        first
    );
}

#[sqlx::test]
async fn both_migration_sets_apply_after_the_project_migrations(pool: PgPool) {
    let project = project_ledger(&pool).await;
    assert!(project > 0);
    assert_eq!(tables(&pool, "awa").await, 0);

    Database::connect(&database_url(&pool))
        .await
        .expect("awa's set to follow the project's");

    assert_eq!(project_ledger(&pool).await, project);
    assert_eq!(
        awa_ledger(&pool).await,
        Some(awa::migrations::CURRENT_VERSION)
    );
}

#[sqlx::test]
async fn the_harmless_job_runs_and_is_idempotent(pool: PgPool) {
    Database::connect(&database_url(&pool))
        .await
        .expect("the worker's own migrations");

    let operation_id = Uuid::now_v7();

    let mut tx = pool.begin().await.expect("a transaction");
    let first = whio_core::jobs::probe::enqueue(&mut tx, operation_id)
        .await
        .expect("a committed probe request");
    tx.commit().await.expect("the transaction to commit");

    let mut tx = pool.begin().await.expect("a transaction");
    let second = whio_core::jobs::probe::enqueue(&mut tx, operation_id)
        .await
        .expect("a coalesced probe request");
    tx.commit().await.expect("the transaction to commit");

    let first = match first {
        Enqueued::Inserted { job_id } => job_id,
        Enqueued::Coalesced => panic!("the first request inserts"),
    };
    assert_eq!(second, Enqueued::Coalesced, "the second request coalesces");

    let queued: i64 = sqlx::query_scalar("SELECT count(*) FROM awa.jobs WHERE kind = $1")
        .bind(PROBE_KIND)
        .fetch_one(&pool)
        .await
        .expect("the queue");
    assert_eq!(queued, 1, "one job holds both requests");

    let mut worker = Worker::start(&pool, PROBE_QUEUE);
    worker.await_health("/health/ready", 200).await;

    await_completion(&pool, first).await;

    let (state, attempt, errors, queue): (String, i16, serde_json::Value, String) = sqlx::query_as(
        "SELECT state::text, attempt, to_jsonb(errors), queue FROM awa.jobs WHERE id = $1",
    )
    .bind(first)
    .fetch_one(&pool)
    .await
    .expect("the completed probe job");
    assert_eq!(state, "completed");
    assert_eq!(attempt, 1, "the job executes once");
    assert_eq!(errors, serde_json::json!([]));
    assert_eq!(queue, PROBE_QUEUE);

    let applied = worker
        .log()
        .lines()
        .filter(|line| line.contains("probe job applied"))
        .count();
    assert_eq!(applied, 1, "the request applies once:\n{}", worker.log());
}

#[sqlx::test]
async fn a_coalesced_enqueue_keeps_the_callers_transaction(pool: PgPool) {
    Database::connect(&database_url(&pool))
        .await
        .expect("the worker's own migrations");

    let operation_id = Uuid::now_v7();

    let mut tx = pool.begin().await.expect("a transaction");
    whio_core::jobs::probe::enqueue(&mut tx, operation_id)
        .await
        .expect("a committed probe request");
    tx.commit().await.expect("the transaction to commit");

    let mut connection = pool.acquire().await.expect("a caller connection");
    sqlx::query("CREATE TEMPORARY TABLE caller_row (id INTEGER)")
        .execute(&mut *connection)
        .await
        .expect("a caller-owned table");

    let mut tx = connection.begin().await.expect("a caller transaction");
    sqlx::query("INSERT INTO caller_row (id) VALUES (1)")
        .execute(&mut *tx)
        .await
        .expect("the caller's write");

    let enqueued = whio_core::jobs::probe::enqueue(&mut tx, operation_id)
        .await
        .expect("a coalesced probe request");
    assert_eq!(
        enqueued,
        Enqueued::Coalesced,
        "the request joins the claimed key"
    );

    tx.commit()
        .await
        .expect("the caller's transaction to commit");

    let written: i64 = sqlx::query_scalar("SELECT count(*) FROM caller_row")
        .fetch_one(&mut *connection)
        .await
        .expect("the caller's row");
    assert_eq!(
        written, 1,
        "the coalesced request did not abort the caller's transaction"
    );
}

async fn await_completion(pool: &PgPool, job_id: i64) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(60);

    loop {
        let row: Option<String> =
            sqlx::query_scalar("SELECT state::text FROM awa.jobs WHERE id = $1")
                .bind(job_id)
                .fetch_optional(pool)
                .await
                .expect("the probe job row");

        if row.as_deref() == Some("completed") {
            return;
        }

        assert!(
            tokio::time::Instant::now() < deadline,
            "the probe job did not complete: {row:?}"
        );

        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}
