use std::{collections::HashMap, net::SocketAddr, time::Duration};

use crate::facades::SourceTags;
use crate::features::identity::secrets;
use crate::features::metadata::{
    ARTIST_DISPLAY_PROFILE, MetadataProfile, domain::FieldId, selection::FieldOrder,
};
use reqwest::Url;

const DEFAULT_BIND_ADDRESS: &str = "127.0.0.1:3000";
const DEFAULT_LOG_LEVEL: &str = "info";
const DEFAULT_YOUTUBE_CONNECT_TIMEOUT_SECONDS: u64 = 2;
const DEFAULT_YOUTUBE_TOTAL_TIMEOUT_SECONDS: u64 = 35;
const DEFAULT_METADATA_STORE_BUDGET_BYTES: u64 = 33_554_432;
const MIN_METADATA_STORE_BUDGET_BYTES: u64 = 1_048_576;
const DEFAULT_METADATA_LANGUAGE: &str = "en";
const MAX_METADATA_LANGUAGE_CHARS: usize = 35;
const DEFAULT_ARTWORK_CACHE_BYTES: u64 = 2 * 1024 * 1024 * 1024;

const KNOWN_METADATA_SOURCES: [&str; 8] = [
    crate::features::metadata::selection::YOUTUBE_MUSIC,
    crate::features::metadata::selection::THEAUDIODB,
    crate::features::metadata::selection::MUSICBRAINZ,
    crate::features::metadata::selection::WIKIPEDIA,
    crate::features::metadata::selection::COMMONS,
    crate::features::metadata::selection::LISTENBRAINZ,
    crate::features::metadata::selection::LASTFM,
    crate::features::metadata::selection::LOCAL,
];

const DATABASE_URL: &str = "WHIO_DATABASE_URL";
const CREDENTIAL_KEY: &str = "WHIO_CREDENTIAL_KEY";
const YOUTUBE_ENABLED: &str = "WHIO_YOUTUBE_ENABLED";
const YOUTUBE_RESOLVER_URL: &str = "WHIO_YOUTUBE_RESOLVER_URL";
const YOUTUBE_RESOLVER_CONNECT_TIMEOUT: &str = "WHIO_YOUTUBE_RESOLVER_CONNECT_TIMEOUT_SECONDS";
const YOUTUBE_RESOLVER_TOTAL_TIMEOUT: &str = "WHIO_YOUTUBE_RESOLVER_TOTAL_TIMEOUT_SECONDS";
const METADATA_RESOLVER_URL: &str = "WHIO_METADATA_RESOLVER_URL";
const METADATA_RESOLVER_CONNECT_TIMEOUT: &str = "WHIO_METADATA_RESOLVER_CONNECT_TIMEOUT_SECONDS";
const METADATA_RESOLVER_TOTAL_TIMEOUT: &str = "WHIO_METADATA_RESOLVER_TOTAL_TIMEOUT_SECONDS";
const METADATA_STORE_BUDGET_BYTES: &str = "WHIO_METADATA_STORE_BUDGET_BYTES";
const METADATA_ARTIST_PROFILE: &str = "WHIO_METADATA_ARTIST_PROFILE";
const METADATA_LANGUAGE: &str = "WHIO_METADATA_LANGUAGE";
const METADATA_SOURCE_ORDER: &str = "WHIO_METADATA_SOURCE_ORDER";
const ARTWORK_CACHE_DIR: &str = "WHIO_ARTWORK_CACHE_DIR";
const ARTWORK_CACHE_BYTES: &str = "WHIO_ARTWORK_CACHE_BYTES";
const PLAYBACK_MAX_SESSIONS: &str = "WHIO_PLAYBACK_MAX_SESSIONS";
const SPOOL_DIR: &str = "WHIO_SPOOL_DIR";
const DEFAULT_MAX_PLAYBACK_SESSIONS: usize = 16;
const YOUTUBE_MEDIA_SUFFIX: &str = "WHIO_YOUTUBE_MEDIA_SUFFIX";
const ARTWORK_HOST_SUFFIXES: &str = "WHIO_ARTWORK_HOST_SUFFIXES";
const SOURCE_TAGS: &str = "WHIO_SOURCE_TAGS";
const MEDIA_ROOTS: &str = "WHIO_MEDIA_ROOTS";

