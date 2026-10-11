use std::{
    path::{Path, PathBuf},
    time::Duration,
};

use crate::platform::config::{ConfigError, Vars, invalid, parse_timeout};

const ENABLED: &str = "WHIO_ACQUISITION_ENABLED";
const STAGING_DIR: &str = "WHIO_ACQUISITION_STAGING_DIR";
const LIBRARY_DIR: &str = "WHIO_ACQUISITION_LIBRARY_DIR";
const CANDIDATE_BUDGET: &str = "WHIO_ACQUISITION_CANDIDATE_BUDGET";
const TRANSFER_TIMEOUT: &str = "WHIO_ACQUISITION_TRANSFER_TIMEOUT_SECONDS";
const POLL: &str = "WHIO_ACQUISITION_POLL_SECONDS";

const DEFAULT_CANDIDATE_BUDGET: usize = 3;
const MIN_CANDIDATE_BUDGET: usize = 1;
const MAX_CANDIDATE_BUDGET: usize = 10;
const DEFAULT_TRANSFER_TIMEOUT_SECONDS: u64 = 1800;
const MIN_TRANSFER_TIMEOUT_SECONDS: u64 = 60;
const MAX_TRANSFER_TIMEOUT_SECONDS: u64 = 86_400;
const DEFAULT_POLL_SECONDS: u64 = 10;
const MIN_POLL_SECONDS: u64 = 1;
const MAX_POLL_SECONDS: u64 = 300;

#[derive(Debug, Clone)]
pub struct AcquisitionConfig {
    pub enabled: bool,
    pub staging_dir: Option<PathBuf>,
    pub library_dir: Option<PathBuf>,
    pub candidate_budget: usize,
    pub transfer_timeout: Duration,
    pub poll: Duration,
}

impl AcquisitionConfig {
    pub(crate) fn from_vars(
        vars: &Vars,
        slskd_enabled: bool,
        musicbrainz_enabled: bool,
        media_roots: &[PathBuf],
    ) -> Result<Self, ConfigError> {
        let enabled = match vars.get(ENABLED) {
            None => false,
            Some(raw) => match raw.trim() {
                "true" => true,
                "false" => false,
                _ => return Err(invalid(ENABLED, raw, "must be true or false")),
            },
        };

        let candidate_budget = bounded_number(
            vars,
            CANDIDATE_BUDGET,
            DEFAULT_CANDIDATE_BUDGET,
            MIN_CANDIDATE_BUDGET,
            MAX_CANDIDATE_BUDGET,
        )?;
        let transfer_timeout = bounded_seconds(
            vars,
            TRANSFER_TIMEOUT,
            DEFAULT_TRANSFER_TIMEOUT_SECONDS,
            MIN_TRANSFER_TIMEOUT_SECONDS,
            MAX_TRANSFER_TIMEOUT_SECONDS,
        )?;
        let poll = bounded_seconds(
            vars,
            POLL,
            DEFAULT_POLL_SECONDS,
            MIN_POLL_SECONDS,
            MAX_POLL_SECONDS,
        )?;

        if !enabled {
            return Ok(Self {
                enabled: false,
                staging_dir: None,
                library_dir: None,
                candidate_budget,
                transfer_timeout,
                poll,
            });
        }

        if !slskd_enabled {
            return Err(ConfigError::Missing {
                name: "WHIO_SLSKD_ENABLED",
            });
        }
        if !musicbrainz_enabled {
            return Err(ConfigError::Missing {
                name: "WHIO_MUSICBRAINZ_ENABLED",
            });
        }

        let staging_dir = absolute(vars, STAGING_DIR)?;
        let library_dir = absolute(vars, LIBRARY_DIR)?;

        if !root_is_trusted(&staging_dir) {
            return Err(invalid(
                STAGING_DIR,
                &staging_dir.display().to_string(),
                "must have a canonical directory ancestor and no symlinks",
            ));
        }

        if !media_roots.contains(&library_dir) {
            return Err(invalid(
                LIBRARY_DIR,
                &library_dir.display().to_string(),
                "must equal one of WHIO_MEDIA_ROOTS",
            ));
        }
        if overlaps(&staging_dir, media_roots) {
            return Err(invalid(
                STAGING_DIR,
                &staging_dir.display().to_string(),
                "must not overlap a media root",
            ));
        }

        Ok(Self {
            enabled: true,
            staging_dir: Some(staging_dir),
            library_dir: Some(library_dir),
            candidate_budget,
            transfer_timeout,
            poll,
        })
    }

    pub fn staging_dir(&self) -> Option<&Path> {
        self.staging_dir.as_deref()
    }

    pub fn library_dir(&self) -> Option<&Path> {
        self.library_dir.as_deref()
    }
}

