use crate::platform::config::{ConfigError, Vars, secret};

const ENABLED: &str = "WHIO_AUDIODB_ENABLED";
const KEY: &str = "WHIO_AUDIODB_KEY";

#[derive(Debug, Clone)]
pub struct Config {
    pub key: String,
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
    let key =
        secret(vars, KEY, "WHIO_AUDIODB_KEY_FILE")?.ok_or(ConfigError::Missing { name: KEY })?;
    Ok(Some(Config { key }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_ignores_secret_values() {
        let vars = Vars::from([(KEY.to_owned(), "unused".to_owned())]);
        assert!(from_vars(&vars).unwrap().is_none());
    }

    #[test]
    fn enabled_values_are_strict() {
        let vars = Vars::from([(ENABLED.to_owned(), "yes".to_owned())]);
        assert!(matches!(
            from_vars(&vars),
            Err(ConfigError::Invalid { name: ENABLED, .. })
        ));
    }

    #[test]
    fn enabled_requires_a_key() {
        let vars = Vars::from([(ENABLED.to_owned(), "true".to_owned())]);
        assert!(matches!(
            from_vars(&vars),
            Err(ConfigError::Missing { name: KEY })
        ));
    }
}
