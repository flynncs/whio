use std::{collections::HashMap, net::SocketAddr, time::Duration};

use reqwest::Url;
use whio_core::compat::SourceTags;
use whio_core::identity::secrets;

const DEFAULT_BIND_ADDRESS: &str = "127.0.0.1:3000";
const DEFAULT_LOG_LEVEL: &str = "info";
const DEFAULT_YOUTUBE_CONNECT_TIMEOUT_SECONDS: u64 = 2;
const DEFAULT_YOUTUBE_TOTAL_TIMEOUT_SECONDS: u64 = 35;

const DATABASE_URL: &str = "WHIO_DATABASE_URL";
const CREDENTIAL_KEY: &str = "WHIO_CREDENTIAL_KEY";
const YOUTUBE_ENABLED: &str = "WHIO_YOUTUBE_ENABLED";
const YOUTUBE_RESOLVER_URL: &str = "WHIO_YOUTUBE_RESOLVER_URL";
const YOUTUBE_RESOLVER_CONNECT_TIMEOUT: &str = "WHIO_YOUTUBE_RESOLVER_CONNECT_TIMEOUT_SECONDS";
const YOUTUBE_RESOLVER_TOTAL_TIMEOUT: &str = "WHIO_YOUTUBE_RESOLVER_TOTAL_TIMEOUT_SECONDS";
const PLAYBACK_MAX_SESSIONS: &str = "WHIO_PLAYBACK_MAX_SESSIONS";
const SPOOL_DIR: &str = "WHIO_SPOOL_DIR";
const DEFAULT_MAX_PLAYBACK_SESSIONS: usize = 16;
const YOUTUBE_MEDIA_SUFFIX: &str = "WHIO_YOUTUBE_MEDIA_SUFFIX";
const ARTWORK_HOST_SUFFIXES: &str = "WHIO_ARTWORK_HOST_SUFFIXES";
const SOURCE_TAGS: &str = "WHIO_SOURCE_TAGS";
const MEDIA_ROOTS: &str = "WHIO_MEDIA_ROOTS";

const DEFAULT_YOUTUBE_MEDIA_SUFFIX: &str = whio_core::media::DEFAULT_YOUTUBE_MEDIA_SUFFIX;

#[derive(Debug)]
pub(crate) struct Config {
    pub(crate) bind_address: SocketAddr,
    pub(crate) database_url: String,
    pub(crate) credential_key: secrets::CredentialKey,
    pub(crate) log_level: tracing::Level,
    pub(crate) youtube_resolver: YoutubeResolverConfig,
    pub(crate) spool_dir: std::path::PathBuf,
    pub(crate) max_playback_sessions: usize,
    pub(crate) max_spool_bytes: u64,
    pub(crate) spool_budget_bytes: u64,
    pub(crate) youtube_media_suffix: String,
    pub(crate) artwork_host_suffixes: Vec<String>,
    pub(crate) source_tags: SourceTags,
    pub(crate) dedup_mode: whio_core::catalogue::DedupMode,
    pub(crate) media_roots: Vec<std::path::PathBuf>,
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

        let credential_key_raw = vars.get(CREDENTIAL_KEY).ok_or(ConfigError::Missing {
            name: CREDENTIAL_KEY,
        })?;
        let credential_key =
            secrets::CredentialKey::from_base64(credential_key_raw).map_err(|err| {
                ConfigError::Invalid {
                    name: CREDENTIAL_KEY,
                    value: credential_key_raw.to_owned(),
                    reason: match err {
                        secrets::SecretError::InvalidKeyLength => "must decode to exactly 32 bytes",
                        secrets::SecretError::InvalidKeyEncoding => "must be valid base64",
                        _ => "invalid credential key",
                    },
                }
            })?;

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

