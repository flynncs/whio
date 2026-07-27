use std::net::SocketAddr;

pub(crate) struct Config {
    pub(crate) bind_address: SocketAddr,
    pub(crate) log_level: tracing::Level,
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

        Ok(Config {
            bind_address,
            log_level,
        })
    }
}
