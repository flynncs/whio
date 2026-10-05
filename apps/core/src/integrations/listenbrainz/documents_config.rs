use crate::platform::config::{ConfigError, Vars, secret};

const ENABLED: &str = "WHIO_LISTENBRAINZ_METADATA_ENABLED";
const TOKEN: &str = "WHIO_LISTENBRAINZ_TOKEN";
const ALGORITHM: &str = "WHIO_LISTENBRAINZ_ALGORITHM";
pub const DEFAULT_ALGORITHM: &str =
    "session_based_days_7500_session_300_contribution_5_threshold_10_limit_100_filter_True_skip_30";

#[derive(Debug, Clone)]
pub struct Config {
    pub token: String,
    pub algorithm: String,
}

pub(crate) fn from_vars(vars: &Vars) -> Result<Option<Config>, ConfigError> {
    let enabled = match vars.get(ENABLED) {
        Some(raw) => raw
            .parse()
            .map_err(|_| crate::platform::config::invalid(ENABLED, raw, "must be true or false"))?,
        None => false,
    };
    if !enabled {
        return Ok(None);
    }
    let token = secret(vars, TOKEN, "WHIO_LISTENBRAINZ_TOKEN_FILE")?
        .ok_or(ConfigError::Missing { name: TOKEN })?;
    let algorithm = vars
        .get(ALGORITHM)
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
        .unwrap_or(DEFAULT_ALGORITHM)
        .to_owned();
    Ok(Some(Config { token, algorithm }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_configuration_ignores_token_and_algorithm() {
        let vars = Vars::from([
            (TOKEN.to_owned(), "ignored".to_owned()),
            (ALGORITHM.to_owned(), " ".to_owned()),
        ]);
        assert!(from_vars(&vars).unwrap().is_none());
    }

    #[test]
    fn enabled_configuration_requires_token_and_uses_default_algorithm() {
        let vars = Vars::from([(ENABLED.to_owned(), "true".to_owned())]);
        assert!(matches!(
            from_vars(&vars),
            Err(ConfigError::Missing { name: TOKEN })
        ));
        let vars = Vars::from([
            (ENABLED.to_owned(), "true".to_owned()),
            (TOKEN.to_owned(), " token ".to_owned()),
            (ALGORITHM.to_owned(), "  ".to_owned()),
        ]);
        let config = from_vars(&vars).unwrap().unwrap();
        assert_eq!(config.token, "token");
        assert_eq!(config.algorithm, DEFAULT_ALGORITHM);
    }

    #[test]
    fn an_explicit_algorithm_is_trimmed() {
        let vars = Vars::from([
            (ENABLED.to_owned(), "true".to_owned()),
            (TOKEN.to_owned(), "token".to_owned()),
            (ALGORITHM.to_owned(), " custom ".to_owned()),
        ]);
        assert_eq!(from_vars(&vars).unwrap().unwrap().algorithm, "custom");
    }
}
