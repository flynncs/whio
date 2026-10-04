use std::{process::Command, time::Duration};

use sqlx::PgPool;

use crate::support;

use support::worker::{CREDENTIAL_KEY, Worker};

const PROBE_QUEUE: &str = "whio.jobs.probe";

#[sqlx::test]
async fn the_worker_starts_and_reports_health(pool: PgPool) {
    let mut worker = Worker::start(&pool, PROBE_QUEUE);

    let live = worker.await_health("/health/live", 200).await;
    assert_eq!(live["status"], "ok");
    assert_eq!(live["postgres_connected"], true);
    assert_eq!(live["shutting_down"], false);

    let ready = worker.await_health("/health/ready", 200).await;
    assert_eq!(ready["status"], "ready");
    assert_eq!(ready["postgres_connected"], true);
    assert_eq!(ready["leader"], true);

    let engine: (String, String) =
        sqlx::query_as("SELECT current_engine, state FROM awa.storage_transition_state")
            .fetch_one(&pool)
            .await
            .expect("awa's engine state");
    assert_eq!(engine, ("queue_storage".to_owned(), "active".to_owned()));
}

#[test]
fn a_configured_queue_without_a_handler_is_a_startup_error() {
    let output = Command::new(env!("CARGO_BIN_EXE_whio-worker"))
        .env("WHIO_DATABASE_URL", "postgres://whio:whio@127.0.0.1:1/whio")
        .env("WHIO_CREDENTIAL_KEY", CREDENTIAL_KEY)
        .env(
            "WHIO_WORKER_QUEUES",
            format!("{PROBE_QUEUE},whio.jobs.unknown"),
        )
        .env("WHIO_WORKER_BIND_ADDRESS", "127.0.0.1:1")
        .output()
        .expect("the whio-worker binary");

    assert!(!output.status.success(), "the worker refused to start");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("unknown queue `whio.jobs.unknown`"),
        "{stderr}"
    );
    assert!(!stderr.contains("database connection failed"), "{stderr}");
}

#[sqlx::test]
async fn an_idle_worker_shuts_down_cleanly(pool: PgPool) {
    let mut worker = Worker::start(&pool, PROBE_QUEUE);
    worker.await_health("/health/ready", 200).await;

    worker.interrupt();

    let status = worker.await_exit(Duration::from_secs(30)).await;
    assert!(status.success(), "idle shutdown exits cleanly: {status}");
    assert!(
        !worker.log().contains("drain timeout exceeded"),
        "idle shutdown needs no drain timeout:\n{}",
        worker.log()
    );
}
