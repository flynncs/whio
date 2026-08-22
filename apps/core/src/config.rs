use std::{collections::HashMap, net::SocketAddr, time::Duration};

use reqwest::Url;

const DEFAULT_BIND_ADDRESS: &str = "127.0.0.1:3000";
const DEFAULT_LOG_LEVEL: &str = "info";
const DEFAULT_YOUTUBE_CONNECT_TIMEOUT_SECONDS: u64 = 2;
const DEFAULT_YOUTUBE_TOTAL_TIMEOUT_SECONDS: u64 = 35;

const DATABASE_URL: &str = "WHIO_DATABASE_URL";
const YOUTUBE_ENABLED: &str = "WHIO_YOUTUBE_ENABLED";
const YOUTUBE_RESOLVER_URL: &str = "WHIO_YOUTUBE_RESOLVER_URL";
const YOUTUBE_RESOLVER_CONNECT_TIMEOUT: &str = "WHIO_YOUTUBE_RESOLVER_CONNECT_TIMEOUT_SECONDS";
const YOUTUBE_RESOLVER_TOTAL_TIMEOUT: &str = "WHIO_YOUTUBE_RESOLVER_TOTAL_TIMEOUT_SECONDS";

#[derive(Debug)]
pub(crate) struct Config {
    pub(crate) bind_address: SocketAddr,
    pub(crate) database_url: String,
    pub(crate) log_level: tracing::Level,
    pub(crate) youtube_resolver: YoutubeResolverConfig,
}

