use std::net::SocketAddr;

#[derive(Debug, Clone)]
pub(crate) struct Config {
    pub(crate) bind_address: SocketAddr,
    pub(crate) log_level: tracing::Level,
}

impl Config {
    pub(crate) fn from_env() -> Result<Self, String> {
        let bind_address = env_or("WHIO_ND_CONNECTOR_BIND_ADDRESS", "127.0.0.1:3001")?
            .parse()
            .map_err(|_| {
                "WHIO_ND_CONNECTOR_BIND_ADDRESS is not a valid socket address".to_owned()
            })?;
        let log_level = match env_or("WHIO_ND_CONNECTOR_LOG_LEVEL", "info")?.as_str() {
            "error" => tracing::Level::ERROR,
            "warn" => tracing::Level::WARN,
            "info" => tracing::Level::INFO,
            "debug" => tracing::Level::DEBUG,
            "trace" => tracing::Level::TRACE,
            _ => return Err("WHIO_ND_CONNECTOR_LOG_LEVEL is not a valid log level".to_owned()),
        };
        Ok(Self {
            bind_address,
            log_level,
        })
    }
}

fn env_or(name: &str, default: &str) -> Result<String, String> {
    match std::env::var(name) {
        Ok(value) if !value.trim().is_empty() => Ok(value),
        Ok(_) => Err(format!("{name} must not be blank")),
        Err(std::env::VarError::NotPresent) => Ok(default.to_owned()),
        Err(_) => Err(format!("{name} is not valid unicode")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_apply_without_environment() {
        for (name, _) in [
            ("WHIO_ND_CONNECTOR_BIND_ADDRESS", ""),
            ("WHIO_ND_CONNECTOR_LOG_LEVEL", ""),
        ] {
            unsafe { std::env::remove_var(name) };
        }
        let config = Config::from_env().unwrap();
        assert_eq!(config.bind_address.to_string(), "127.0.0.1:3001");
        assert_eq!(config.log_level, tracing::Level::INFO);
    }
}
