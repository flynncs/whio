use std::time::Duration;

use reqwest::Url;

use crate::platform::config::{ConfigError, Vars, invalid, parse_timeout, secret};

const ENABLED: &str = "WHIO_SLSKD_ENABLED";
const URL: &str = "WHIO_SLSKD_URL";
const API_KEY: &str = "WHIO_SLSKD_API_KEY";
const API_KEY_FILE: &str = "WHIO_SLSKD_API_KEY_FILE";
const SEARCH_TIMEOUT: &str = "WHIO_SLSKD_SEARCH_TIMEOUT_SECONDS";

const DEFAULT_SEARCH_TIMEOUT_SECONDS: u64 = 15;
const MIN_SEARCH_TIMEOUT_SECONDS: u64 = 1;
const MAX_SEARCH_TIMEOUT_SECONDS: u64 = 120;

#[derive(Debug, Clone)]
pub struct Config {
    pub url: Url,
    pub api_key: String,
    pub search_timeout: Duration,
}

pub(crate) fn from_vars(vars: &Vars) -> Result<Option<Config>, ConfigError> {
    let enabled = match vars.get(ENABLED) {
        None => false,
        Some(raw) => match raw.trim() {
            "true" => true,
            "false" => false,
            _ => return Err(invalid(ENABLED, raw, "must be true or false")),
        },
    };

    if !enabled {
        return Ok(None);
    }

    let raw = vars.get(URL).ok_or(ConfigError::Missing { name: URL })?;
    let url = raw
        .parse::<Url>()
        .map_err(|_| invalid(URL, raw, "must be an absolute HTTP(S) URL"))?;
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
        return Err(invalid(URL, raw, "must be an absolute HTTP(S) URL"));
    }

    let api_key =
        secret(vars, API_KEY, API_KEY_FILE)?.ok_or(ConfigError::Missing { name: API_KEY })?;

    let search_timeout = parse_timeout(vars, SEARCH_TIMEOUT, DEFAULT_SEARCH_TIMEOUT_SECONDS)?;
    if !(MIN_SEARCH_TIMEOUT_SECONDS..=MAX_SEARCH_TIMEOUT_SECONDS)
        .contains(&search_timeout.as_secs())
    {
        return Err(invalid(
            SEARCH_TIMEOUT,
            &search_timeout.as_secs().to_string(),
            "must be between 1 and 120 seconds",
        ));
    }

    Ok(Some(Config {
        url,
        api_key,
        search_timeout,
    }))
}
