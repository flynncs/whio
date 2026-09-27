use std::sync::Arc;

use axum::{Json, Router, extract::State, http::StatusCode, response::IntoResponse, routing::get};
use serde::Serialize;
use sqlx::PgPool;
use thiserror::Error;
use tokio::net::TcpListener;

use crate::{
    app::config::{Config, ConfigError, WorkerConfig},
    app::database::{Database, DatabaseError},
    app::startup::shutdown_signal,
    jobs::{self, QueueRegistration},
};

#[derive(Debug, Error)]
pub(crate) enum WorkerError {
    #[error("invalid worker configuration")]
    Config(#[from] ConfigError),

    #[error("missing required WHIO_WORKER_QUEUES")]
    MissingQueues,

    #[error("unknown queue `{queue}` in WHIO_WORKER_QUEUES")]
    UnknownQueue { queue: String },

    #[error("no job handler is registered for queue `{queue}`")]
    UnregisteredQueue { queue: String },

    #[error(transparent)]
    Database(#[from] DatabaseError),

    #[error("awa worker failed to build")]
    Build(#[source] awa::BuildError),

    #[error("awa worker failed to start")]
    Start(#[source] awa::AwaError),

    #[error("worker listener failed")]
    Listen(#[source] std::io::Error),

    #[error("worker server failed")]
    Serve(#[source] std::io::Error),

    #[error("worker server task failed")]
    Join(#[source] tokio::task::JoinError),
}

#[derive(Serialize)]
struct WorkerHealth {
    status: &'static str,
    shutting_down: bool,
    postgres_connected: bool,
    poll_loop_alive: bool,
    heartbeat_alive: bool,
    maintenance_alive: bool,
    leader: bool,
}

impl WorkerHealth {
    fn of(health: &awa::HealthCheck, status: &'static str) -> Self {
        Self {
            status,
            shutting_down: health.shutting_down,
            postgres_connected: health.postgres_connected,
            poll_loop_alive: health.poll_loop_alive,
            heartbeat_alive: health.heartbeat_alive,
            maintenance_alive: health.maintenance_alive,
            leader: health.leader,
        }
    }
}

pub async fn run() -> Result<(), Box<dyn std::error::Error>> {
    let config = Config::from_env()?;

    tracing_subscriber::fmt()
        .with_max_level(config.log_level)
        .init();

    let worker = config.worker;
    let registered = jobs::queues();
    let queues = check_queues(&worker.queues, &registered)?;
    let database = Database::connect(&config.database_url).await?;
    let client = build(&database.pool(), &queues, &worker)?;

    tracing::info!(queues = ?worker.queues, "awa worker starting");

    client.start().await.map_err(WorkerError::Start)?;

    serve_until_shutdown(client, worker).await
}

fn check_queues<'a>(
    configured: &[String],
    registered: &'a [QueueRegistration],
) -> Result<Vec<&'a QueueRegistration>, WorkerError> {
    if configured.is_empty() {
        return Err(WorkerError::MissingQueues);
    }

    configured
        .iter()
        .map(|name| {
            let queue = registered
                .iter()
                .find(|queue| queue.name == name)
                .ok_or_else(|| WorkerError::UnknownQueue {
                    queue: name.clone(),
                })?;

            if queue.kinds.is_empty() {
                return Err(WorkerError::UnregisteredQueue {
                    queue: name.clone(),
                });
            }

            Ok(queue)
        })
        .collect()
}

fn build(
    pool: &PgPool,
    queues: &[&QueueRegistration],
    worker: &WorkerConfig,
) -> Result<awa::Client, WorkerError> {
    let queue_config = awa::QueueConfig {
        max_workers: worker.max_workers,
        poll_interval: worker.poll_interval,
        ..awa::QueueConfig::default()
    };

    jobs::register(
        awa::Client::builder(pool.clone()).promote_interval(worker.promote_interval),
        queues,
        &queue_config,
    )
    .build()
    .map_err(WorkerError::Build)
}

async fn serve_until_shutdown(
    client: awa::Client,
    worker: WorkerConfig,
) -> Result<(), Box<dyn std::error::Error>> {
    let client = Arc::new(client);
    let listener = TcpListener::bind(worker.bind_address)
        .await
        .map_err(WorkerError::Listen)?;

    tracing::info!(
        address = %worker.bind_address,
        drain_timeout_seconds = worker.drain_timeout.as_secs(),
        "worker health listening"
    );

    let (drained, drained_rx) = tokio::sync::oneshot::channel();
    let router = health_router(Arc::clone(&client));
    let mut server = tokio::spawn(async move {
        axum::serve(listener, router)
            .with_graceful_shutdown(async move {
                let _ = drained_rx.await;
            })
            .await
    });

    tokio::select! {
        joined = &mut server => joined.map_err(WorkerError::Join)?.map_err(WorkerError::Serve)?,
        () = shutdown_signal() => {
            client.shutdown(worker.drain_timeout).await;
            let _ = drained.send(());
            server.await.map_err(WorkerError::Join)?.map_err(WorkerError::Serve)?;
        }
    }

    Ok(())
}

fn health_router(client: Arc<awa::Client>) -> Router {
    Router::new()
        .route("/health/live", get(live))
        .route("/health/ready", get(ready))
        .with_state(client)
}

fn services_alive(health: &awa::HealthCheck) -> bool {
    health.postgres_connected && health.heartbeat_alive && health.maintenance_alive
}

async fn live(State(client): State<Arc<awa::Client>>) -> impl IntoResponse {
    let health = client.health_check().await;

    if services_alive(&health) {
        (StatusCode::OK, Json(WorkerHealth::of(&health, "ok"))).into_response()
    } else {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(WorkerHealth::of(&health, "not_ready")),
        )
            .into_response()
    }
}

async fn ready(State(client): State<Arc<awa::Client>>) -> impl IntoResponse {
    let health = client.health_check().await;

    if health.healthy {
        (StatusCode::OK, Json(WorkerHealth::of(&health, "ready"))).into_response()
    } else {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(WorkerHealth::of(&health, "not_ready")),
        )
            .into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jobs::{KindRegistration, QueueRegistration};

    fn handler(builder: awa::ClientBuilder) -> awa::ClientBuilder {
        builder
    }

    fn registered<'a>(name: &'static str, kinds: &'a [KindRegistration]) -> Vec<QueueRegistration>
    where
        'a: 'static,
    {
        Vec::from([QueueRegistration { name, kinds }])
    }

    #[test]
    fn configured_queues_are_selected_from_the_registry() {
        let registry = registered("whio.jobs.probe", &[handler]);
        let selected = check_queues(&["whio.jobs.probe".to_owned()], &registry).unwrap();

        assert_eq!(
            selected.iter().map(|queue| queue.name).collect::<Vec<_>>(),
            vec!["whio.jobs.probe"]
        );
    }

    #[test]
    fn an_unknown_configured_queue_is_rejected_and_named() {
        let registry = registered("whio.jobs.probe", &[handler]);

        let error = check_queues(&["whio.jobs.other".to_owned()], &registry).unwrap_err();

        assert!(matches!(
            &error,
            WorkerError::UnknownQueue { queue } if queue == "whio.jobs.other"
        ));
        assert!(error.to_string().contains("whio.jobs.other"));
    }

    #[test]
    fn a_configured_queue_without_a_handler_is_rejected_and_named() {
        let registry = registered("whio.jobs.probe", &[]);

        let error = check_queues(&["whio.jobs.probe".to_owned()], &registry).unwrap_err();

        assert!(matches!(
            &error,
            WorkerError::UnregisteredQueue { queue } if queue == "whio.jobs.probe"
        ));
        assert!(error.to_string().contains("whio.jobs.probe"));
    }

    #[test]
    fn an_absent_queue_set_is_rejected() {
        let registry = registered("whio.jobs.probe", &[handler]);

        assert!(matches!(
            check_queues(&[], &registry),
            Err(WorkerError::MissingQueues)
        ));
    }

    #[test]
    fn the_registry_declares_the_probe_queue_with_a_handler() {
        let queues = jobs::queues();

        assert_eq!(queues.len(), 1);
        assert_eq!(queues[0].name, jobs::probe::PROBE_QUEUE);
        assert!(!queues[0].kinds.is_empty());
    }
}
