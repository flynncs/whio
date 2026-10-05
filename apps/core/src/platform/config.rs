use std::{collections::HashMap, fs, time::Duration};

pub(crate) type Vars = HashMap<String, String>;

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

pub(crate) fn invalid(name: &'static str, value: &str, reason: &'static str) -> ConfigError {
    ConfigError::Invalid {
        name,
        value: value.to_owned(),
        reason,
    }
}

pub(crate) fn parse_timeout(
    vars: &Vars,
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

pub(crate) fn secret(
    vars: &Vars,
    name: &'static str,
    file_name: &'static str,
) -> Result<Option<String>, ConfigError> {
    let value = vars.get(name);
    let file = vars.get(file_name);
    match (value, file) {
        (Some(_), Some(_)) => Err(invalid(
            name,
            "<redacted>",
            "must not set both value and file",
        )),
        (Some(value), None) => nonblank(value, name),
        (None, Some(path)) => {
            let contents = fs::read_to_string(path)
                .map_err(|_| invalid(file_name, "<redacted>", "could not read secret file"))?;
            nonblank(&contents, file_name)
        }
        (None, None) => Ok(None),
    }
}

fn nonblank(value: &str, name: &'static str) -> Result<Option<String>, ConfigError> {
    let value = value.trim();
    if value.is_empty() {
        return Err(invalid(name, "<redacted>", "must not be blank"));
    }
    Ok(Some(value.to_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;

    const NAME: &str = "WHIO_TEST_SECRET";
    const FILE: &str = "WHIO_TEST_SECRET_FILE";

    #[test]
    fn secret_reads_trimmed_values_or_file_contents_and_rejects_invalid_combinations() {
        let value = Vars::from([(NAME.to_owned(), " value ".to_owned())]);
        assert_eq!(
            secret(&value, NAME, FILE).unwrap().as_deref(),
            Some("value")
        );
        assert_eq!(secret(&Vars::new(), NAME, FILE).unwrap(), None);

        let path = std::env::temp_dir().join(format!("whio-secret-{}", uuid::Uuid::now_v7()));
        fs::write(&path, " file value \n").unwrap();
        let file = Vars::from([(FILE.to_owned(), path.display().to_string())]);
        assert_eq!(
            secret(&file, NAME, FILE).unwrap().as_deref(),
            Some("file value")
        );
        fs::write(&path, " \n").unwrap();
        assert!(matches!(
            secret(&file, NAME, FILE),
            Err(ConfigError::Invalid { name: FILE, .. })
        ));
        fs::remove_file(&path).unwrap();

        let both = Vars::from([
            (NAME.to_owned(), "one".to_owned()),
            (FILE.to_owned(), "path".to_owned()),
        ]);
        assert!(matches!(
            secret(&both, NAME, FILE),
            Err(ConfigError::Invalid { name: NAME, .. })
        ));
        let blank = Vars::from([(NAME.to_owned(), "  ".to_owned())]);
        assert!(matches!(
            secret(&blank, NAME, FILE),
            Err(ConfigError::Invalid { name: NAME, .. })
        ));
        let unreadable = Vars::from([(FILE.to_owned(), "/path/that/does/not/exist".to_owned())]);
        let error = secret(&unreadable, NAME, FILE).unwrap_err();
        assert!(matches!(error, ConfigError::Invalid { name: FILE, .. }));
        assert!(!error.to_string().contains("secret-content"));
    }
}