const WORKER_QUEUES: &str = "WHIO_WORKER_QUEUES";
const WORKER_BIND_ADDRESS: &str = "WHIO_WORKER_BIND_ADDRESS";
const WORKER_MAX_WORKERS: &str = "WHIO_WORKER_MAX_WORKERS";
const WORKER_POLL_INTERVAL: &str = "WHIO_WORKER_POLL_INTERVAL_MILLISECONDS";
const WORKER_PROMOTE_INTERVAL: &str = "WHIO_WORKER_PROMOTE_INTERVAL_MILLISECONDS";
const WORKER_DRAIN_TIMEOUT: &str = "WHIO_WORKER_DRAIN_TIMEOUT_SECONDS";

const DEFAULT_WORKER_BIND_ADDRESS: &str = "127.0.0.1:3001";
const DEFAULT_WORKER_MAX_WORKERS: u32 = 4;
const DEFAULT_WORKER_POLL_INTERVAL_MILLISECONDS: u64 = 200;
const DEFAULT_WORKER_PROMOTE_INTERVAL_MILLISECONDS: u64 = 250;
const DEFAULT_WORKER_DRAIN_TIMEOUT_SECONDS: u64 = 30;

const DEFAULT_YOUTUBE_MEDIA_SUFFIX: &str =
    crate::features::playback::media::DEFAULT_YOUTUBE_MEDIA_SUFFIX;

#[derive(Debug)]
pub(crate) struct Config {
    pub(crate) bind_address: SocketAddr,
    pub(crate) database_url: String,
    pub(crate) credential_key: secrets::CredentialKey,
    pub(crate) log_level: tracing::Level,
    pub(crate) youtube_resolver: YoutubeResolverConfig,
    pub(crate) metadata_resolver: MetadataResolverConfig,
    pub(crate) metadata_store_budget_bytes: u64,
    pub(crate) metadata_artist_profile: MetadataProfile,
    pub(crate) metadata_language: String,
    pub(crate) metadata_source_order: FieldOrder,
    pub(crate) artwork_cache_dir: Option<std::path::PathBuf>,
    pub(crate) artwork_cache_bytes: u64,
    pub(crate) spool_dir: std::path::PathBuf,
    pub(crate) max_playback_sessions: usize,
    pub(crate) max_spool_bytes: u64,
    pub(crate) spool_budget_bytes: u64,
    pub(crate) youtube_media_suffix: String,
    pub(crate) artwork_host_suffixes: Vec<String>,
    pub(crate) source_tags: SourceTags,
    pub(crate) dedup_mode: crate::features::music_library::catalogue::DedupMode,
    pub(crate) libraries: Vec<crate::features::music_library::library::LibraryRegistration>,
    pub(crate) worker: WorkerConfig,
}

#[derive(Debug, Clone)]
pub(crate) struct WorkerConfig {
    pub(crate) queues: Vec<String>,
    pub(crate) bind_address: SocketAddr,
    pub(crate) max_workers: u32,
    pub(crate) poll_interval: Duration,
    pub(crate) promote_interval: Duration,
    pub(crate) drain_timeout: Duration,
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

#[derive(Debug)]
pub(crate) enum MetadataResolverConfig {
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

        let metadata_resolver = match vars.get(METADATA_RESOLVER_URL) {
            None => MetadataResolverConfig::Disabled,
            Some(raw_url) => {
                let url = raw_url.parse::<Url>().map_err(|_| {
                    invalid(
                        METADATA_RESOLVER_URL,
                        raw_url,
                        "must be an absolute HTTP(S) URL",
                    )
                })?;

                if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
                    return Err(invalid(
                        METADATA_RESOLVER_URL,
                        raw_url,
                        "must be an absolute HTTP(S) URL",
                    ));
                }

                let connect_timeout = parse_timeout(
                    vars,
                    METADATA_RESOLVER_CONNECT_TIMEOUT,
                    DEFAULT_YOUTUBE_CONNECT_TIMEOUT_SECONDS,
                )?;
                let total_timeout = parse_timeout(
                    vars,
                    METADATA_RESOLVER_TOTAL_TIMEOUT,
                    DEFAULT_YOUTUBE_TOTAL_TIMEOUT_SECONDS,
                )?;

                if total_timeout < Duration::from_secs(DEFAULT_YOUTUBE_TOTAL_TIMEOUT_SECONDS) {
                    return Err(invalid(
                        METADATA_RESOLVER_TOTAL_TIMEOUT,
                        &total_timeout.as_secs().to_string(),
                        "must be at least 35 seconds",
                    ));
                }
                if total_timeout <= connect_timeout {
                    return Err(invalid(
                        METADATA_RESOLVER_TOTAL_TIMEOUT,
                        &total_timeout.as_secs().to_string(),
                        "must be greater than the connect timeout",
                    ));
                }

                MetadataResolverConfig::Enabled {
                    url,
                    connect_timeout,
                    total_timeout,
                }
            }
        };

