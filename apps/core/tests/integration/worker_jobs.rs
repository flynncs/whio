use std::{
    net::{SocketAddr, TcpListener},
    path::PathBuf,
    process::{Child, Command, Stdio},
    time::Duration,
};

use sqlx::PgPool;
use uuid::Uuid;
use whio_core::{
    app::database::Database,
    jobs::probe::{PROBE_FAILING_MAX_ATTEMPTS, PROBE_QUEUE},
    platform::jobs::Enqueued,
};

use crate::support;

use support::worker::{CREDENTIAL_KEY, Worker, database_url};

fn free_address() -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").expect("a free port");
    listener.local_addr().expect("a local address")
}

struct CrashWorker {
    child: Child,
    log: PathBuf,
}

impl CrashWorker {
    fn start(pool: &PgPool, queues: &str) -> Self {
        let address = free_address();
        let log = std::env::temp_dir().join(format!(
            "whio-worker-{}-{}.log",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("a clock")
                .as_nanos()
        ));
        let output = std::fs::File::create(&log).expect("a worker log file");

        let mut command = Command::new(env!("CARGO_BIN_EXE_whio-worker"));
        command
            .env("WHIO_DATABASE_URL", database_url(pool))
            .env("WHIO_CREDENTIAL_KEY", CREDENTIAL_KEY)
            .env("WHIO_WORKER_QUEUES", queues)
            .env("WHIO_WORKER_BIND_ADDRESS", address.to_string())
            .env("WHIO_LOG_LEVEL", "info")
            .stdout(Stdio::from(output.try_clone().expect("a log handle")))
            .stderr(Stdio::from(output));

        let child = command.spawn().expect("the whio-worker binary");

        Self { child, log }
    }

    fn kill9(&mut self) {
        self.child.kill().expect("SIGKILL delivered");
    }
}

impl Drop for CrashWorker {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_file(&self.log);
    }
}

async fn expire_heartbeat(pool: &PgPool, operation_id: Uuid) {
    sqlx::query(
        "UPDATE awa.jobs SET heartbeat_at = now() - interval '2 minutes'
         WHERE args->>'operation_id' = $1 AND state::text = 'running'",
    )
    .bind(operation_id.to_string())
    .execute(pool)
    .await
    .expect("the stale worker heartbeat");
}

async fn release_probe(pool: &PgPool, operation_id: Uuid) {
    sqlx::query(
        "INSERT INTO public.probe_releases (operation_id) VALUES ($1) ON CONFLICT DO NOTHING",
    )
    .bind(operation_id)
    .execute(pool)
    .await
    .expect("the probe release");
}

async fn count_jobs(pool: &PgPool, operation_id: Uuid, state: &str) -> i64 {
    sqlx::query_scalar(
        "SELECT count(*) FROM awa.jobs WHERE args->>'operation_id' = $1 AND state::text = $2",
    )
    .bind(operation_id.to_string())
    .bind(state)
    .fetch_one(pool)
    .await
    .expect("the job count")
}

async fn await_job_state(pool: &PgPool, operation_id: Uuid, state: &str, within: Duration) {
    let deadline = tokio::time::Instant::now() + within;
    loop {
        if count_jobs(pool, operation_id, state).await > 0 {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "job {operation_id} never reached {state}"
        );
        tokio::time::sleep(Duration::from_millis(5)).await
    }
}

async fn effect_transitions(pool: &PgPool, operation_id: Uuid) -> i64 {
    let transitions: i32 =
        sqlx::query_scalar("SELECT transitions FROM public.probe_effects WHERE operation_id = $1")
            .bind(operation_id)
            .fetch_one(pool)
            .await
            .expect("the recorded effect");
    i64::from(transitions)
}