fn absolute(vars: &Vars, name: &'static str) -> Result<PathBuf, ConfigError> {
    let raw = vars.get(name).ok_or(ConfigError::Missing { name })?;
    let path = PathBuf::from(raw);

    if !path.is_absolute() {
        return Err(invalid(name, raw, "must be an absolute path"));
    }

    Ok(path)
}

pub(crate) fn root_is_trusted(path: &Path) -> bool {
    let mut current = PathBuf::new();
    let mut deepest_existing = None;

    for component in path.components() {
        if !matches!(
            component,
            std::path::Component::Normal(_)
                | std::path::Component::RootDir
                | std::path::Component::Prefix(_)
        ) {
            return false;
        }

        current.push(component);

        match std::fs::symlink_metadata(&current) {
            Ok(metadata) => {
                if !metadata.is_dir() || metadata.file_type().is_symlink() {
                    return false;
                }

                deepest_existing = Some(current.clone());
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return false,
        }
    }

    match deepest_existing {
        Some(existing) => match std::fs::canonicalize(&existing) {
            Ok(canonical) => canonical == existing,
            Err(_) => false,
        },
        None => false,
    }
}

fn overlaps(staging_dir: &Path, media_roots: &[PathBuf]) -> bool {
    media_roots
        .iter()
        .any(|root| staging_dir.starts_with(root) || root.starts_with(staging_dir))
}

fn bounded_number(
    vars: &Vars,
    name: &'static str,
    default: usize,
    min: usize,
    max: usize,
) -> Result<usize, ConfigError> {
    let raw = vars.get(name).map(String::as_str);
    let value = match raw {
        Some(raw) => raw
            .parse::<usize>()
            .map_err(|_| invalid(name, raw, "must be a positive integer"))?,
        None => default,
    };

    if !(min..=max).contains(&value) {
        return Err(invalid(
            name,
            &value.to_string(),
            "must be within the allowed range",
        ));
    }

    Ok(value)
}

fn bounded_seconds(
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

#[cfg(test)]
mod tests {
    use super::*;

    fn vars(values: &[(&str, &str)]) -> Vars {
        values
            .iter()
            .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
            .collect()
    }

    fn media() -> Vec<PathBuf> {
        vec![PathBuf::from("/music")]
    }

    fn enabled() -> Vars {
        vars(&[
            ("WHIO_ACQUISITION_ENABLED", "true"),
            ("WHIO_ACQUISITION_STAGING_DIR", "/staging"),
            ("WHIO_ACQUISITION_LIBRARY_DIR", "/music"),
        ])
    }

    #[test]
    fn acquisition_is_disabled_by_default() {
        let config = AcquisitionConfig::from_vars(&Vars::new(), false, false, &[]).unwrap();

        assert!(!config.enabled);
        assert_eq!(config.candidate_budget, 3);
        assert_eq!(config.transfer_timeout, Duration::from_secs(1800));
        assert_eq!(config.poll, Duration::from_secs(10));

        let invalid = vars(&[("WHIO_ACQUISITION_ENABLED", "maybe")]);
        assert!(AcquisitionConfig::from_vars(&invalid, true, true, &media()).is_err());
    }

    #[test]
    fn enabled_acquisition_requires_slskd_and_musicbrainz() {
        assert!(AcquisitionConfig::from_vars(&enabled(), false, true, &media()).is_err());
        assert!(AcquisitionConfig::from_vars(&enabled(), true, false, &media()).is_err());
        assert!(AcquisitionConfig::from_vars(&enabled(), true, true, &media()).is_ok());
    }

    #[test]
    fn the_library_dir_must_be_a_media_root() {
        let vars = vars(&[
            ("WHIO_ACQUISITION_ENABLED", "true"),
            ("WHIO_ACQUISITION_STAGING_DIR", "/staging"),
            ("WHIO_ACQUISITION_LIBRARY_DIR", "/elsewhere"),
        ]);

        assert!(AcquisitionConfig::from_vars(&vars, true, true, &media()).is_err());
    }

    #[test]
    fn staging_inside_a_media_root_is_invalid() {
        for staging in ["/music/staging", "/"] {
            let vars = vars(&[
                ("WHIO_ACQUISITION_ENABLED", "true"),
                ("WHIO_ACQUISITION_STAGING_DIR", staging),
                ("WHIO_ACQUISITION_LIBRARY_DIR", "/music"),
            ]);

            assert!(
                AcquisitionConfig::from_vars(&vars, true, true, &media()).is_err(),
                "staging {staging} overlaps a media root"
            );
        }
    }

    #[test]
    fn a_symlinked_staging_root_is_invalid() {
        let real = std::env::temp_dir().join(format!("whio-aq-cfg-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&real).unwrap();
        let link = real.join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let root = vars(&[
            ("WHIO_ACQUISITION_ENABLED", "true"),
            ("WHIO_ACQUISITION_STAGING_DIR", &link.display().to_string()),
            ("WHIO_ACQUISITION_LIBRARY_DIR", "/music"),
        ]);
        assert!(AcquisitionConfig::from_vars(&root, true, true, &media()).is_err());

        let parent = real.join("parentlink");
        std::os::unix::fs::symlink(&real, &parent).unwrap();
        let child = parent.join("staging");
        let parent_symlink = vars(&[
            ("WHIO_ACQUISITION_ENABLED", "true"),
            ("WHIO_ACQUISITION_STAGING_DIR", &child.display().to_string()),
            ("WHIO_ACQUISITION_LIBRARY_DIR", "/music"),
        ]);
        assert!(AcquisitionConfig::from_vars(&parent_symlink, true, true, &media()).is_err());

        let _ = std::fs::remove_dir_all(&real);
    }

    #[test]
    fn one_bounds_case_per_number() {
        let budget = |value: &str| {
            let mut vars = enabled();
            vars.insert(CANDIDATE_BUDGET.to_owned(), value.to_owned());
            AcquisitionConfig::from_vars(&vars, true, true, &media())
        };
        assert!(budget("0").is_err());
        assert!(budget("1").is_ok());
        assert!(budget("10").is_ok());
        assert!(budget("11").is_err());

        let timeout = |value: &str| {
            let mut vars = enabled();
            vars.insert(TRANSFER_TIMEOUT.to_owned(), value.to_owned());
            AcquisitionConfig::from_vars(&vars, true, true, &media())
        };
        assert!(timeout("59").is_err());
        assert!(timeout("60").is_ok());
        assert!(timeout("86400").is_ok());
        assert!(timeout("86401").is_err());

        let poll = |value: &str| {
            let mut vars = enabled();
            vars.insert(POLL.to_owned(), value.to_owned());
            AcquisitionConfig::from_vars(&vars, true, true, &media())
        };
        assert!(poll("0").is_err());
        assert!(poll("1").is_ok());
        assert!(poll("300").is_ok());
        assert!(poll("301").is_err());
    }
}

#[cfg(test)]
mod root_tests {
    use super::*;

    fn vars(values: &[(&str, &str)]) -> Vars {
        values
            .iter()
            .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
            .collect()
    }

    fn media() -> Vec<PathBuf> {
        vec![PathBuf::from("/music")]
    }

    #[test]
    fn an_absent_staging_root_is_trusted() {
        let vars = vars(&[
            ("WHIO_ACQUISITION_ENABLED", "true"),
            ("WHIO_ACQUISITION_STAGING_DIR", "/definitely/absent/whio-aq"),
            ("WHIO_ACQUISITION_LIBRARY_DIR", "/music"),
        ]);

        assert!(AcquisitionConfig::from_vars(&vars, true, true, &media()).is_ok());
    }

    #[test]
    fn a_traversal_staging_root_is_invalid() {
        let vars = vars(&[
            ("WHIO_ACQUISITION_ENABLED", "true"),
            ("WHIO_ACQUISITION_STAGING_DIR", "/music/../staging-aq"),
            ("WHIO_ACQUISITION_LIBRARY_DIR", "/music"),
        ]);

        assert!(AcquisitionConfig::from_vars(&vars, true, true, &media()).is_err());
    }

    #[test]
    fn a_regular_file_staging_root_is_invalid() {
        let root = std::env::temp_dir().join(format!("whio-aq-cfg-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&root).unwrap();
        let file = root.join("file");
        std::fs::write(&file, b"x").unwrap();
        let staging = file.display().to_string();
        let vars = vars(&[
            ("WHIO_ACQUISITION_ENABLED", "true"),
            ("WHIO_ACQUISITION_STAGING_DIR", &staging),
            ("WHIO_ACQUISITION_LIBRARY_DIR", "/music"),
        ]);

        assert!(AcquisitionConfig::from_vars(&vars, true, true, &media()).is_err());
        assert_eq!(std::fs::read(&file).unwrap(), b"x");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_non_directory_component_is_invalid() {
        let root = std::env::temp_dir().join(format!("whio-aq-cfg-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&root).unwrap();
        let file = root.join("file");
        std::fs::write(&file, b"x").unwrap();
        let staging = file.join("child").display().to_string();
        let vars = vars(&[
            ("WHIO_ACQUISITION_ENABLED", "true"),
            ("WHIO_ACQUISITION_STAGING_DIR", &staging),
            ("WHIO_ACQUISITION_LIBRARY_DIR", "/music"),
        ]);

        assert!(AcquisitionConfig::from_vars(&vars, true, true, &media()).is_err());
        let _ = std::fs::remove_dir_all(&root);
    }
}
