use crate::platform::{
    config::{ConfigError, Vars},
    http::USER_AGENT,
};

const ENABLED: &str = "WHIO_MUSICBRAINZ_ENABLED";
const USER_AGENT_NAME: &str = "WHIO_MUSICBRAINZ_USER_AGENT";
const WIKIMEDIA_USER_AGENT_NAME: &str = "WHIO_WIKIMEDIA_USER_AGENT";

#[derive(Debug, Clone)]
pub struct Config {
    pub user_agent: String,
    pub wikimedia_user_agent: String,
}

pub(crate) fn from_vars(vars: &Vars) -> Result<Option<Config>, ConfigError> {
    let enabled = match vars.get(ENABLED) {
        Some(raw) => raw
            .parse()
            .map_err(|_| crate::platform::config::invalid(ENABLED, raw, "must be true or false"))?,
        None => true,
    };
    if !enabled {
        return Ok(None);
    }
    let user_agent = vars
        .get(USER_AGENT_NAME)
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
        .unwrap_or(USER_AGENT)
        .to_owned();
    let wikimedia_user_agent = vars
        .get(WIKIMEDIA_USER_AGENT_NAME)
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
        .unwrap_or(&user_agent)
        .to_owned();
    Ok(Some(Config {
        user_agent,
        wikimedia_user_agent,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn configured_user_agents_trim_and_inherit() {
        let vars = Vars::from([(USER_AGENT_NAME.to_owned(), " custom ".to_owned())]);
        let config = from_vars(&vars).unwrap().unwrap();
        assert_eq!(config.user_agent, "custom");
        assert_eq!(config.wikimedia_user_agent, "custom");
    }

    #[test]
    fn enabled_values_are_strict_and_disabled_values_ignore_agents() {
        assert!(from_vars(&Vars::from([(ENABLED.to_owned(), "yes".to_owned())])).is_err());
        let vars = Vars::from([
            (ENABLED.to_owned(), "false".to_owned()),
            (USER_AGENT_NAME.to_owned(), " ".to_owned()),
            (WIKIMEDIA_USER_AGENT_NAME.to_owned(), " ".to_owned()),
        ]);
        assert!(from_vars(&vars).unwrap().is_none());
    }

    #[test]
    fn defaults_are_enabled_with_platform_user_agent() {
        let config = from_vars(&Vars::new()).unwrap().unwrap();
        assert_eq!(config.user_agent, USER_AGENT);
        assert_eq!(config.wikimedia_user_agent, USER_AGENT);
    }
}