        let metadata_store_budget_bytes = match vars.get(METADATA_STORE_BUDGET_BYTES) {
            None => DEFAULT_METADATA_STORE_BUDGET_BYTES,
            Some(raw) => raw
                .parse::<u64>()
                .map_err(|_| invalid(METADATA_STORE_BUDGET_BYTES, raw, "must be a byte count"))?,
        };
        if metadata_store_budget_bytes < MIN_METADATA_STORE_BUDGET_BYTES {
            return Err(invalid(
                METADATA_STORE_BUDGET_BYTES,
                &metadata_store_budget_bytes.to_string(),
                "must be at least 1 MiB",
            ));
        }

        let profile_raw = vars
            .get(METADATA_ARTIST_PROFILE)
            .map(String::as_str)
            .unwrap_or(ARTIST_DISPLAY_PROFILE);
        let metadata_artist_profile = MetadataProfile::parse(profile_raw).ok_or_else(|| {
            invalid(
                METADATA_ARTIST_PROFILE,
                profile_raw,
                "must be a known metadata profile",
            )
        })?;

        let metadata_language = vars
            .get(METADATA_LANGUAGE)
            .map(|raw| raw.trim().to_owned())
            .unwrap_or_else(|| DEFAULT_METADATA_LANGUAGE.to_owned());
        if metadata_language.is_empty()
            || metadata_language.chars().count() > MAX_METADATA_LANGUAGE_CHARS
        {
            return Err(invalid(
                METADATA_LANGUAGE,
                &metadata_language,
                "must be a language tag of at most 35 characters",
            ));
        }

        let metadata_source_order = match vars.get(METADATA_SOURCE_ORDER) {
            None => FieldOrder::defaults(),
            Some(raw) => parse_source_order(raw)?,
        };

