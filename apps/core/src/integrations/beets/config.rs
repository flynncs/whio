use std::{path::PathBuf, time::Duration};

use crate::platform::config::{ConfigError, Vars, invalid, parse_timeout};

const ACQUISITION_ENABLED: &str = "WHIO_ACQUISITION_ENABLED";
const COMMAND: &str = "WHIO_BEETS_COMMAND";
const CONFIG: &str = "WHIO_BEETS_CONFIG";
const DEADLINE: &str = "WHIO_BEETS_DEADLINE_SECONDS";
const STALL: &str = "WHIO_BEETS_STALL_SECONDS";

const DEFAULT_COMMAND: &str = "beet";
const DEFAULT_CONFIG: &str = "/etc/whio/beets.yaml";
const DEFAULT_DEADLINE_SECONDS: u64 = 600;
const DEFAULT_STALL_SECONDS: u64 = 120;
const MIN_DEADLINE_SECONDS: u64 = 30;
const MAX_DEADLINE_SECONDS: u64 = 7200;
const MIN_STALL_SECONDS: u64 = 5;
const MAX_STALL_SECONDS: u64 = 3600;

#[derive(Debug, Clone)]
pub struct Config {
    pub command: String,
    pub config: PathBuf,
    pub deadline: Duration,
    pub stall: Duration,
}

pub(crate) fn from_vars(vars: &Vars) -> Result<Option<Config>, ConfigError> {
    let enabled = match vars.get(ACQUISITION_ENABLED) {
        None => false,
        Some(raw) => match raw.trim() {
            "true" => true,
            "false" => false,
            _ => return Err(invalid(ACQUISITION_ENABLED, raw, "must be true or false")),
        },
    };

    if !enabled {
        return Ok(None);
    }

    let command = vars
        .get(COMMAND)
        .map(|raw| raw.trim())
        .filter(|raw| !raw.is_empty())
        .unwrap_or(DEFAULT_COMMAND)
        .to_owned();
    let config = PathBuf::from(
        vars.get(CONFIG)
            .map(String::as_str)
            .unwrap_or(DEFAULT_CONFIG),
    );
    let deadline = bounded(
        vars,
        DEADLINE,
        DEFAULT_DEADLINE_SECONDS,
        MIN_DEADLINE_SECONDS,
        MAX_DEADLINE_SECONDS,
    )?;
    let stall = bounded(
        vars,
        STALL,
        DEFAULT_STALL_SECONDS,
        MIN_STALL_SECONDS,
        MAX_STALL_SECONDS,
    )?;

    Ok(Some(Config {
        command,
        config,
        deadline,
        stall,
    }))
}

fn bounded(
    vars: &Vars,
    name: &'static str,
    default_seconds: u64,
    min: u64,
    max: u64,
) -> Result<Duration, ConfigError> {
    let value = parse_timeout(vars, name, default_seconds)?;

    if !(min..=max).contains(&value.as_secs()) {
        return Err(invalid(
            name,
            &value.as_secs().to_string(),
            "must be within the allowed range",
        ));
    }

    Ok(value)
}
