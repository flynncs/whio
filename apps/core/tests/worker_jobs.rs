#![cfg(feature = "postgres-integration")]

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

mod support;

use support::worker::{CREDENTIAL_KEY, Worker, database_url};

const RETRY_OVERHEAD_MS: i64 = 1_000;

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
        tokio::time::sleep(Duration::from_millis(20)).await;
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
    let first = whio_core::jobs::probe::enqueue(&mut tx, operation_id)
        .await
        .expect("an enqueue");
    tx.commit().await.expect("the commit");

    let mut tx = pool.begin().await.expect("a transaction");
    let second = whio_core::jobs::probe::enqueue(&mut tx, operation_id)
        .await
        .expect("an enqueue");
    tx.commit().await.expect("the commit");

    let first_id = match first {
        Enqueued::Inserted { job_id } => job_id,
        Enqueued::Coalesced => panic!("the first enqueue inserts"),
    };
    assert_eq!(second, Enqueued::Coalesced, "the second enqueue coalesces");

    assert_eq!(
        count_jobs(&pool, operation_id, "available").await
            + count_jobs(&pool, operation_id, "scheduled").await,
        1,
        "two identical enqueues produce one job"
    );

    let mut worker = Worker::start(&pool, PROBE_QUEUE);
    worker.await_health("/health/ready", 200).await;
    await_job_state(&pool, operation_id, "completed", Duration::from_secs(60)).await;

    let mut tx = pool.begin().await.expect("a transaction");
    let third = whio_core::jobs::probe::enqueue(&mut tx, operation_id)
        .await
        .expect("an enqueue");
    tx.commit().await.expect("the commit");

    let third_id = match third {
        Enqueued::Inserted { job_id } => job_id,
        Enqueued::Coalesced => panic!("a post-completion enqueue inserts a fresh job"),
    };
    assert_ne!(third_id, first_id, "the third enqueue is a new job");
    assert_eq!(
        count_jobs(&pool, operation_id, "completed").await,
        1,
        "the first job completed"
    );
    assert_eq!(
        count_jobs(&pool, operation_id, "available").await
            + count_jobs(&pool, operation_id, "scheduled").await,
        1,
        "the third enqueue produced a second job"
    );
}

#[sqlx::test]
async fn a_duplicate_execution_has_one_effect(pool: PgPool) {
    Database::connect(&database_url(&pool))
        .await
        .expect("the worker's own migrations");

    let operation_id = Uuid::now_v7();

    let mut tx = pool.begin().await.expect("a transaction");
    let enqueued = whio_core::jobs::probe::enqueue_slow(&mut tx, operation_id, 60_000, false)
        .await
        .expect("an enqueue");
    tx.commit().await.expect("the commit");
    assert!(matches!(enqueued, Enqueued::Inserted { .. }));

    let mut worker = CrashWorker::start(&pool, PROBE_QUEUE);
    await_job_state(&pool, operation_id, "running", Duration::from_secs(60)).await;
    await_progress_flushed(&pool, operation_id, Duration::from_secs(30)).await;

    worker.kill9();

    assert_eq!(
        count_jobs(&pool, operation_id, "running").await,
        1,
        "the in-flight job survives the crash rather than being lost"
    );

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
async fn the_retry_schedule_is_measured(pool: PgPool) {
    Database::connect(&database_url(&pool))
        .await
        .expect("the worker's own migrations");

    let operation_id = Uuid::now_v7();

    let mut tx = pool.begin().await.expect("a transaction");
    let enqueued = whio_core::jobs::probe::enqueue_failing(&mut tx, operation_id)
        .await
        .expect("an enqueue");
    tx.commit().await.expect("the commit");
    assert!(matches!(enqueued, Enqueued::Inserted { .. }));

    let started = std::time::Instant::now();
    let mut worker = Worker::start(&pool, PROBE_QUEUE);
    worker.await_health("/health/ready", 200).await;
    await_job_state(&pool, operation_id, "failed", Duration::from_secs(180)).await;
    let elapsed = started.elapsed();

    let errors: serde_json::Value = sqlx::query_scalar(
        "SELECT to_jsonb(errors) FROM awa.jobs
         WHERE args->>'operation_id' = $1 AND state::text = 'failed'",
    )
    .bind(operation_id.to_string())
    .fetch_one(&pool)
    .await
    .expect("the failed job's errors");

    let entries = errors.as_array().expect("a JSON array of errors");
    let max_attempts = PROBE_FAILING_MAX_ATTEMPTS as usize;
    assert_eq!(
        entries.len(),
        max_attempts,
        "the job exhausted exactly max_attempts attempts"
    );

    let times: Vec<(i64, chrono::DateTime<chrono::Utc>)> = entries
        .iter()
        .map(|entry| {
            let attempt = entry["attempt"].as_i64().expect("an attempt number");
            let at =
                chrono::DateTime::parse_from_rfc3339(entry["at"].as_str().expect("a timestamp"))
                    .expect("an RFC 3339 timestamp")
                    .with_timezone(&chrono::Utc);
            (attempt, at)
        })
        .collect();

    for (index, (attempt, _)) in times.iter().enumerate() {
        assert_eq!(
            *attempt,
            (index + 1) as i64,
            "attempts are consecutive from one"
        );
    }

    for window in times.windows(2) {
        let (attempt, at) = &window[0];
        let (_, next_at) = &window[1];
        let gap_ms = (*next_at - *at).num_milliseconds();
        let base_seconds = 2.0_f64.powi(*attempt as i32);
        let lower_ms = (base_seconds * 1000.0) as i64 - 150;
        let upper_ms = (base_seconds * 1.25 * 1000.0) as i64 + RETRY_OVERHEAD_MS;
        assert!(
            gap_ms >= lower_ms && gap_ms <= upper_ms,
            "attempt {attempt} gap {gap_ms}ms outside the backoff envelope [{lower_ms}, {upper_ms}]"
        );
    }

    let backoff_supremum_seconds: f64 = (1..max_attempts)
        .map(|attempt| 2.0_f64.powi(attempt as i32) * 1.25)
        .sum();
    let overhead_seconds = max_attempts as f64 * (RETRY_OVERHEAD_MS as f64 / 1000.0);
    assert!(
        elapsed < Duration::from_secs_f64(backoff_supremum_seconds + overhead_seconds + 5.0),
        "the realised schedule is bounded in time: {elapsed:?}"
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
    let enqueued = whio_core::jobs::probe::enqueue_slow(&mut tx, operation_id, 60_000, false)
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

    worker.await_draining().await;
    await_runtime_shutting_down(&pool, Duration::from_secs(10)).await;

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
    let enqueued = whio_core::jobs::probe::enqueue_slow(&mut tx, operation_id, 60_000, true)
        .await
        .expect("an enqueue");
    tx.commit().await.expect("the commit");
    assert!(matches!(enqueued, Enqueued::Inserted { .. }));

    let mut worker = Worker::start(&pool, PROBE_QUEUE);
    worker.await_health("/health/ready", 200).await;
    await_job_state(&pool, operation_id, "running", Duration::from_secs(60)).await;
    await_progress_flushed(&pool, operation_id, Duration::from_secs(30)).await;

    worker.interrupt();

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