        let artwork_cache_dir = vars.get(ARTWORK_CACHE_DIR).map(std::path::PathBuf::from);
        let artwork_cache_bytes = match vars.get(ARTWORK_CACHE_BYTES) {
            None => DEFAULT_ARTWORK_CACHE_BYTES,
            Some(raw) => raw
                .parse::<u64>()
                .map_err(|_| invalid(ARTWORK_CACHE_BYTES, raw, "must be a positive byte count"))?,
        };
        if artwork_cache_bytes == 0 {
            return Err(invalid(
                ARTWORK_CACHE_BYTES,
                "0",
                "must be a positive byte count",
            ));
        }

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
            crate::features::playback::delivery::DEFAULT_MAX_SPOOL_BYTES,
        )?;
        let spool_budget_bytes = spool_limit(
            "WHIO_SPOOL_BUDGET_BYTES",
            crate::features::playback::delivery::DEFAULT_SPOOL_BUDGET_BYTES,
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

        let default_artwork_suffixes =
            crate::features::music_library::artwork::DEFAULT_HOST_SUFFIXES.join(",");
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

        let libraries = match vars.get(MEDIA_ROOTS) {
            None => Vec::new(),
            Some(raw) => parse_media_roots(raw)?,
        };

        let dedup_mode = match vars
            .get("WHIO_DEDUP_MODE")
            .map(String::as_str)
            .unwrap_or("suppress")
        {
            "off" => crate::features::music_library::catalogue::DedupMode::Off,
            "shadow" => crate::features::music_library::catalogue::DedupMode::Shadow,
            "suppress" => crate::features::music_library::catalogue::DedupMode::Suppress,
            raw => {
                return Err(invalid(
                    "WHIO_DEDUP_MODE",
                    raw,
                    "must be off, shadow or suppress",
                ));
            }
        };

        let worker = WorkerConfig {
            queues: match vars.get(WORKER_QUEUES) {
                None => Vec::new(),
                Some(raw) => parse_queues(raw)?,
            },
            bind_address: {
                let raw = vars
                    .get(WORKER_BIND_ADDRESS)
                    .map(String::as_str)
                    .unwrap_or(DEFAULT_WORKER_BIND_ADDRESS);
                raw.parse()
                    .map_err(|_| invalid(WORKER_BIND_ADDRESS, raw, "must be a socket address"))?
            },
            max_workers: {
                let raw = vars
                    .get(WORKER_MAX_WORKERS)
                    .map(String::as_str)
                    .map(str::to_owned)
                    .unwrap_or_else(|| DEFAULT_WORKER_MAX_WORKERS.to_string());
                let workers = raw.parse().map_err(|_| {
                    invalid(WORKER_MAX_WORKERS, &raw, "must be a positive worker count")
                })?;
                if workers == 0 {
                    return Err(invalid(
                        WORKER_MAX_WORKERS,
                        &raw,
                        "must be a positive worker count",
                    ));
                }
                workers
            },
            poll_interval: parse_millis(
                vars,
                WORKER_POLL_INTERVAL,
                DEFAULT_WORKER_POLL_INTERVAL_MILLISECONDS,
            )?,
            promote_interval: parse_millis(
                vars,
                WORKER_PROMOTE_INTERVAL,
                DEFAULT_WORKER_PROMOTE_INTERVAL_MILLISECONDS,
            )?,
            drain_timeout: parse_timeout(
                vars,
                WORKER_DRAIN_TIMEOUT,
                DEFAULT_WORKER_DRAIN_TIMEOUT_SECONDS,
            )?,
        };

        Ok(Self {
            dedup_mode,
            bind_address,
            database_url: database_raw.to_owned(),
            log_level,
            credential_key,
            youtube_resolver,
            metadata_resolver,
            metadata_store_budget_bytes,
            metadata_artist_profile,
            metadata_language,
            metadata_source_order,
            artwork_cache_dir,
            artwork_cache_bytes,
            spool_dir,
            max_playback_sessions,
            max_spool_bytes,
            spool_budget_bytes,
            youtube_media_suffix,
            artwork_host_suffixes,
            source_tags,
            libraries,
            worker,
        })
    }

    pub(crate) fn media_roots(&self) -> Vec<std::path::PathBuf> {
        crate::features::music_library::library::deduplicated_roots(&self.libraries)
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

fn parse_millis(
    vars: &HashMap<String, String>,
    name: &'static str,
    default_millis: u64,
) -> Result<Duration, ConfigError> {
    let default = default_millis.to_string();
    let raw = vars.get(name).map(String::as_str).unwrap_or(&default);
    let millis = raw.parse::<u64>().map_err(|_| {
        invalid(
            name,
            raw,
            "must be a positive integer number of milliseconds",
        )
    })?;

    if millis == 0 {
        return Err(invalid(
            name,
            raw,
            "must be a positive integer number of milliseconds",
        ));
    }

    Ok(Duration::from_millis(millis))
}

fn parse_queues(raw: &str) -> Result<Vec<String>, ConfigError> {
    let mut queues: Vec<String> = Vec::new();

    for entry in raw.split(',') {
        let entry = entry.trim();
        if entry.is_empty() {
            continue;
        }
        if queues.iter().any(|queue| queue == entry) {
            return Err(invalid(WORKER_QUEUES, entry, "must list each queue once"));
        }
        queues.push(entry.to_owned());
    }

    if queues.is_empty() {
        return Err(invalid(WORKER_QUEUES, raw, "must list at least one queue"));
    }

    Ok(queues)
}

fn parse_source_order(raw: &str) -> Result<FieldOrder, ConfigError> {
    let mut order = FieldOrder::new();
    let mut fields: Vec<FieldId> = Vec::new();

    for entry in raw.split(';') {
        let entry = entry.trim();
        if entry.is_empty() {
            continue;
        }

        let Some((field_raw, sources_raw)) = entry.split_once('=') else {
            return Err(invalid(
                METADATA_SOURCE_ORDER,
                entry,
                "must be `field=source,source`",
            ));
        };
        let field_raw = field_raw.trim();
        let field = FieldId::parse(field_raw).ok_or_else(|| {
            invalid(
                METADATA_SOURCE_ORDER,
                field_raw,
                "must name a known metadata field",
            )
        })?;

        if fields.contains(&field) {
            return Err(invalid(
                METADATA_SOURCE_ORDER,
                field_raw,
                "must list each field once",
            ));
        }

        let mut sources: Vec<String> = Vec::new();
        for source in sources_raw.split(',') {
            let source = source.trim();
            if source.is_empty() {
                continue;
            }
            if !KNOWN_METADATA_SOURCES.contains(&source) {
                return Err(invalid(
                    METADATA_SOURCE_ORDER,
                    source,
                    "must name a known metadata source",
                ));
            }
            if sources.iter().any(|seen| seen == source) {
                return Err(invalid(
                    METADATA_SOURCE_ORDER,
                    source,
                    "must list each source once",
                ));
            }

            sources.push(source.to_owned());
        }

        if sources.is_empty() {
            return Err(invalid(
                METADATA_SOURCE_ORDER,
                field_raw,
                "must list at least one source",
            ));
        }

        fields.push(field);
        order.set(field, sources);
    }

    if fields.is_empty() {
        return Err(invalid(
            METADATA_SOURCE_ORDER,
            raw,
            "must list at least one field",
        ));
    }

    Ok(order)
}

fn parse_media_roots(
    raw: &str,
) -> Result<Vec<crate::features::music_library::library::LibraryRegistration>, ConfigError> {
    use crate::features::music_library::library::{
        DEFAULT_LABEL, LibraryRegistration, is_label_token,
    };

    let mut entries: Vec<(String, std::path::PathBuf)> = Vec::new();

    for entry in raw.split(',') {
        let entry = entry.trim();
        if entry.is_empty() {
            continue;
        }

        let (label, path) = match entry.split_once('=') {
            Some((prefix, rest)) if is_label_token(prefix) => (prefix.to_owned(), rest.trim()),
            _ => (DEFAULT_LABEL.to_owned(), entry),
        };
        entries.push((label, std::path::PathBuf::from(path)));
    }

    if entries.is_empty() {
        return Err(invalid(
            MEDIA_ROOTS,
            raw,
            "must list at least one directory",
        ));
    }

    for (_, root) in &entries {
        if !root.is_absolute() {
            return Err(invalid(
                MEDIA_ROOTS,
                &root.display().to_string(),
                "must be an absolute path",
            ));
        }
    }

    let mut grouped: Vec<(String, Vec<std::path::PathBuf>)> = Vec::new();

    for (label, root) in entries {
        match grouped.iter_mut().find(|(existing, _)| *existing == label) {
            Some((_, roots)) => {
                if roots.contains(&root) {
                    return Err(invalid(
                        MEDIA_ROOTS,
                        &root.display().to_string(),
                        "must not list the same directory twice",
                    ));
                }
                roots.push(root);
            }
            None => grouped.push((label, vec![root])),
        }
    }

    Ok(grouped
        .into_iter()
        .map(|(label, roots)| LibraryRegistration::new(label, roots))
        .collect())
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
        use crate::features::music_library::catalogue::DedupMode;
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

    fn registered(config: &Config) -> Vec<(String, Vec<std::path::PathBuf>)> {
        config
            .libraries
            .iter()
            .map(|library| (library.label().to_owned(), library.roots().to_vec()))
            .collect()
    }

    #[test]
    fn media_roots_default_empty_and_accept_an_absolute_list() {
        let config = parse_config(&[]).unwrap();
        assert!(config.libraries.is_empty());
        assert!(config.media_roots().is_empty());

        let config = parse_config(&[(MEDIA_ROOTS, "/tank/data/media/music")]).unwrap();
        assert_eq!(
            registered(&config),
            vec![(
                "default".to_owned(),
                vec![std::path::PathBuf::from("/tank/data/media/music")]
            )]
        );
        assert_eq!(
            config.media_roots(),
            vec![std::path::PathBuf::from("/tank/data/media/music")]
        );

        let config = parse_config(&[(MEDIA_ROOTS, " /a , /b ")]).unwrap();
        assert_eq!(
            registered(&config),
            vec![(
                "default".to_owned(),
                vec![
                    std::path::PathBuf::from("/a"),
                    std::path::PathBuf::from("/b")
                ]
            )]
        );
        assert_eq!(
            config.media_roots(),
            vec![
                std::path::PathBuf::from("/a"),
                std::path::PathBuf::from("/b")
            ]
        );
    }

    #[test]
    fn media_roots_reject_relative_empty_and_duplicate_entries() {
        for value in [
            "",
            " , ",
            "relative/path",
            "/a,/a",
            "/a, /a",
            "music=/a,music=/a",
            "music=/a,music=/a,/b",
        ] {
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
    fn labelled_entries_register_one_library_each() {
        let config = parse_config(&[(MEDIA_ROOTS, "music=/a,books=/b,/c")]).unwrap();

        assert_eq!(
            registered(&config),
            vec![
                ("music".to_owned(), vec![std::path::PathBuf::from("/a")]),
                ("books".to_owned(), vec![std::path::PathBuf::from("/b")]),
                ("default".to_owned(), vec![std::path::PathBuf::from("/c")]),
            ]
        );
    }

    #[test]
    fn a_root_shared_by_two_labels_is_configurable_and_walked_once() {
        let config = parse_config(&[(MEDIA_ROOTS, "music=/a,books=/a")]).unwrap();

        assert_eq!(
            registered(&config),
            vec![
                ("music".to_owned(), vec![std::path::PathBuf::from("/a")]),
                ("books".to_owned(), vec![std::path::PathBuf::from("/a")]),
            ]
        );
        assert_eq!(config.media_roots(), vec![std::path::PathBuf::from("/a")]);
    }

    #[test]
    fn an_explicit_default_label_is_the_bare_library() {
        assert_eq!(
            registered(&parse_config(&[(MEDIA_ROOTS, "default=/a")]).unwrap()),
            vec![("default".to_owned(), vec![std::path::PathBuf::from("/a")])]
        );
        assert_eq!(
            registered(&parse_config(&[(MEDIA_ROOTS, "/a,default=/b")]).unwrap()),
            vec![(
                "default".to_owned(),
                vec![
                    std::path::PathBuf::from("/a"),
                    std::path::PathBuf::from("/b")
                ]
            )]
        );
    }

    #[test]
    fn label_boundaries_keep_a_path_or_reject_it() {
        let over_long = "a".repeat(101);
        for value in [
            "=/a".to_owned(),
            format!("{over_long}=/a"),
            "mu sic=/a".to_owned(),
            "/a=/b/c".to_owned(),
        ] {
            let config = parse_config(&[(MEDIA_ROOTS, &value)]);
            if value.starts_with('/') {
                assert_eq!(
                    registered(&config.unwrap()),
                    vec![("default".to_owned(), vec![std::path::PathBuf::from(&value)])],
                    "{value}"
                );
            } else {
                assert!(
                    matches!(
                        config,
                        Err(ConfigError::Invalid {
                            name: MEDIA_ROOTS,
                            ..
                        })
                    ),
                    "{value}"
                );
            }
        }
    }

    #[test]
    fn a_path_containing_an_equals_sign_is_a_path() {
        let config = parse_config(&[(MEDIA_ROOTS, "/tank/my=music")]).unwrap();

        assert_eq!(
            registered(&config),
            vec![(
                "default".to_owned(),
                vec![std::path::PathBuf::from("/tank/my=music")]
            )]
        );
    }

    #[test]
    fn nested_roots_register_under_one_library() {
        let config =
            parse_config(&[(MEDIA_ROOTS, "music=/tank/music,music=/tank/music/live")]).unwrap();

        assert_eq!(
            registered(&config),
            vec![(
                "music".to_owned(),
                vec![
                    std::path::PathBuf::from("/tank/music"),
                    std::path::PathBuf::from("/tank/music/live")
                ]
            )]
        );
        assert_eq!(
            config.media_roots(),
            vec![
                std::path::PathBuf::from("/tank/music"),
                std::path::PathBuf::from("/tank/music/live")
            ]
        );
    }

    #[test]
    fn worker_queues_parse_as_a_list_and_reject_a_malformed_one() {
        let config =
            parse_config(&[(WORKER_QUEUES, " whio.jobs.probe , whio.jobs.other ")]).unwrap();
        assert_eq!(
            config.worker.queues,
            vec!["whio.jobs.probe".to_owned(), "whio.jobs.other".to_owned()]
        );

        for value in ["", " , ", "whio.jobs.probe,whio.jobs.probe", "a, a"] {
            assert!(
                matches!(
                    parse_config(&[(WORKER_QUEUES, value)]),
                    Err(ConfigError::Invalid {
                        name: WORKER_QUEUES,
                        ..
                    })
                ),
                "{value}"
            );
        }
    }

    #[test]
    fn worker_limits_reject_absent_bounds() {
        for (name, value) in [
            (WORKER_BIND_ADDRESS, "not an address"),
            (WORKER_MAX_WORKERS, "0"),
            (WORKER_MAX_WORKERS, "many"),
            (WORKER_POLL_INTERVAL, "0"),
            (WORKER_PROMOTE_INTERVAL, "later"),
            (WORKER_DRAIN_TIMEOUT, "0"),
        ] {
            assert!(
                matches!(
                    parse_config(&[(name, value)]),
                    Err(ConfigError::Invalid { .. })
                ),
                "{name}={value}"
            );
        }
    }

    #[test]
    fn metadata_resolver_defaults_to_disabled_and_ignores_inactive_values() {
        let config = parse_config(&[
            (METADATA_RESOLVER_URL, ""),
            (METADATA_RESOLVER_CONNECT_TIMEOUT, "not a timeout"),
            (METADATA_RESOLVER_TOTAL_TIMEOUT, "1"),
        ]);

        assert!(config.is_err());
        assert!(matches!(
            parse_config(&[]).unwrap().metadata_resolver,
            MetadataResolverConfig::Disabled
        ));
    }

    #[test]
    fn an_enabled_metadata_resolver_uses_defaults_and_accepts_overrides() {
        let config =
            parse_config(&[(METADATA_RESOLVER_URL, "http://metadata.example.test")]).unwrap();
        assert!(matches!(
            config.metadata_resolver,
            MetadataResolverConfig::Enabled {
                connect_timeout,
                total_timeout,
                ..
            } if connect_timeout == Duration::from_secs(2)
                && total_timeout == Duration::from_secs(35)
        ));

        let config = parse_config(&[
            (METADATA_RESOLVER_URL, "https://metadata.example.test/v1"),
            (METADATA_RESOLVER_CONNECT_TIMEOUT, "4"),
            (METADATA_RESOLVER_TOTAL_TIMEOUT, "45"),
        ])
        .unwrap();
        assert!(matches!(
            config.metadata_resolver,
            MetadataResolverConfig::Enabled {
                url,
                connect_timeout,
                total_timeout,
            } if url.as_str() == "https://metadata.example.test/v1"
                && connect_timeout == Duration::from_secs(4)
                && total_timeout == Duration::from_secs(45)
        ));
    }

    #[test]
    fn an_enabled_metadata_resolver_requires_a_valid_url_and_timeouts() {
        for url in ["metadata.example.test", "file:///tmp/resolver", "http://"] {
            assert!(
                matches!(
                    parse_config(&[(METADATA_RESOLVER_URL, url)]),
                    Err(ConfigError::Invalid {
                        name: METADATA_RESOLVER_URL,
                        ..
                    })
                ),
                "{url}"
            );
        }

        for (name, value) in [
            (METADATA_RESOLVER_CONNECT_TIMEOUT, "0"),
            (METADATA_RESOLVER_CONNECT_TIMEOUT, "many"),
            (METADATA_RESOLVER_TOTAL_TIMEOUT, "34"),
            (METADATA_RESOLVER_TOTAL_TIMEOUT, "0"),
        ] {
            assert!(
                matches!(
                    parse_config(&[
                        (METADATA_RESOLVER_URL, "http://metadata.example.test"),
                        (name, value),
                    ]),
                    Err(ConfigError::Invalid { .. })
                ),
                "{name}={value}"
            );
        }

        assert!(
            parse_config(&[
                (METADATA_RESOLVER_URL, "http://metadata.example.test"),
                (METADATA_RESOLVER_CONNECT_TIMEOUT, "40"),
                (METADATA_RESOLVER_TOTAL_TIMEOUT, "35"),
            ])
            .is_err()
        );
    }

    #[test]
    fn the_metadata_store_budget_defaults_and_rejects_under_a_megabyte() {
        assert_eq!(
            parse_config(&[]).unwrap().metadata_store_budget_bytes,
            33_554_432
        );
        assert_eq!(
            parse_config(&[(METADATA_STORE_BUDGET_BYTES, "2097152")])
                .unwrap()
                .metadata_store_budget_bytes,
            2_097_152
        );

        for value in ["0", "1048575", "many", "-1"] {
            assert!(
                matches!(
                    parse_config(&[(METADATA_STORE_BUDGET_BYTES, value)]),
                    Err(ConfigError::Invalid {
                        name: METADATA_STORE_BUDGET_BYTES,
                        ..
                    })
                ),
                "{value}"
            );
        }
    }

    #[test]
    fn the_metadata_profile_and_language_default_and_reject_unknown_values() {
        let config = parse_config(&[]).unwrap();
        assert_eq!(config.metadata_artist_profile.as_str(), "artist-display.v1");
        assert_eq!(config.metadata_language, "en");

        let config = parse_config(&[
            (METADATA_ARTIST_PROFILE, "artist-display.v1"),
            (METADATA_LANGUAGE, " de "),
        ])
        .unwrap();
        assert_eq!(config.metadata_artist_profile.as_str(), "artist-display.v1");
        assert_eq!(config.metadata_language, "de");

        assert!(matches!(
            parse_config(&[(METADATA_ARTIST_PROFILE, "artist-display.v2")]),
            Err(ConfigError::Invalid {
                name: METADATA_ARTIST_PROFILE,
                ..
            })
        ));

        for value in ["", "   ", &"a".repeat(36)] {
            assert!(
                matches!(
                    parse_config(&[(METADATA_LANGUAGE, value)]),
                    Err(ConfigError::Invalid {
                        name: METADATA_LANGUAGE,
                        ..
                    })
                ),
                "{value}"
            );
        }
    }

    fn ordered(config: &Config, field: FieldId) -> Vec<String> {
        config.metadata_source_order.sources(field).to_vec()
    }

    #[test]
    fn the_source_order_defaults_to_the_per_field_tiers() {
        let config = parse_config(&[]).unwrap();

        assert_eq!(
            ordered(&config, FieldId::BiographyContent),
            vec!["youtube_music", "theaudiodb", "wikipedia", "lastfm"]
        );
        assert_eq!(
            ordered(&config, FieldId::ImagePortrait),
            vec!["theaudiodb", "commons"]
        );
        assert_eq!(
            ordered(&config, FieldId::MetricListen),
            vec!["listenbrainz"]
        );
        assert!(ordered(&config, FieldId::BiographyLanguage).is_empty());
    }

    #[test]
    fn a_configured_source_order_replaces_only_the_named_fields() {
        let config = parse_config(&[(
            METADATA_SOURCE_ORDER,
            " biography.content = wikipedia,theaudiodb ; tag=musicbrainz ",
        )])
        .unwrap();

        assert_eq!(
            ordered(&config, FieldId::BiographyContent),
            vec!["wikipedia", "theaudiodb"]
        );
        assert_eq!(ordered(&config, FieldId::Tag), vec!["musicbrainz"]);
        assert!(ordered(&config, FieldId::ImagePortrait).is_empty());
    }

    #[test]
    fn an_unknown_field_or_source_in_the_source_order_is_a_configuration_error() {
        for value in [
            "unknown.field=wikipedia",
            "biography.content=unknown_source",
            "biography.content",
            "=wikipedia",
            "biography.content=",
            "",
            "   ",
            "biography.content=wikipedia;biography.content=theaudiodb",
            "biography.content=wikipedia,wikipedia",
        ] {
            assert!(
                matches!(
                    parse_config(&[(METADATA_SOURCE_ORDER, value)]),
                    Err(ConfigError::Invalid {
                        name: METADATA_SOURCE_ORDER,
                        ..
                    })
                ),
                "{value}"
            );
        }
    }

    #[test]
    fn the_artwork_cache_is_disabled_without_a_directory_and_defaults_its_budget() {
        let config = parse_config(&[]).unwrap();
        assert!(config.artwork_cache_dir.is_none());
        assert_eq!(config.artwork_cache_bytes, 2 * 1024 * 1024 * 1024);

        let config = parse_config(&[
            (ARTWORK_CACHE_DIR, "/tank/whio/artwork"),
            (ARTWORK_CACHE_BYTES, "1073741824"),
        ])
        .unwrap();
        assert_eq!(
            config.artwork_cache_dir,
            Some(std::path::PathBuf::from("/tank/whio/artwork"))
        );
        assert_eq!(config.artwork_cache_bytes, 1_073_741_824);

        assert!(matches!(
            parse_config(&[(ARTWORK_CACHE_BYTES, "0")]),
            Err(ConfigError::Invalid {
                name: ARTWORK_CACHE_BYTES,
                ..
            })
        ));
        assert!(matches!(
            parse_config(&[(ARTWORK_CACHE_BYTES, "lots")]),
            Err(ConfigError::Invalid {
                name: ARTWORK_CACHE_BYTES,
                ..
            })
        ));
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