#[derive(Debug)]
pub(crate) enum YoutubeResolverConfig {
    Disabled,
    Enabled {
        url: Url,
        connect_timeout: Duration,
        total_timeout: Duration,
    },
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum ConfigError {
    #[error("invalid {name}: `{value}` ({reason})")]
    Invalid {
        name: &'static str,
        value: String,
        reason: &'static str,
    },

    #[error("missing required {name}")]
    Missing { name: &'static str },

    #[error("invalid {name} ({reason})")]
    InvalidDatabaseUrl {
        name: &'static str,
        reason: &'static str,
    },
}

impl Config {
    pub(crate) fn from_env() -> Result<Self, ConfigError> {
        let vars = std::env::vars().collect::<HashMap<_, _>>();
        Self::from_map(&vars)
    }

    fn from_map(vars: &HashMap<String, String>) -> Result<Self, ConfigError> {
        let bind_raw = vars
            .get("WHIO_BIND_ADDRESS")
            .map(String::as_str)
            .unwrap_or(DEFAULT_BIND_ADDRESS);
        let bind_address = bind_raw
            .parse()
            .map_err(|_| invalid("WHIO_BIND_ADDRESS", bind_raw, "must be a socket address"))?;

        let database_raw = vars
            .get(DATABASE_URL)
            .ok_or(ConfigError::Missing { name: DATABASE_URL })?;
        let database_url =
            database_raw
                .parse::<Url>()
                .map_err(|_| ConfigError::InvalidDatabaseUrl {
                    name: DATABASE_URL,
                    reason: "must be a valid PostgreSQL URL",
                })?;
        if !matches!(database_url.scheme(), "postgres" | "postgresql")
            || database_url.host_str().is_none()
        {
            return Err(ConfigError::InvalidDatabaseUrl {
                name: DATABASE_URL,
                reason: "must be a PostgreSQL URL with a host",
            });
        }

        let log_raw = vars
            .get("WHIO_LOG_LEVEL")
            .map(String::as_str)
            .unwrap_or(DEFAULT_LOG_LEVEL);
        let log_level = log_raw
            .parse()
            .map_err(|_| invalid("WHIO_LOG_LEVEL", log_raw, "must be a tracing level"))?;

        let enabled = match vars.get(YOUTUBE_ENABLED) {
            Some(raw) => raw
                .parse()
                .map_err(|_| invalid(YOUTUBE_ENABLED, raw, "must be true or false"))?,
            None => false,
        };

        let youtube_resolver = if enabled {
            let raw_url = vars.get(YOUTUBE_RESOLVER_URL).ok_or(ConfigError::Missing {
                name: YOUTUBE_RESOLVER_URL,
            })?;
            let url = raw_url.parse::<Url>().map_err(|_| {
                invalid(
                    YOUTUBE_RESOLVER_URL,
                    raw_url,
                    "must be an absolute HTTP(S) URL",
                )
            })?;

            if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
                return Err(invalid(
                    YOUTUBE_RESOLVER_URL,
                    raw_url,
                    "must be an absolute HTTP(S) URL",
                ));
            }

            let connect_timeout = parse_timeout(
                vars,
                YOUTUBE_RESOLVER_CONNECT_TIMEOUT,
                DEFAULT_YOUTUBE_CONNECT_TIMEOUT_SECONDS,
            )?;
            let total_timeout = parse_timeout(
                vars,
                YOUTUBE_RESOLVER_TOTAL_TIMEOUT,
                DEFAULT_YOUTUBE_TOTAL_TIMEOUT_SECONDS,
            )?;

            if total_timeout < Duration::from_secs(DEFAULT_YOUTUBE_TOTAL_TIMEOUT_SECONDS) {
                return Err(invalid(
                    YOUTUBE_RESOLVER_TOTAL_TIMEOUT,
                    &total_timeout.as_secs().to_string(),
                    "must be at least 35 seconds",
                ));
            }
            if total_timeout <= connect_timeout {
                return Err(invalid(
                    YOUTUBE_RESOLVER_TOTAL_TIMEOUT,
                    &total_timeout.as_secs().to_string(),
                    "must be greater than the connect timeout",
                ));
            }

            YoutubeResolverConfig::Enabled {
                url,
                connect_timeout,
                total_timeout,
            }
        } else {
            YoutubeResolverConfig::Disabled
        };

        Ok(Self {
            bind_address,
            database_url: database_raw.to_owned(),
            log_level,
            youtube_resolver,
        })
    }
}

fn parse_timeout(
    vars: &HashMap<String, String>,
    name: &'static str,
    default_seconds: u64,
) -> Result<Duration, ConfigError> {
    let default = default_seconds.to_string();
    let raw = vars.get(name).map(String::as_str).unwrap_or(&default);
    let seconds = raw
        .parse::<u64>()
        .map_err(|_| invalid(name, raw, "must be a positive integer number of seconds"))?;

    if seconds == 0 {
        return Err(invalid(
            name,
            raw,
            "must be a positive integer number of seconds",
        ));
    }

    Ok(Duration::from_secs(seconds))
}

fn invalid(name: &'static str, value: &str, reason: &'static str) -> ConfigError {
    ConfigError::Invalid {
        name,
        value: value.to_owned(),
        reason,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_config(values: &[(&str, &str)]) -> Result<Config, ConfigError> {
        let mut vars: HashMap<String, String> = values
            .iter()
            .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
            .collect();
        vars.entry(DATABASE_URL.to_owned())
            .or_insert_with(|| "postgres://whio:whio-dev@localhost/whio".to_owned());
        Config::from_map(&vars)
    }

    #[test]
    fn database_configuration_is_required() {
        let vars = HashMap::new();

        assert!(matches!(
            Config::from_map(&vars),
            Err(ConfigError::Missing { name: DATABASE_URL })
        ));
    }

    #[test]
    fn database_configuration_requires_a_postgresql_url() {
        for value in ["not a url", "http://localhost/whio", "postgres://"] {
            let mut vars = HashMap::new();
            vars.insert(DATABASE_URL.to_owned(), value.to_owned());

            assert!(matches!(
                Config::from_map(&vars),
                Err(ConfigError::InvalidDatabaseUrl {
                    name: DATABASE_URL,
                    ..
                }),
            ));
        }
    }

    #[test]
    fn defaults_to_disabled_and_ignores_inactive_values() {
        let config = parse_config(&[
            (YOUTUBE_RESOLVER_URL, "not a url"),
            (YOUTUBE_RESOLVER_CONNECT_TIMEOUT, "not a timeout"),
            (YOUTUBE_RESOLVER_TOTAL_TIMEOUT, "1"),
        ])
        .unwrap();

        assert_eq!(config.bind_address, "127.0.0.1:3000".parse().unwrap());
        assert_eq!(config.log_level, tracing::Level::INFO);
        assert!(matches!(
            config.youtube_resolver,
            YoutubeResolverConfig::Disabled
        ));
    }

    #[test]
    fn enabled_configuration_uses_defaults_and_accepts_overrides() {
        let config = parse_config(&[
            (YOUTUBE_ENABLED, "true"),
            (YOUTUBE_RESOLVER_URL, "http://resolver.example.test"),
        ])
        .unwrap();
        assert!(matches!(
            config.youtube_resolver,
            YoutubeResolverConfig::Enabled {
                connect_timeout,
                total_timeout,
                ..
            } if connect_timeout == Duration::from_secs(2)
                && total_timeout == Duration::from_secs(35)
        ));

        let config = parse_config(&[
            (YOUTUBE_ENABLED, "true"),
            (YOUTUBE_RESOLVER_URL, "https://resolver.example.test/v1"),
            (YOUTUBE_RESOLVER_CONNECT_TIMEOUT, "4"),
            (YOUTUBE_RESOLVER_TOTAL_TIMEOUT, "45"),
        ])
        .unwrap();
        assert!(matches!(
            config.youtube_resolver,
            YoutubeResolverConfig::Enabled {
                url,
                connect_timeout,
                total_timeout,
            } if url.as_str() == "https://resolver.example.test/v1"
                && connect_timeout == Duration::from_secs(4)
                && total_timeout == Duration::from_secs(45)
        ));
    }

    #[test]
    fn enabled_configuration_requires_a_valid_url() {
        assert!(parse_config(&[(YOUTUBE_ENABLED, "true")]).is_err());
        for url in ["resolver.example.test", "file:///tmp/resolver", "http://"] {
            assert!(
                parse_config(&[(YOUTUBE_ENABLED, "true"), (YOUTUBE_RESOLVER_URL, url)]).is_err()
            );
        }
    }

    #[test]
    fn enabled_configuration_rejects_invalid_values() {
        assert!(parse_config(&[(YOUTUBE_ENABLED, "yes")]).is_err());
        assert!(
            parse_config(&[
                (YOUTUBE_ENABLED, "true"),
                (YOUTUBE_RESOLVER_URL, "http://resolver.example.test"),
                (YOUTUBE_RESOLVER_CONNECT_TIMEOUT, "0"),
            ])
            .is_err()
        );
        assert!(
            parse_config(&[
                (YOUTUBE_ENABLED, "true"),
                (YOUTUBE_RESOLVER_URL, "http://resolver.example.test"),
                (YOUTUBE_RESOLVER_TOTAL_TIMEOUT, "34"),
            ])
            .is_err()
        );
        assert!(
            parse_config(&[
                (YOUTUBE_ENABLED, "true"),
                (YOUTUBE_RESOLVER_URL, "http://resolver.example.test"),
                (YOUTUBE_RESOLVER_CONNECT_TIMEOUT, "40"),
                (YOUTUBE_RESOLVER_TOTAL_TIMEOUT, "35"),
            ])
            .is_err()
        );
    }
}
