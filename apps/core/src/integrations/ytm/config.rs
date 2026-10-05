use std::time::Duration;

use reqwest::Url;

use crate::platform::config::{ConfigError, Vars, invalid, parse_timeout};

const YOUTUBE_ENABLED: &str = "WHIO_YOUTUBE_ENABLED";
const RESOLVER_URL: &str = "WHIO_YOUTUBE_RESOLVER_URL";
const CONNECT_TIMEOUT: &str = "WHIO_YOUTUBE_RESOLVER_CONNECT_TIMEOUT_SECONDS";
const TOTAL_TIMEOUT: &str = "WHIO_YOUTUBE_RESOLVER_TOTAL_TIMEOUT_SECONDS";
pub const DEFAULT_CONNECT_TIMEOUT_SECONDS: u64 = 2;
pub const DEFAULT_TOTAL_TIMEOUT_SECONDS: u64 = 35;

#[derive(Debug, Clone)]
pub struct Config {
    pub url: Url,
    pub connect_timeout: Duration,
    pub total_timeout: Duration,
}

pub(crate) fn from_vars(vars: &Vars) -> Result<Option<Config>, ConfigError> {
    let enabled = match vars.get(YOUTUBE_ENABLED) {
        Some(raw) => raw
            .parse()
            .map_err(|_| invalid(YOUTUBE_ENABLED, raw, "must be true or false"))?,
        None => false,
    };
    if !enabled {
        return Ok(None);
    }

    let raw_url = vars
        .get(RESOLVER_URL)
        .ok_or(ConfigError::Missing { name: RESOLVER_URL })?;
    let url = raw_url
        .parse::<Url>()
        .map_err(|_| invalid(RESOLVER_URL, raw_url, "must be an absolute HTTP(S) URL"))?;
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
        return Err(invalid(
            RESOLVER_URL,
            raw_url,
            "must be an absolute HTTP(S) URL",
        ));
    }
    let connect_timeout = parse_timeout(vars, CONNECT_TIMEOUT, DEFAULT_CONNECT_TIMEOUT_SECONDS)?;
    let total_timeout = parse_timeout(vars, TOTAL_TIMEOUT, DEFAULT_TOTAL_TIMEOUT_SECONDS)?;
    if total_timeout < Duration::from_secs(DEFAULT_TOTAL_TIMEOUT_SECONDS) {
        return Err(invalid(
            TOTAL_TIMEOUT,
            &total_timeout.as_secs().to_string(),
            "must be at least 35 seconds",
        ));
    }
    if total_timeout <= connect_timeout {
        return Err(invalid(
            TOTAL_TIMEOUT,
            &total_timeout.as_secs().to_string(),
            "must be greater than the connect timeout",
        ));
    }
    Ok(Some(Config {
        url,
        connect_timeout,
        total_timeout,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vars(entries: &[(&str, &str)]) -> Vars {
        entries
            .iter()
            .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
            .collect()
    }

    #[test]
    fn defaults_to_disabled_and_ignores_inactive_values() {
        assert!(
            from_vars(&vars(&[
                (RESOLVER_URL, "not a url"),
                (CONNECT_TIMEOUT, "not a timeout"),
                (TOTAL_TIMEOUT, "1"),
            ]))
            .unwrap()
            .is_none()
        );
    }

    #[test]
    fn enabled_configuration_uses_defaults_and_accepts_overrides() {
        let config = from_vars(&vars(&[
            (YOUTUBE_ENABLED, "true"),
            (RESOLVER_URL, "http://resolver.example.test"),
        ]))
        .unwrap()
        .unwrap();
        assert_eq!(config.connect_timeout, Duration::from_secs(2));
        assert_eq!(config.total_timeout, Duration::from_secs(35));

        let config = from_vars(&vars(&[
            (YOUTUBE_ENABLED, "true"),
            (RESOLVER_URL, "https://resolver.example.test/v1"),
            (CONNECT_TIMEOUT, "4"),
            (TOTAL_TIMEOUT, "45"),
        ]))
        .unwrap()
        .unwrap();
        assert_eq!(config.url.as_str(), "https://resolver.example.test/v1");
        assert_eq!(config.connect_timeout, Duration::from_secs(4));
        assert_eq!(config.total_timeout, Duration::from_secs(45));
    }

    #[test]
    fn enabled_configuration_requires_a_valid_url() {
        assert!(from_vars(&vars(&[(YOUTUBE_ENABLED, "true")])).is_err());
        for url in ["resolver.example.test", "file:///tmp/resolver", "http://"] {
            assert!(from_vars(&vars(&[(YOUTUBE_ENABLED, "true"), (RESOLVER_URL, url),])).is_err());
        }
    }

    #[test]
    fn enabled_configuration_rejects_invalid_values() {
        assert!(from_vars(&vars(&[(YOUTUBE_ENABLED, "yes")])).is_err());
        for entries in [
            vec![
                (YOUTUBE_ENABLED, "true"),
                (RESOLVER_URL, "http://resolver.example.test"),
                (CONNECT_TIMEOUT, "0"),
            ],
            vec![
                (YOUTUBE_ENABLED, "true"),
                (RESOLVER_URL, "http://resolver.example.test"),
                (TOTAL_TIMEOUT, "34"),
            ],
            vec![
                (YOUTUBE_ENABLED, "true"),
                (RESOLVER_URL, "http://resolver.example.test"),
                (CONNECT_TIMEOUT, "40"),
                (TOTAL_TIMEOUT, "35"),
            ],
        ] {
            assert!(from_vars(&vars(&entries)).is_err());
        }
    }
}
