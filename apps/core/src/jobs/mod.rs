pub mod listening_delivery;
pub mod metadata_refresh;
pub mod probe;

use awa::{ClientBuilder, QueueConfig};

pub(crate) type KindRegistration = fn(ClientBuilder) -> ClientBuilder;

#[derive(Debug)]
pub(crate) struct QueueRegistration {
    pub(crate) name: &'static str,
    pub(crate) kinds: &'static [KindRegistration],
}

pub(crate) fn queues() -> Vec<QueueRegistration> {
    vec![
        QueueRegistration {
            name: probe::PROBE_QUEUE,
            kinds: &[probe::register],
        },
        QueueRegistration {
            name: metadata_refresh::METADATA_REFRESH_QUEUE,
            kinds: &[metadata_refresh::register],
        },
        QueueRegistration {
            name: listening_delivery::LISTEN_DELIVERY_QUEUE,
            kinds: &[listening_delivery::register],
        },
    ]
}

pub(crate) fn register(
    builder: ClientBuilder,
    queues: &[&QueueRegistration],
    config: &QueueConfig,
) -> ClientBuilder {
    queues.iter().fold(builder, |builder, queue| {
        queue.kinds.iter().fold(
            builder.queue(queue.name, config.clone()),
            |builder, kind| kind(builder),
        )
    })
}
