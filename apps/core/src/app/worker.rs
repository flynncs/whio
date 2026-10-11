use std::sync::Arc;
use std::time::Duration;

use axum::{Json, Router, extract::State, http::StatusCode, response::IntoResponse, routing::get};
use serde::Serialize;
use sqlx::PgPool;
use thiserror::Error;
use tokio::net::TcpListener;

use crate::{
    app::config::{Config, ConfigError, WorkerConfig},
    app::database::{Database, DatabaseError},
    app::startup::{WorkerState, shutdown_signal},
    jobs::{self, QueueRegistration},
};

#[derive(Debug, Error)]
pub(crate) enum WorkerError {
    #[error(transparent)]
    Config(#[from] ConfigError),

    #[error("missing required WHIO_WORKER_QUEUES")]
    MissingQueues,

    #[error("unknown queue `{queue}` in WHIO_WORKER_QUEUES")]
    UnknownQueue { queue: String },

    #[error("no job handler is registered for queue `{queue}`")]
    UnregisteredQueue { queue: String },

    #[error("queue `{queue}` requires {dependency}, which is not configured")]
    UnavailableDependency {
        queue: String,
        dependency: &'static str,
    },

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

    #[error("a required worker dependency could not be constructed")]
    Dependency(#[source] crate::features::playback::providers::ProviderError),
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

    crate::platform::telemetry::init(&config.telemetry);

    let worker = config.worker.clone();
    let registered = jobs::queues();
    let queues = check_queues(&worker.queues, &registered)?;
    check_queue_dependencies(&queues, &config.integrations, config.acquisition.enabled)?;
    let database = Database::connect(&config.database_url).await?;
    let state =
        super::startup::worker_state(&config, database.pool()).map_err(WorkerError::Dependency)?;
    let client = build(&database.pool(), &queues, &worker, &config, state)?;

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

fn check_queue_dependencies(
    queues: &[&QueueRegistration],
    integrations: &crate::integrations::IntegrationsConfig,
    acquisition_enabled: bool,
) -> Result<(), WorkerError> {
    for queue in queues {
        if queue.name == jobs::metadata_refresh::METADATA_REFRESH_QUEUE
            && !integrations.has_document_provider()
        {
            return Err(WorkerError::UnavailableDependency {
                queue: queue.name.to_owned(),
                dependency: "a metadata document provider (WHIO_MUSICBRAINZ_ENABLED, WHIO_AUDIODB_ENABLED, WHIO_LISTENBRAINZ_METADATA_ENABLED or WHIO_YOUTUBE_ENABLED)",
            });
        }

        if queue.name == jobs::acquisition::ACQUISITION_QUEUE && !acquisition_enabled {
            return Err(WorkerError::UnavailableDependency {
                queue: queue.name.to_owned(),
                dependency: "WHIO_ACQUISITION_ENABLED",
            });
        }
    }

    Ok(())
}

fn queue_config(
    worker: &WorkerConfig,
    queue: &QueueRegistration,
    beets_deadline: Option<Duration>,
) -> awa::QueueConfig {
    awa::QueueConfig {
        max_workers: worker.max_workers,
        poll_interval: worker.poll_interval,
        deadline_duration: queue_deadline(queue.name, beets_deadline),
        ..awa::QueueConfig::default()
    }
}

fn queue_deadline(name: &str, beets_deadline: Option<Duration>) -> Duration {
    match name {
        jobs::listening_delivery::LISTEN_DELIVERY_QUEUE => {
            jobs::listening_delivery::LISTEN_DELIVERY_DEADLINE
        }
        jobs::acquisition::ACQUISITION_QUEUE => beets_deadline
            .map(|deadline| deadline + Duration::from_secs(120))
            .unwrap_or_else(|| awa::QueueConfig::default().deadline_duration),
        _ => awa::QueueConfig::default().deadline_duration,
    }
}

fn build(
    pool: &PgPool,
    queues: &[&QueueRegistration],
    worker: &WorkerConfig,
    config: &Config,
    state: WorkerState,
) -> Result<awa::Client, WorkerError> {
    let mut builder = awa::Client::builder(pool.clone())
        .promote_interval(worker.promote_interval)
        .heartbeat_rescue_interval(worker.heartbeat_rescue_interval)
        .state(Arc::clone(&state.metadata));
    if let Some(interval) = worker.heartbeat_interval {
        builder = builder.heartbeat_interval(interval);
    }
    if let Some(staleness) = worker.heartbeat_staleness {
        builder = builder.heartbeat_staleness(staleness);
    }

    if let Some(subjects) = state.subjects.as_ref() {
        builder = builder.state(Arc::clone(subjects));
    }
    if let Some(delivery) = state.delivery.as_ref() {
        builder = builder.state(Arc::clone(delivery));
    }
    if let Some(acquisition) = state.acquisition.as_ref() {
        builder = builder.state(Arc::clone(acquisition));
    }

    queues
        .iter()
        .fold(builder, |builder, queue| {
            jobs::register(
                builder,
                std::slice::from_ref(queue),
                &queue_config(
                    worker,
                    queue,
                    config
                        .integrations
                        .beets
                        .as_ref()
                        .map(|beets| beets.deadline),
                ),
                worker.retry_policy,
            )
        })
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

    fn handler(
        builder: awa::ClientBuilder,
        _retry_policy: jobs::RetryPolicy,
    ) -> awa::ClientBuilder {
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
    fn the_registry_declares_every_queue_with_a_handler() {
        let queues = jobs::queues();
        let names = queues.iter().map(|queue| queue.name).collect::<Vec<_>>();

        assert_eq!(
            names,
            vec![
                jobs::probe::PROBE_QUEUE,
                jobs::metadata_refresh::METADATA_REFRESH_QUEUE,
                jobs::listening_delivery::LISTEN_DELIVERY_QUEUE,
                jobs::acquisition::ACQUISITION_QUEUE,
            ]
        );
        assert!(queues.iter().all(|queue| !queue.kinds.is_empty()));
    }

    fn registry_queue(name: &str) -> QueueRegistration {
        let queues = jobs::queues();

        queues
            .into_iter()
            .find(|queue| queue.name == name)
            .expect("the queue is registered")
    }

    fn enabled_integrations() -> crate::integrations::IntegrationsConfig {
        crate::integrations::IntegrationsConfig {
            ytm: None,
            musicbrainz: Some(crate::integrations::musicbrainz::config::Config {
                user_agent: "test".to_owned(),
                wikimedia_user_agent: "test".to_owned(),
            }),
            audiodb: None,
            listenbrainz_documents: None,
            slskd: None,
            beets: None,
        }
    }

    #[test]
    fn a_metadata_refresh_queue_without_the_resolver_is_refused_and_named() {
        let queue = registry_queue(jobs::metadata_refresh::METADATA_REFRESH_QUEUE);

        let error = check_queue_dependencies(
            &[&queue],
            &crate::integrations::IntegrationsConfig {
                ytm: None,
                musicbrainz: None,
                audiodb: None,
                listenbrainz_documents: None,
                slskd: None,
                beets: None,
            },
            false,
        )
        .expect_err("a metadata refresh queue needs the metadata transport");

        assert!(matches!(
            &error,
            WorkerError::UnavailableDependency { queue, dependency }
                if queue == jobs::metadata_refresh::METADATA_REFRESH_QUEUE
                    && *dependency == "a metadata document provider (WHIO_MUSICBRAINZ_ENABLED, WHIO_AUDIODB_ENABLED, WHIO_LISTENBRAINZ_METADATA_ENABLED or WHIO_YOUTUBE_ENABLED)"
        ));
        assert!(error.to_string().contains("WHIO_MUSICBRAINZ_ENABLED"));
    }

    #[test]
    fn a_metadata_refresh_queue_with_the_resolver_and_a_probe_queue_are_accepted() {
        let metadata = registry_queue(jobs::metadata_refresh::METADATA_REFRESH_QUEUE);
        let probe = registry_queue(jobs::probe::PROBE_QUEUE);

        assert!(check_queue_dependencies(&[&metadata], &enabled_integrations(), false).is_ok());
        assert!(
            check_queue_dependencies(
                &[&probe],
                &crate::integrations::IntegrationsConfig {
                    ytm: None,
                    musicbrainz: None,
                    audiodb: None,
                    listenbrainz_documents: None,
                    slskd: None,
                    beets: None,
                },
                false,
            )
            .is_ok()
        );
    }

    #[test]
    fn the_acquisition_queue_requires_acquisition() {
        let queue = registry_queue(jobs::acquisition::ACQUISITION_QUEUE);

        let error = check_queue_dependencies(&[&queue], &enabled_integrations(), false)
            .expect_err("the acquisition queue needs acquisition");

        assert!(matches!(
            &error,
            WorkerError::UnavailableDependency { queue, dependency }
                if queue == jobs::acquisition::ACQUISITION_QUEUE
                    && *dependency == "WHIO_ACQUISITION_ENABLED"
        ));
        assert!(
            check_queue_dependencies(&[&queue], &enabled_integrations(), true).is_ok(),
            "an enabled acquisition accepts the queue"
        );
    }

    #[test]
    fn the_acquisition_queue_deadline_covers_the_beets_run() {
        let deadline = queue_deadline(
            jobs::acquisition::ACQUISITION_QUEUE,
            Some(Duration::from_secs(600)),
        );

        assert_eq!(deadline, Duration::from_secs(600 + 120));
    }

    #[test]
    fn the_listen_delivery_queue_deadline_is_explicit_and_inside_the_handler_window() {
        let worker = WorkerConfig {
            queues: Vec::new(),
            bind_address: "127.0.0.1:0".parse().unwrap(),
            max_workers: 2,
            poll_interval: Duration::from_millis(200),
            promote_interval: Duration::from_millis(250),
            drain_timeout: Duration::from_secs(30),
            heartbeat_rescue_interval: Duration::from_secs(30),
            heartbeat_interval: None,
            heartbeat_staleness: None,
            retry_policy: jobs::RetryPolicy::default(),
        };
        let delivery = registry_queue(jobs::listening_delivery::LISTEN_DELIVERY_QUEUE);
        let probe = registry_queue(jobs::probe::PROBE_QUEUE);

        let configured = queue_config(&worker, &delivery, None);

        assert_eq!(
            configured.deadline_duration,
            jobs::listening_delivery::LISTEN_DELIVERY_DEADLINE
        );
        assert!(
            configured.deadline_duration
                < crate::features::music_library::delivery::SNOOZE_DEADLINE
        );
        assert_eq!(
            queue_config(&worker, &probe, None).deadline_duration,
            awa::QueueConfig::default().deadline_duration
        );
    }
}
