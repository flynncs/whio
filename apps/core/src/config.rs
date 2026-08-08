use std::{net::SocketAddr, time::Duration};

use reqwest::Url;

pub(crate) struct Config {
    pub(crate) bind_address: SocketAddr,
    pub(crate) log_level: tracing::Level,
    pub(crate) resolver_url: Url,
    pub(crate) resolver_connect_timeout: Duration,
    pub(crate) resolver_total_timeout: Duration,
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum ConfigError {
    #[error("invalid {name}: `{value}`")]
    Invalid { name: &'static str, value: String },
}

impl Config {
    pub(crate) fn from_env() -> Result<Config, ConfigError> {
        let bind_raw =
            std::env::var("WHIO_BIND_ADDRESS").unwrap_or_else(|_| "127.0.0.1:3000".to_owned());

        let bind_address = bind_raw
            .parse::<SocketAddr>()
            .map_err(|_| ConfigError::Invalid {
                name: "WHIO_BIND_ADDRESS",
                value: bind_raw.clone(),
            })?;

        let log_raw = std::env::var("WHIO_LOG_LEVEL").unwrap_or_else(|_| "info".to_owned());

        let log_level = log_raw
            .parse::<tracing::Level>()
            .map_err(|_| ConfigError::Invalid {
                name: "WHIO_LOG_LEVEL",
                value: log_raw.clone(),
            })?;

        let resolver_raw = std::env::var("WHIO_RESOLVER_URL")
            .unwrap_or_else(|_| "http://127.0.0.1:8000".to_owned());

        let resolver_url = resolver_raw
            .parse::<Url>()
            .map_err(|_| ConfigError::Invalid {
                name: "WHIO_RESOLVER_URL",
                value: resolver_raw,
            })?;

        Ok(Config {
            bind_address,
            log_level,
            resolver_url,
            resolver_connect_timeout: Duration::from_secs(2),
            resolver_total_timeout: Duration::from_secs(10),
        })
    }
}