        let spool_dir = vars
            .get(SPOOL_DIR)
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| std::env::temp_dir().join("whio-spools"));

        let max_playback_sessions = match vars.get(PLAYBACK_MAX_SESSIONS) {
            None => DEFAULT_MAX_PLAYBACK_SESSIONS,
            Some(raw) => raw.parse().map_err(|_| {
                invalid(
                    PLAYBACK_MAX_SESSIONS,
                    raw,
                    "must be a positive session count",
                )
            })?,
        };
        if max_playback_sessions == 0 {
            return Err(invalid(
                PLAYBACK_MAX_SESSIONS,
                "0",
                "must be a positive session count",
            ));
        }

        let spool_limit = |name: &'static str, default: u64| -> Result<u64, ConfigError> {
            let value = match vars.get(name) {
                Some(raw) => raw
                    .parse::<u64>()
                    .map_err(|_| invalid(name, raw, "must be a positive byte count"))?,
                None => default,
            };
            if value < 65536 || value > (u32::MAX as u64) * 65536 {
                return Err(invalid(
                    name,
                    &value.to_string(),
                    "must be between 64 KiB and 256 TiB",
                ));
            }
            Ok(value)
        };
        let max_spool_bytes = spool_limit(
            "WHIO_SPOOL_MAX_BYTES",
            whio_core::delivery::DEFAULT_MAX_SPOOL_BYTES,
        )?;
        let spool_budget_bytes = spool_limit(
            "WHIO_SPOOL_BUDGET_BYTES",
            whio_core::delivery::DEFAULT_SPOOL_BUDGET_BYTES,
        )?;
        if max_spool_bytes > spool_budget_bytes {
            return Err(invalid(
                "WHIO_SPOOL_MAX_BYTES",
                &max_spool_bytes.to_string(),
                "must not exceed WHIO_SPOOL_BUDGET_BYTES",
            ));
        }

        let youtube_media_suffix = vars
            .get(YOUTUBE_MEDIA_SUFFIX)
            .map(|raw| raw.trim().to_ascii_lowercase())
            .filter(|raw| !raw.is_empty())
            .unwrap_or_else(|| DEFAULT_YOUTUBE_MEDIA_SUFFIX.to_owned());

        let default_artwork_suffixes = whio_core::media::DEFAULT_ARTWORK_HOST_SUFFIXES.join(",");
        let artwork_host_suffixes = parse_suffixes(
            vars.get(ARTWORK_HOST_SUFFIXES)
                .map(String::as_str)
                .unwrap_or(&default_artwork_suffixes),
            ARTWORK_HOST_SUFFIXES,
        )?;

        let source_tags = match vars.get(SOURCE_TAGS) {
            None => SourceTags::Off,
            Some(raw) => SourceTags::parse(raw)
                .ok_or_else(|| invalid(SOURCE_TAGS, raw, "must be `off` or `artist`"))?,
        };

        let media_roots = match vars.get(MEDIA_ROOTS) {
            None => Vec::new(),
            Some(raw) => parse_media_roots(raw)?,
        };

        let dedup_mode = match vars
            .get("WHIO_DEDUP_MODE")
            .map(String::as_str)
            .unwrap_or("suppress")
        {
            "off" => whio_core::catalogue::DedupMode::Off,
            "shadow" => whio_core::catalogue::DedupMode::Shadow,
            "suppress" => whio_core::catalogue::DedupMode::Suppress,
            raw => {
                return Err(invalid(
                    "WHIO_DEDUP_MODE",
                    raw,
                    "must be off, shadow or suppress",
                ));
            }
        };

        Ok(Self {
            dedup_mode,
            bind_address,
            database_url: database_raw.to_owned(),
            log_level,
            credential_key,
            youtube_resolver,
            spool_dir,
            max_playback_sessions,
            max_spool_bytes,
            spool_budget_bytes,
            youtube_media_suffix,
            artwork_host_suffixes,
            source_tags,
            media_roots,
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

fn parse_media_roots(raw: &str) -> Result<Vec<std::path::PathBuf>, ConfigError> {
    let roots: Vec<std::path::PathBuf> = raw
        .split(',')
        .map(|root| std::path::PathBuf::from(root.trim()))
        .filter(|root| !root.as_os_str().is_empty())
        .collect();

    if roots.is_empty() {
        return Err(invalid(
            MEDIA_ROOTS,
            raw,
            "must list at least one directory",
        ));
    }

    for (index, root) in roots.iter().enumerate() {
        if !root.is_absolute() {
            return Err(invalid(
                MEDIA_ROOTS,
                &root.display().to_string(),
                "must be an absolute path",
            ));
        }
        if roots[..index].contains(root) {
            return Err(invalid(
                MEDIA_ROOTS,
                &root.display().to_string(),
                "must not list the same directory twice",
            ));
        }
    }

    Ok(roots)
}

/// host suffixes are matched case-insensitively and must list at least one entry.
fn parse_suffixes(raw: &str, name: &'static str) -> Result<Vec<String>, ConfigError> {
    let suffixes: Vec<String> = raw
        .split(',')
        .map(|suffix| suffix.trim().to_ascii_lowercase())
        .filter(|suffix| !suffix.is_empty())
        .collect();

    if suffixes.is_empty() {
        return Err(invalid(name, raw, "must list at least one host suffix"));
    }

    Ok(suffixes)
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
        // base64 of 32 zero bytes
        vars.entry(CREDENTIAL_KEY.to_owned())
            .or_insert_with(|| "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA".to_owned());
        Config::from_map(&vars)
    }

    #[test]
    fn dedup_defaults_to_suppression_and_accepts_rollback_modes() {
        use whio_core::catalogue::DedupMode;
        assert_eq!(parse_config(&[]).unwrap().dedup_mode, DedupMode::Suppress);
        assert_eq!(
            parse_config(&[("WHIO_DEDUP_MODE", "off")])
                .unwrap()
                .dedup_mode,
            DedupMode::Off
        );
        assert_eq!(
            parse_config(&[("WHIO_DEDUP_MODE", "shadow")])
                .unwrap()
                .dedup_mode,
            DedupMode::Shadow
        );
        assert!(parse_config(&[("WHIO_DEDUP_MODE", "unknown")]).is_err());
    }

    #[test]
    fn media_suffixes_default_and_accept_overrides() {
        let config = parse_config(&[]).unwrap();
        assert_eq!(config.youtube_media_suffix, "googlevideo.com");
        assert_eq!(
            config.artwork_host_suffixes,
            vec!["googleusercontent.com", "ytimg.com"]
        );

        let config = parse_config(&[
            (YOUTUBE_MEDIA_SUFFIX, "Example.COM"),
            (ARTWORK_HOST_SUFFIXES, " cdn.example , art.example "),
        ])
        .unwrap();
        assert_eq!(config.youtube_media_suffix, "example.com");
        assert_eq!(
            config.artwork_host_suffixes,
            vec!["cdn.example", "art.example"]
        );

        assert!(matches!(
            parse_config(&[(ARTWORK_HOST_SUFFIXES, " , ")]),
            Err(ConfigError::Invalid {
                name: ARTWORK_HOST_SUFFIXES,
                ..
            })
        ));
    }

    #[test]
    fn source_tags_default_off_and_reject_unknown_placements() {
        assert_eq!(parse_config(&[]).unwrap().source_tags, SourceTags::Off);
        assert_eq!(
            parse_config(&[(SOURCE_TAGS, "artist")])
                .unwrap()
                .source_tags,
            SourceTags::ArtistPrefix
        );
        assert_eq!(
            parse_config(&[(SOURCE_TAGS, " off ")]).unwrap().source_tags,
            SourceTags::Off
        );

        assert!(matches!(
            parse_config(&[(SOURCE_TAGS, "album")]),
            Err(ConfigError::Invalid {
                name: SOURCE_TAGS,
                ..
            })
        ));
    }

    #[test]
    fn media_roots_default_empty_and_accept_an_absolute_list() {
        assert!(parse_config(&[]).unwrap().media_roots.is_empty());

        let config = parse_config(&[(MEDIA_ROOTS, "/tank/data/media/music")]).unwrap();
        assert_eq!(
            config.media_roots,
            vec![std::path::PathBuf::from("/tank/data/media/music")]
        );

        let config = parse_config(&[(MEDIA_ROOTS, " /a , /b ")]).unwrap();
        assert_eq!(
            config.media_roots,
            vec![
                std::path::PathBuf::from("/a"),
                std::path::PathBuf::from("/b")
            ]
        );
    }

    #[test]
    fn media_roots_reject_relative_empty_and_duplicate_entries() {
        for value in ["", " , ", "relative/path", "/a,/a", "/a, /a"] {
            assert!(
                matches!(
                    parse_config(&[(MEDIA_ROOTS, value)]),
                    Err(ConfigError::Invalid {
                        name: MEDIA_ROOTS,
                        ..
                    })
                ),
                "{value}"
            );
        }
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
    fn credential_key_configuration_is_required() {
        let mut vars = HashMap::new();
        vars.insert(
            DATABASE_URL.to_owned(),
            "postgres://whio:whio-dev@localhost/whio".to_owned(),
        );

        assert!(matches!(
            Config::from_map(&vars),
            Err(ConfigError::Missing {
                name: CREDENTIAL_KEY
            })
        ));
    }

    #[test]
    fn credential_key_must_decode_to_32_bytes() {
        let mut vars = HashMap::new();
        vars.insert(
            DATABASE_URL.to_owned(),
            "postgres://whio:whio-dev@localhost/whio".to_owned(),
        );
        vars.insert(CREDENTIAL_KEY.to_owned(), "tooshort".to_owned());

        assert!(matches!(
            Config::from_map(&vars),
            Err(ConfigError::Invalid {
                name: CREDENTIAL_KEY,
                ..
            })
        ));
    }

    #[test]
    fn playback_sessions_and_spool_dir_have_sane_defaults() {
        let config = parse_config(&[]).unwrap();
        assert_eq!(config.max_playback_sessions, 16);
        assert_eq!(config.spool_dir, std::env::temp_dir().join("whio-spools"));
    }

    #[test]
    fn playback_limits_accept_overrides_and_reject_zero_sessions() {
        let config = parse_config(&[
            (PLAYBACK_MAX_SESSIONS, "4"),
            (SPOOL_DIR, "/tmp/whio-spools"),
        ])
        .unwrap();
        assert_eq!(config.max_playback_sessions, 4);
        assert_eq!(
            config.spool_dir,
            std::path::PathBuf::from("/tmp/whio-spools")
        );

        assert!(matches!(
            parse_config(&[(PLAYBACK_MAX_SESSIONS, "0")]),
            Err(ConfigError::Invalid {
                name: PLAYBACK_MAX_SESSIONS,
                ..
            })
        ));
        assert!(matches!(
            parse_config(&[(PLAYBACK_MAX_SESSIONS, "many")]),
            Err(ConfigError::Invalid {
                name: PLAYBACK_MAX_SESSIONS,
                ..
            })
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
