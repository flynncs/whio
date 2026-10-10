use tracing_subscriber::prelude::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LogFormat {
    Text,
    Json,
}

#[derive(Clone, Debug)]
pub(crate) struct TelemetryConfig {
    pub(crate) format: LogFormat,
    pub(crate) filter: String,
}

pub(crate) fn init(config: &TelemetryConfig) {
    let filter = tracing_subscriber::EnvFilter::new(&config.filter);
    match config.format {
        LogFormat::Text => tracing_subscriber::registry()
            .with(filter)
            .with(tracing_subscriber::fmt::layer())
            .init(),
        LogFormat::Json => tracing_subscriber::registry()
            .with(filter)
            .with(tracing_subscriber::fmt::layer().json())
            .init(),
    }
}