async fn await_progress_flushed(pool: &PgPool, operation_id: Uuid, within: Duration) {
    let deadline = tokio::time::Instant::now() + within;
    loop {
        let flushed: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM awa.jobs
             WHERE args->>'operation_id' = $1 AND state::text = 'running' AND progress IS NOT NULL",
        )
        .bind(operation_id.to_string())
        .fetch_one(pool)
        .await
        .expect("the flushed progress");
        if flushed > 0 {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "job {operation_id} never flushed its effect"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

async fn await_runtime_shutting_down(pool: &PgPool, within: Duration) {
    let deadline = tokio::time::Instant::now() + within;
    loop {
        let shutting_down: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM awa.runtime_instances WHERE shutting_down = TRUE",
        )
        .fetch_one(pool)
        .await
        .expect("the runtime snapshot");
        if shutting_down > 0 {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the runtime instance never reported shutting down"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

#[sqlx::test]
async fn a_rolled_back_enqueue_leaves_no_job(pool: PgPool) {
    Database::connect(&database_url(&pool))
        .await
        .expect("the worker's own migrations");

    let rolled_back = Uuid::now_v7();
    let mut tx = pool.begin().await.expect("a transaction");
    whio_core::jobs::probe::enqueue(&mut tx, rolled_back)
        .await
        .expect("an enqueue");
    tx.rollback().await.expect("the rollback");

    let committed = Uuid::now_v7();
    let mut tx = pool.begin().await.expect("a transaction");
    whio_core::jobs::probe::enqueue(&mut tx, committed)
        .await
        .expect("an enqueue");
    tx.commit().await.expect("the commit");

    assert_eq!(
        count_jobs(&pool, rolled_back, "available").await
            + count_jobs(&pool, rolled_back, "scheduled").await,
        0,
        "the rolled-back enqueue left a job"
    );
    assert_eq!(
        count_jobs(&pool, committed, "available").await
            + count_jobs(&pool, committed, "scheduled").await,
        1,
        "the committed enqueue left exactly one job"
    );
}

#[sqlx::test]
async fn two_identical_enqueues_coalesce(pool: PgPool) {
    Database::connect(&database_url(&pool))
        .await
        .expect("the worker's own migrations");

    let operation_id = Uuid::now_v7();
    let mut tx = pool.begin().await.expect("a transaction");
    let first = whio_core::jobs::probe::enqueue_slow(&mut tx, operation_id, false)
        .await
        .expect("an enqueue");
    tx.commit().await.expect("the commit");
    assert!(matches!(first, Enqueued::Inserted { .. }));

    let mut worker = Worker::start(&pool, PROBE_QUEUE);
    worker.await_health("/health/ready", 200).await;
    await_job_state(&pool, operation_id, "running", Duration::from_secs(60)).await;
    await_progress_flushed(&pool, operation_id, Duration::from_secs(30)).await;

    for _ in 0..2 {
        let mut tx = pool.begin().await.expect("a transaction");
        let repeated = whio_core::jobs::probe::enqueue_slow(&mut tx, operation_id, false)
            .await
            .expect("an enqueue");
        tx.commit().await.expect("the commit");
        assert_eq!(repeated, Enqueued::Coalesced, "a running job coalesces");
    }

    assert_eq!(
        count_jobs(&pool, operation_id, "running").await
            + count_jobs(&pool, operation_id, "available").await
            + count_jobs(&pool, operation_id, "scheduled").await,
        1,
        "three identical enqueues hold one non-terminal job"
    );

    release_probe(&pool, operation_id).await;
    await_job_state(&pool, operation_id, "completed", Duration::from_secs(60)).await;
    assert_eq!(
        count_jobs(&pool, operation_id, "completed").await,
        1,
        "the held job completes after release"
    );
    assert_eq!(
        count_jobs(&pool, operation_id, "available").await
            + count_jobs(&pool, operation_id, "scheduled").await,
        0,
        "coalesced enqueues did not create another job"
    );
    worker.interrupt();
    assert!(worker.await_exit(Duration::from_secs(30)).await.success());

    let mut tx = pool.begin().await.expect("a transaction");
    let after_completion = whio_core::jobs::probe::enqueue_slow(&mut tx, operation_id, false)
        .await
        .expect("an enqueue after completion");
    tx.commit().await.expect("the commit");
    assert!(matches!(after_completion, Enqueued::Inserted { .. }));
    assert_eq!(
        count_jobs(&pool, operation_id, "completed").await
            + count_jobs(&pool, operation_id, "available").await
            + count_jobs(&pool, operation_id, "scheduled").await,
        2,
        "a new enqueue after completion creates a second job"
    );
}

#[sqlx::test]
async fn a_duplicate_execution_has_one_effect(pool: PgPool) {
    Database::connect(&database_url(&pool))
        .await
        .expect("the worker's own migrations");

    let operation_id = Uuid::now_v7();

    let mut tx = pool.begin().await.expect("a transaction");
    let enqueued = whio_core::jobs::probe::enqueue_slow(&mut tx, operation_id, false)
        .await
        .expect("an enqueue");
    tx.commit().await.expect("the commit");
    assert!(matches!(enqueued, Enqueued::Inserted { .. }));

    let mut worker = CrashWorker::start(&pool, PROBE_QUEUE);
    await_job_state(&pool, operation_id, "running", Duration::from_secs(60)).await;
    await_progress_flushed(&pool, operation_id, Duration::from_secs(30)).await;

    worker.kill9();
    expire_heartbeat(&pool, operation_id).await;

    assert_eq!(
        count_jobs(&pool, operation_id, "running").await,
        1,
        "the in-flight job survives the crash rather than being lost"
    );

    release_probe(&pool, operation_id).await;
    let mut worker = Worker::start(&pool, PROBE_QUEUE);
    worker.await_health("/health/ready", 200).await;
    await_job_state(&pool, operation_id, "completed", Duration::from_secs(180)).await;

    let attempt: i16 = sqlx::query_scalar(
        "SELECT attempt FROM awa.jobs
         WHERE args->>'operation_id' = $1 AND state::text = 'completed'",
    )
    .bind(operation_id.to_string())
    .fetch_one(&pool)
    .await
    .expect("the completed job's attempt");

    assert_eq!(attempt, 2, "the rescued job ran twice, once per delivery");
    assert_eq!(
        effect_transitions(&pool, operation_id).await,
        1,
        "two deliveries produced one effect, not two"
    );
}

#[sqlx::test]
async fn the_worker_uses_the_injected_retry_schedule(pool: PgPool) {
    Database::connect(&database_url(&pool))
        .await
        .expect("the worker's own migrations");
    let operation_id = Uuid::now_v7();
    let mut tx = pool.begin().await.expect("a transaction");
    let enqueued = whio_core::jobs::probe::enqueue_failing(&mut tx, operation_id)
        .await
        .expect("a failing job enqueue");
    tx.commit().await.expect("the commit");
    assert!(matches!(enqueued, Enqueued::Inserted { .. }));

    let mut worker = Worker::start(&pool, PROBE_QUEUE);
    worker.await_health("/health/ready", 200).await;
    await_job_state(&pool, operation_id, "failed", Duration::from_secs(60)).await;

    let progress: serde_json::Value = sqlx::query_scalar(
        "SELECT progress FROM awa.jobs WHERE args->>'operation_id' = $1 AND state::text = 'failed'",
    )
    .bind(operation_id.to_string())
    .fetch_one(&pool)
    .await
    .expect("the failed job progress");
    let attempts = progress["metadata"]["probe_retry_attempts"]
        .as_array()
        .expect("the job-row retry attempt history")
        .iter()
        .map(|attempt| {
            let number = attempt["attempt"].as_i64().expect("attempt number") as i16;
            let timestamp = chrono::DateTime::parse_from_rfc3339(
                attempt["at"].as_str().expect("attempt timestamp"),
            )
            .expect("RFC 3339 attempt timestamp")
            .with_timezone(&chrono::Utc);
            (number, timestamp)
        })
        .collect::<Vec<_>>();
    assert_eq!(
        attempts.len(),
        PROBE_FAILING_MAX_ATTEMPTS as usize,
        "the worker exhausted exactly max_attempts"
    );
    assert_eq!(count_jobs(&pool, operation_id, "failed").await, 1);

    for (index, (attempt, _)) in attempts.iter().enumerate() {
        assert_eq!(
            *attempt,
            index as i16 + 1,
            "attempts are ordered and consecutive"
        );
    }
    for window in attempts.windows(2) {
        let attempt = window[0].0;
        let gap_ms = (window[1].1 - window[0].1).num_milliseconds();
        let lower_ms = 0;
        let upper_ms = 1_000;
        assert!(
            gap_ms >= lower_ms && gap_ms <= upper_ms,
            "attempt {attempt} spacing {gap_ms}ms outside the injected retry window [{lower_ms}, {upper_ms}]"
        );
    }
}

#[sqlx::test]
async fn concurrent_coalesced_enqueues_produce_one_job(pool: PgPool) {
    Database::connect(&database_url(&pool))
        .await
        .expect("the worker's own migrations");

    let operation_id = Uuid::now_v7();

    let (first, second) = tokio::join!(
        whio_core::jobs::probe::enqueue_coalesced(&pool, operation_id),
        whio_core::jobs::probe::enqueue_coalesced(&pool, operation_id),
    );

    let inserted = [first.expect("an enqueue"), second.expect("an enqueue")]
        .into_iter()
        .filter(|enqueued| matches!(enqueued, Enqueued::Inserted { .. }))
        .count();
    assert_eq!(
        inserted, 1,
        "two concurrent identical payloads insert one job"
    );
    assert_eq!(
        count_jobs(&pool, operation_id, "available").await
            + count_jobs(&pool, operation_id, "scheduled").await,
        1,
        "the two calls hold one non-terminal job"
    );

    sqlx::query("UPDATE awa.jobs SET state = 'running' WHERE args->>'operation_id' = $1")
        .bind(operation_id.to_string())
        .execute(&pool)
        .await
        .expect("the explicit running state");

    let while_running = whio_core::jobs::probe::enqueue_coalesced(&pool, operation_id)
        .await
        .expect("an enqueue while running");
    assert_eq!(while_running, Enqueued::Coalesced);
    assert_eq!(
        count_jobs(&pool, operation_id, "running").await,
        1,
        "a running job keeps its coalescing claim"
    );

    sqlx::query("UPDATE awa.jobs SET state = 'completed' WHERE args->>'operation_id' = $1")
        .bind(operation_id.to_string())
        .execute(&pool)
        .await
        .expect("the explicit completed state");
    let after_terminal = whio_core::jobs::probe::enqueue_coalesced(&pool, operation_id)
        .await
        .expect("a post-completion enqueue");
    assert!(matches!(after_terminal, Enqueued::Inserted { .. }));
    assert_eq!(
        count_jobs(&pool, operation_id, "completed").await
            + count_jobs(&pool, operation_id, "available").await,
        2,
        "a terminal job releases the claim so a new one inserts"
    );
}

#[sqlx::test]
async fn a_job_enqueued_while_stopped_runs_on_start(pool: PgPool) {
    Database::connect(&database_url(&pool))
        .await
        .expect("the worker's own migrations");

    let operation_id = Uuid::now_v7();

    let mut tx = pool.begin().await.expect("a transaction");
    let enqueued = whio_core::jobs::probe::enqueue(&mut tx, operation_id)
        .await
        .expect("an enqueue");
    tx.commit().await.expect("the commit");
    assert!(matches!(enqueued, Enqueued::Inserted { .. }));

    assert_eq!(
        count_jobs(&pool, operation_id, "completed").await,
        0,
        "the job is not run before a worker exists"
    );

    let mut worker = Worker::start(&pool, PROBE_QUEUE);
    worker.await_health("/health/ready", 200).await;
    await_job_state(&pool, operation_id, "completed", Duration::from_secs(60)).await;

    let applied = worker
        .log()
        .lines()
        .filter(|line| line.contains("probe job applied"))
        .count();
    assert_eq!(
        applied,
        1,
        "the enqueued job ran after start:\n{}",
        worker.log()
    );
}

#[sqlx::test]
async fn a_graceful_stop_drains_or_releases(pool: PgPool) {
    Database::connect(&database_url(&pool))
        .await
        .expect("the worker's own migrations");

    let operation_id = Uuid::now_v7();
    let queued_id = Uuid::now_v7();

    let mut tx = pool.begin().await.expect("a transaction");
    let enqueued = whio_core::jobs::probe::enqueue_slow(&mut tx, operation_id, false)
        .await
        .expect("an enqueue");
    tx.commit().await.expect("the commit");
    assert!(matches!(enqueued, Enqueued::Inserted { .. }));

    let mut tx = pool.begin().await.expect("a transaction");
    let queued = whio_core::jobs::probe::enqueue(&mut tx, queued_id)
        .await
        .expect("an enqueue");
    tx.commit().await.expect("the commit");
    assert!(matches!(queued, Enqueued::Inserted { .. }));

    let mut worker = Worker::start_with(&pool, PROBE_QUEUE, &[("WHIO_WORKER_MAX_WORKERS", "1")]);
    worker.await_health("/health/ready", 200).await;
    await_job_state(&pool, operation_id, "running", Duration::from_secs(60)).await;
    await_progress_flushed(&pool, operation_id, Duration::from_secs(30)).await;

    worker.interrupt();

    let status = worker.await_exit(Duration::from_secs(30)).await;
    assert!(
        status.success(),
        "the draining worker exits cleanly: {status}"
    );

    assert!(
        !worker.log().contains("drain timeout exceeded"),
        "the drain released the in-flight handler within the timeout:\n{}",
        worker.log()
    );

    assert_eq!(
        count_jobs(&pool, queued_id, "available").await
            + count_jobs(&pool, queued_id, "scheduled").await,
        1,
        "the drain stopped claiming new jobs after the handler released its slot:\n{}",
        worker.log()
    );

    assert_eq!(
        count_jobs(&pool, operation_id, "completed").await
            + count_jobs(&pool, operation_id, "running").await,
        1,
        "the in-flight job was finished or released, not lost:\n{}",
        worker.log()
    );

    assert_eq!(
        effect_transitions(&pool, operation_id).await,
        1,
        "the in-flight work was processed, not dropped:\n{}",
        worker.log()
    );
}

#[sqlx::test]
async fn a_graceful_stop_releases_a_job_that_outlasts_the_drain(pool: PgPool) {
    Database::connect(&database_url(&pool))
        .await
        .expect("the worker's own migrations");

    let operation_id = Uuid::now_v7();

    let mut tx = pool.begin().await.expect("a transaction");
    let enqueued = whio_core::jobs::probe::enqueue_slow(&mut tx, operation_id, true)
        .await
        .expect("an enqueue");
    tx.commit().await.expect("the commit");
    assert!(matches!(enqueued, Enqueued::Inserted { .. }));

    let mut worker = Worker::start(&pool, PROBE_QUEUE);
    worker.await_health("/health/ready", 200).await;
    await_job_state(&pool, operation_id, "running", Duration::from_secs(60)).await;
    await_progress_flushed(&pool, operation_id, Duration::from_secs(30)).await;

    worker.interrupt();

    worker.await_draining().await;
    await_runtime_shutting_down(&pool, Duration::from_secs(10)).await;

    let status = worker.await_exit(Duration::from_secs(60)).await;
    assert!(
        status.success(),
        "the draining worker exits cleanly: {status}"
    );

    assert!(
        worker.log().contains("drain timeout exceeded"),
        "the outlasting job forced the drain to time out:\n{}",
        worker.log()
    );
    assert_eq!(
        count_jobs(&pool, operation_id, "running").await,
        1,
        "the outlasting job was released, not finished or lost:\n{}",
        worker.log()
    );

    release_probe(&pool, operation_id).await;
    let mut worker = Worker::start(&pool, PROBE_QUEUE);
    worker.await_health("/health/ready", 200).await;
    await_job_state(&pool, operation_id, "completed", Duration::from_secs(180)).await;

    assert_eq!(
        effect_transitions(&pool, operation_id).await,
        1,
        "the recovered run did not duplicate the effect:\n{}",
        worker.log()
    );
}
