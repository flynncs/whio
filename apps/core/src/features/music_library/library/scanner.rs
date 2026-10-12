use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use rayon::prelude::*;

use crate::features::music_library::{
    domain::{
        AlbumMetadata, AlbumTrack, ArtistCredit, ProviderId, SourceIdentity, SourceScope,
        TrackMetadata, ValidationError,
    },
    storage::contracts::{CatalogueWrite, SourceFileFacts, SourceLookup, TrackRepositoryError},
};

use super::tags::{TagError, content_type_for_container, read_tags};

const AUDIO_EXTENSIONS: &[&str] = &[
    "flac", "mp3", "m4a", "mp4", "ogg", "oga", "opus", "wav", "aiff", "aif", "ape", "wv", "mpc",
];

pub const LOCAL_PROVIDER: &str = "local";

const SOURCE_FACTS_VERSION: u16 = 3;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ScanReport {
    pub indexed: usize,
    pub unchanged: usize,
    pub untagged: usize,
    pub failed: usize,
    pub missing: usize,
    pub unwalked: usize,
    pub empty: usize,
}

#[derive(Debug, thiserror::Error)]
pub enum ScanError {
    #[error("media root could not be read")]
    Unreadable,

    #[error("track repository failed: {0}")]
    Repository(#[from] TrackRepositoryError),
}

struct FoundFile {
    absolute: PathBuf,
    relative: String,
    size_bytes: u64,
    modified_at: Option<chrono::DateTime<chrono::Utc>>,
    suffix: Option<String>,
}

enum Prepared {
    Unchanged {
        relative: String,
    },
    Untagged {
        relative: String,
        reason: SkipReason,
    },
    Failed {
        relative: String,
        reason: SkipReason,
    },
    Ready(Box<PreparedTrack>),
}

#[derive(Debug, Clone, Copy)]
enum SkipReason {
    NoTags,
    MissingTitle,
    MissingArtist,
    Unreadable,
}

struct PreparedTrack {
    relative: String,
    facts: SourceFileFacts,
    external_id: String,
    metadata: TrackMetadata,
    album: Option<AlbumMetadata>,
    membership: Option<AlbumTrack>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScanMode {
    Incremental,
    Full,
}

pub async fn scan(
    catalogue: &Arc<dyn CatalogueWrite>,
    sources: &Arc<dyn SourceLookup>,
    roots: &[PathBuf],
) -> Result<ScanReport, ScanError> {
    let progress = AtomicU32::new(0);

    scan_with(catalogue, sources, roots, ScanMode::Incremental, &progress).await
}

pub async fn scan_with(
    catalogue: &Arc<dyn CatalogueWrite>,
    sources: &Arc<dyn SourceLookup>,
    roots: &[PathBuf],
    mode: ScanMode,
    progress: &AtomicU32,
) -> Result<ScanReport, ScanError> {
    let provider = ProviderId::new(LOCAL_PROVIDER.to_owned())
        .expect("the local provider id is a valid identifier");
    let now = chrono::Utc::now();
    let mut report = ScanReport::default();
    let mut processed = 0u32;
    let empty = HashMap::new();

    for root in roots {
        let scope = SourceScope::Named(root.display().to_string());
        let known = known_paths(sources, &provider, &scope).await?;
        let Ok(found) = walk(root) else {
            report.unwalked += 1;
            continue;
        };
        if found.is_empty() {
            report.empty += 1;
            continue;
        }
        let mut seen = Vec::with_capacity(found.len());
        let lookup = if mode == ScanMode::Full {
            &empty
        } else {
            &known
        };

        let prepared: Vec<Prepared> = found
            .into_par_iter()
            .map(|file| prepare(lookup, file))
            .collect();

        for item in prepared {
            processed = processed.saturating_add(1);
            progress.store(processed, Ordering::SeqCst);

            match item {
                Prepared::Unchanged { relative } => {
                    seen.push(relative);
                    report.unchanged += 1;
                }
                Prepared::Untagged { relative, reason } => {
                    tracing::warn!(path = %relative, reason = ?reason, "library scan skipped an untagged file");
                    seen.push(relative);
                    report.untagged += 1;
                }
                Prepared::Failed { relative, reason } => {
                    tracing::warn!(path = %relative, reason = ?reason, "library scan failed to read a file");
                    seen.push(relative);
                    report.failed += 1;
                }
                Prepared::Ready(track) => {
                    let PreparedTrack {
                        relative,
                        facts,
                        external_id,
                        metadata,
                        album,
                        membership,
                    } = *track;

                    let source = SourceIdentity::new(provider.clone(), scope.clone(), external_id)
                        .expect("scanned identity is non-empty");

                    match index_track(catalogue, &source, &facts, metadata, album, membership).await
                    {
                        Ok(()) => {
                            seen.push(relative);
                            report.indexed += 1;
                        }
                        Err(error) => {
                            tracing::warn!(path = %relative, error = ?error, "library scan could not index a file");
                            seen.push(relative);
                            if matches!(
                                error,
                                ScanError::Repository(TrackRepositoryError::Unavailable)
                            ) {
                                return Err(error);
                            }
                            report.failed += 1;
                        }
                    }
                }
            }
        }

        report.missing += catalogue
            .reconcile_missing(&provider, &scope, &seen, now)
            .await?
            .marked_missing;
    }

    if !roots.is_empty() && report.unwalked == roots.len() {
        return Err(ScanError::Unreadable);
    }

    Ok(report)
}

async fn index_track(
    catalogue: &Arc<dyn CatalogueWrite>,
    source: &SourceIdentity,
    facts: &SourceFileFacts,
    metadata: TrackMetadata,
    album: Option<AlbumMetadata>,
    membership: Option<AlbumTrack>,
) -> Result<(), ScanError> {
    let stored = catalogue
        .index_source(source.clone(), metadata, facts)
        .await?;

    if let Some(album) = &album {
        let album = catalogue.resolve_or_create_album(album).await?;
        if let Some(membership) = membership {
            catalogue
                .set_album_track(album.id(), stored.id(), &membership)
                .await?;
        }
        catalogue.set_album_source(album.id(), source).await?;
    } else {
        catalogue.clear_album_track(stored.id()).await?;
    }

    Ok(())
}

fn prepare(known: &HashMap<String, SourceFileFacts>, file: FoundFile) -> Prepared {
    let mut facts = SourceFileFacts {
        path: Some(file.relative.clone()),
        size_bytes: file.size_bytes,
        modified_at: file.modified_at,
        missing_at: None,
        content_type: None,
        suffix: file.suffix,
        container: None,
        codec: None,
        bitrate_kbps: None,
        sample_rate_hz: None,
        bit_depth: None,
        channels: None,
        facts_version: SOURCE_FACTS_VERSION,
        acoustid_fingerprint: None,
    };

    if let Some(previous) = known.get(&file.relative)
        && previous.size_bytes == file.size_bytes
        && previous.modified_at.map(|time| time.timestamp_micros())
            == file.modified_at.map(|time| time.timestamp_micros())
        && previous.facts_version >= SOURCE_FACTS_VERSION
    {
        return Prepared::Unchanged {
            relative: file.relative,
        };
    }

    match read_tags(&file.absolute) {
        Ok(tags) => {
            let external_id = file.relative.clone();
            if tags.invalid_isrc {
                tracing::warn!(path = %file.relative, field = "isrc", "library scan dropped an invalid optional tag");
            }
            let album = match album_metadata(&tags) {
                Ok(album) => album,
                Err(()) => {
                    tracing::warn!(path = %file.relative, field = "album", "library scan dropped invalid album metadata");
                    None
                }
            };
            let membership = album.as_ref().and_then(|_| album_track(&tags));
            let acoustid_fingerprint = tags.acoustid_fingerprint.clone();
            let (primary, roles) = track_credits(&tags);

            if primary.is_empty() {
                return Prepared::Untagged {
                    relative: file.relative,
                    reason: SkipReason::MissingArtist,
                };
            }
            let base = match TrackMetadata::new(tags.title, primary, tags.duration_ms) {
                Ok(metadata) => metadata,
                Err(_) => {
                    return Prepared::Untagged {
                        relative: file.relative,
                        reason: SkipReason::MissingTitle,
                    };
                }
            };
            let metadata = optional_fact(base, &file.relative, "recording_id", |metadata| {
                metadata.with_recording_id(tags.recording_id)
            });
            let metadata = optional_fact(metadata, &file.relative, "isrc", |metadata| {
                metadata.with_isrc(tags.isrc)
            });
            let metadata = metadata
                .with_genres(tags.genres)
                .with_credits(roles)
                .with_recording_date(tags.recording_date)
                .with_original_date(tags.original_date);

            facts.acoustid_fingerprint = acoustid_fingerprint;
            facts.content_type = tags
                .container
                .as_deref()
                .and_then(content_type_for_container)
                .map(str::to_owned);
            facts.container = tags.container;
            facts.codec = tags.codec;
            facts.bitrate_kbps = tags.bitrate_kbps;
            facts.sample_rate_hz = tags.sample_rate_hz;
            facts.bit_depth = tags.bit_depth;
            facts.channels = tags.channels;

            Prepared::Ready(Box::new(PreparedTrack {
                relative: file.relative,
                facts,
                external_id,
                metadata,
                album,
                membership,
            }))
        }
        Err(TagError::Untagged) => Prepared::Untagged {
            relative: file.relative,
            reason: SkipReason::NoTags,
        },
        Err(TagError::MissingTitle) => Prepared::Untagged {
            relative: file.relative,
            reason: SkipReason::MissingTitle,
        },
        Err(TagError::Unreadable) => Prepared::Failed {
            relative: file.relative,
            reason: SkipReason::Unreadable,
        },
    }
}

fn album_metadata(tags: &super::TrackTags) -> Result<Option<AlbumMetadata>, ()> {
    let Some(title) = &tags.album else {
        return Ok(None);
    };

    let mut album = AlbumMetadata::new(title.clone()).map_err(|_| ())?;

    if let Some(album_artist) = &tags.album_artist {
        let credit = ArtistCredit::new(album_artist.clone(), tags.album_artist_id.clone())
            .map_err(|_| ())?;
        album = album.with_album_artist(credit);
    }

    album
        .with_year(tags.year)
        .and_then(|album| album.with_release_type(tags.release_type.clone()))
        .and_then(|album| album.with_label(tags.label.clone()))
        .and_then(|album| album.with_release_group_id(tags.release_group_id.clone()))
        .and_then(|album| album.with_release_id(tags.release_id.clone()))
        .map(|album| {
            album
                .with_release_date(tags.release_date)
                .with_release_country(tags.country.clone())
                .with_compilation(tags.compilation)
        })
        .map(Some)
        .map_err(|_| ())
}

fn album_track(tags: &super::TrackTags) -> Option<AlbumTrack> {
    let track_number = tags
        .track_number
        .and_then(|number| u16::try_from(number).ok())
        .filter(|number| *number >= 1);
    let disc_number = tags
        .disc_number
        .and_then(|number| u16::try_from(number).ok())
        .filter(|number| *number >= 1);
    let position = track_number.map_or(0, |number| u32::from(number) - 1);

    AlbumTrack::new(disc_number, track_number, position)
        .ok()
        .map(|membership| membership.with_totals(total(tags.disc_total), total(tags.track_total)))
}

fn total(value: Option<u32>) -> Option<u16> {
    value.and_then(|value| u16::try_from(value).ok())
}

fn track_credits(tags: &super::TrackTags) -> (Vec<ArtistCredit>, Vec<ArtistCredit>) {
    let primary = if !tags.artists.is_empty() {
        let ids = if tags.artists.len() == tags.artist_ids.len() {
            tags.artist_ids.as_slice()
        } else {
            &[]
        };
        tags.artists
            .iter()
            .enumerate()
            .filter_map(|(index, name)| {
                ArtistCredit::new(name.clone(), ids.get(index).cloned()).ok()
            })
            .collect()
    } else {
        tags.album_artist
            .as_ref()
            .and_then(|name| ArtistCredit::new(name.clone(), tags.album_artist_id.clone()).ok())
            .into_iter()
            .collect()
    };

    let roles = tags
        .credits
        .iter()
        .filter_map(|credit| {
            ArtistCredit::new(credit.name.clone(), credit.musicbrainz_artist_id.clone())
                .ok()
                .map(|artist| artist.with_role(credit.role))
        })
        .collect();

    (primary, roles)
}

fn optional_fact(
    metadata: TrackMetadata,
    path: &str,
    field: &'static str,
    apply: impl FnOnce(TrackMetadata) -> Result<TrackMetadata, ValidationError>,
) -> TrackMetadata {
    match apply(metadata.clone()) {
        Ok(applied) => applied,
        Err(_) => {
            tracing::warn!(path = %path, field, "library scan dropped an invalid optional tag");
            metadata
        }
    }
}

async fn known_paths(
    sources: &Arc<dyn SourceLookup>,
    provider: &ProviderId,
    scope: &SourceScope,
) -> Result<HashMap<String, SourceFileFacts>, ScanError> {
    let recorded = sources.list_scanned_paths(provider, scope).await?;
    Ok(recorded
        .into_iter()
        .filter_map(|(_, facts)| facts.path.clone().map(|path| (path, facts)))
        .collect())
}

fn walk(root: &Path) -> Result<Vec<FoundFile>, ScanError> {
    let mut found = Vec::new();

    for entry in jwalk::WalkDir::new(root) {
        let entry = entry.map_err(|_| ScanError::Unreadable)?;
        if !entry.file_type().is_file() {
            continue;
        }
        let absolute = entry.path();
        if !is_audio(&absolute) {
            continue;
        }

        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        let relative = absolute
            .strip_prefix(root)
            .unwrap_or(&absolute)
            .to_string_lossy()
            .into_owned();

        found.push(FoundFile {
            relative,
            size_bytes: metadata.len(),
            modified_at: metadata.modified().ok().map(chrono::DateTime::from),
            suffix: absolute
                .extension()
                .and_then(|extension| extension.to_str())
                .map(|extension| extension.to_ascii_lowercase()),
            absolute,
        });
    }

    found.sort_by(|left, right| left.relative.cmp(&right.relative));
    Ok(found)
}

fn is_audio(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .map(|extension| extension.to_ascii_lowercase())
        .is_some_and(|extension| AUDIO_EXTENSIONS.contains(&extension.as_str()))
}

#[cfg(test)]
mod tests {
    #[cfg(feature = "postgres-integration")]
    use super::super::tags::tagged_flac;
    use super::*;
    #[cfg(feature = "postgres-integration")]
    use crate::features::music_library::{
        storage::contracts::LibraryRead, storage::postgres::PostgresTrackRepository,
    };
    #[cfg(feature = "postgres-integration")]
    use lofty::prelude::ItemKey;
    #[cfg(feature = "postgres-integration")]
    use uuid::Uuid;

    fn temp_root(label: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("whio-{label}-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    #[test]
    fn audio_extensions_are_matched_case_insensitively() {
        assert!(is_audio(Path::new("/m/a.flac")));
        assert!(is_audio(Path::new("/m/a.FLAC")));
        assert!(is_audio(Path::new("/m/a.Mp3")));
        assert!(!is_audio(Path::new("/m/a.lrc")));
        assert!(!is_audio(Path::new("/m/a.jpg")));
        assert!(!is_audio(Path::new("/m/no-extension")));
    }

    #[test]
    fn walk_finds_audio_recursively_and_skips_sidecars() {
        let root = temp_root("walk");
        std::fs::create_dir_all(root.join("Artist/Album")).unwrap();
        std::fs::write(root.join("Artist/Album/01.flac"), b"x").unwrap();
        std::fs::write(root.join("Artist/Album/01.lrc"), b"x").unwrap();
        std::fs::write(root.join("Artist/Album/cover.jpg"), b"x").unwrap();
        std::fs::write(root.join("Artist/loose.mp3"), b"x").unwrap();

        let names = walk(&root)
            .unwrap()
            .iter()
            .map(|file| file.relative.clone())
            .collect::<Vec<_>>();

        assert_eq!(names, vec!["Artist/Album/01.flac", "Artist/loose.mp3"]);
        std::fs::remove_dir_all(&root).unwrap();
    }

    fn fixture() -> PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/tagged.flac")
    }

    #[cfg(feature = "postgres-integration")]
    fn wave_fixture() -> PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/tagged.wav")
    }

    #[cfg(feature = "postgres-integration")]
    async fn stored_facts(
        sources: &Arc<dyn SourceLookup>,
        root: &Path,
        relative: &str,
    ) -> SourceFileFacts {
        let source = local_identity(root, relative);

        sources
            .find_source_file_facts(&source)
            .await
            .unwrap()
            .expect("the scanned file is recorded")
    }

    #[cfg(feature = "postgres-integration")]
    fn local_identity(root: &Path, relative: &str) -> SourceIdentity {
        SourceIdentity::new(
            ProviderId::new(LOCAL_PROVIDER.to_owned()).unwrap(),
            SourceScope::Named(root.display().to_string()),
            relative.to_owned(),
        )
        .unwrap()
    }

    #[cfg(feature = "postgres-integration")]
    fn local_facts(
        relative: &str,
        missing_at: Option<chrono::DateTime<chrono::Utc>>,
    ) -> SourceFileFacts {
        SourceFileFacts {
            path: Some(relative.to_owned()),
            size_bytes: 0,
            modified_at: None,
            missing_at,
            content_type: None,
            suffix: Some("flac".to_owned()),
            container: None,
            codec: None,
            bitrate_kbps: None,
            sample_rate_hz: None,
            bit_depth: None,
            channels: None,
            facts_version: SOURCE_FACTS_VERSION,
            acoustid_fingerprint: None,
        }
    }

    #[cfg(feature = "postgres-integration")]
    async fn seed_local_row(
        catalogue: &Arc<dyn CatalogueWrite>,
        root: &Path,
        relative: &str,
        missing_at: Option<chrono::DateTime<chrono::Utc>>,
    ) {
        catalogue
            .index_source(
                local_identity(root, relative),
                TrackMetadata::from_names(
                    relative.to_owned(),
                    vec!["Seeded Artist".to_owned()],
                    Some(1_000),
                )
                .unwrap(),
                &local_facts(relative, missing_at),
            )
            .await
            .unwrap();
    }

    #[cfg(feature = "postgres-integration")]
    #[sqlx::test]
    async fn valid_isrc_and_required_facts_survive_invalid_optional_tags(pool: sqlx::PgPool) {
        use lofty::{
            config::WriteOptions,
            file::{AudioFile, TaggedFileExt},
        };

        let root = temp_root("isrc-ingest");
        tagged_flac(
            &root.join("isrc.flac"),
            &[(ItemKey::Isrc, "GBGLM0700129;USJI10900291;USK6G0700001")],
        );
        tagged_flac(
            &root.join("invalid-isrc.flac"),
            &[(ItemKey::Isrc, "not-an-isrc")],
        );
        let invalid_recording = "x".repeat(65);
        tagged_flac(
            &root.join("invalid-recording.flac"),
            &[(ItemKey::MusicBrainzRecordingId, invalid_recording.as_str())],
        );
        let missing_title = root.join("missing-title.flac");
        std::fs::copy(fixture(), &missing_title).unwrap();
        let mut tagged = lofty::probe::Probe::open(&missing_title)
            .unwrap()
            .read()
            .unwrap();
        let has_primary = tagged.primary_tag_mut().is_some();
        let tag = if has_primary {
            tagged.primary_tag_mut().unwrap()
        } else {
            tagged.first_tag_mut().unwrap()
        };
        tag.take(&ItemKey::TrackTitle).count();
        tagged
            .save_to_path(&missing_title, WriteOptions::default())
            .unwrap();

        let store = Arc::new(PostgresTrackRepository::new(pool.clone()));
        let catalogue = Arc::clone(&store) as Arc<dyn CatalogueWrite>;
        let sources = Arc::clone(&store) as Arc<dyn SourceLookup>;
        let report = scan(&catalogue, &sources, std::slice::from_ref(&root))
            .await
            .unwrap();

        assert_eq!(report.indexed, 3);
        assert_eq!(report.untagged, 1);
        let isrc_id = track_id_for(&pool, "isrc.flac").await;
        let isrc: Option<String> = sqlx::query_scalar("SELECT isrc FROM tracks WHERE id = $1")
            .bind(isrc_id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(isrc.as_deref(), Some("GBGLM0700129"));
        let no_isrc_id = track_id_for(&pool, "invalid-isrc.flac").await;
        let no_isrc: Option<String> = sqlx::query_scalar("SELECT isrc FROM tracks WHERE id = $1")
            .bind(no_isrc_id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(no_isrc, None);
        let invalid_id = track_id_for(&pool, "invalid-recording.flac").await;
        let recording_id: Option<String> =
            sqlx::query_scalar("SELECT musicbrainz_recording_id FROM tracks WHERE id = $1")
                .bind(invalid_id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(recording_id, None);

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[cfg(feature = "postgres-integration")]
    #[sqlx::test]
    async fn scan_records_representation_facts_from_the_file(pool: sqlx::PgPool) {
        let root = temp_root("representation");
        std::fs::copy(fixture(), root.join("a.flac")).unwrap();

        let store = Arc::new(PostgresTrackRepository::new(pool.clone()));
        let catalogue = Arc::clone(&store) as Arc<dyn CatalogueWrite>;
        let sources = Arc::clone(&store) as Arc<dyn SourceLookup>;

        let report = scan(&catalogue, &sources, std::slice::from_ref(&root))
            .await
            .unwrap();
        assert_eq!(report.indexed, 1);

        let facts = stored_facts(&sources, &root, "a.flac").await;
        assert_eq!(facts.container.as_deref(), Some("flac"));
        assert_eq!(facts.codec.as_deref(), Some("flac"));
        assert_eq!(facts.content_type.as_deref(), Some("audio/flac"));
        assert_eq!(facts.suffix.as_deref(), Some("flac"));
        assert_eq!(facts.bitrate_kbps, Some(123));
        assert_eq!(facts.sample_rate_hz, Some(44_100));
        assert_eq!(facts.bit_depth, Some(16));
        assert_eq!(facts.channels, Some(1));
        assert_eq!(facts.facts_version, SOURCE_FACTS_VERSION);

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[cfg(feature = "postgres-integration")]
    #[sqlx::test]
    async fn a_mislabelled_suffix_does_not_decide_the_container(pool: sqlx::PgPool) {
        let root = temp_root("mislabelled");
        std::fs::copy(fixture(), root.join("a.mp3")).unwrap();

        let store = Arc::new(PostgresTrackRepository::new(pool.clone()));
        let catalogue = Arc::clone(&store) as Arc<dyn CatalogueWrite>;
        let sources = Arc::clone(&store) as Arc<dyn SourceLookup>;

        let report = scan(&catalogue, &sources, std::slice::from_ref(&root))
            .await
            .unwrap();
        assert_eq!(report.indexed, 1);

        let facts = stored_facts(&sources, &root, "a.mp3").await;
        assert_eq!(facts.container.as_deref(), Some("flac"));
        assert_eq!(facts.codec.as_deref(), Some("flac"));
        assert_eq!(facts.content_type.as_deref(), Some("audio/flac"));
        assert_eq!(facts.suffix.as_deref(), Some("mp3"));
        assert_eq!(facts.bit_depth, Some(16));

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[cfg(feature = "postgres-integration")]
    #[sqlx::test]
    async fn facts_version_backfills_on_an_incremental_scan(pool: sqlx::PgPool) {
        let root = temp_root("backfill");
        std::fs::copy(fixture(), root.join("a.flac")).unwrap();

        let store = Arc::new(PostgresTrackRepository::new(pool.clone()));
        let catalogue = Arc::clone(&store) as Arc<dyn CatalogueWrite>;
        let sources = Arc::clone(&store) as Arc<dyn SourceLookup>;
        let source = SourceIdentity::new(
            ProviderId::new(LOCAL_PROVIDER.to_owned()).unwrap(),
            SourceScope::Named(root.display().to_string()),
            "a.flac".to_owned(),
        )
        .unwrap();

        let first = scan(&catalogue, &sources, std::slice::from_ref(&root))
            .await
            .unwrap();
        assert_eq!(first.indexed, 1);

        let current = stored_facts(&sources, &root, "a.flac").await;
        let stale = SourceFileFacts {
            content_type: None,
            container: None,
            codec: None,
            bitrate_kbps: None,
            sample_rate_hz: None,
            bit_depth: None,
            channels: None,
            facts_version: 0,
            ..current
        };
        catalogue
            .set_source_file_facts(&source, &stale)
            .await
            .unwrap();

        let second = scan(&catalogue, &sources, std::slice::from_ref(&root))
            .await
            .unwrap();
        assert_eq!(
            second.indexed, 1,
            "a row below the extractor version is re-read"
        );
        assert_eq!(second.unchanged, 0);

        let refreshed = stored_facts(&sources, &root, "a.flac").await;
        assert_eq!(refreshed.facts_version, SOURCE_FACTS_VERSION);
        assert_eq!(refreshed.container.as_deref(), Some("flac"));
        assert_eq!(refreshed.content_type.as_deref(), Some("audio/flac"));
        assert_eq!(refreshed.bitrate_kbps, Some(123));

        let third = scan(&catalogue, &sources, std::slice::from_ref(&root))
            .await
            .unwrap();
        assert_eq!(third.unchanged, 1);
        assert_eq!(third.indexed, 0);

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[cfg(feature = "postgres-integration")]
    #[sqlx::test]
    async fn absent_properties_stay_absent(pool: sqlx::PgPool) {
        let root = temp_root("absent");
        std::fs::copy(wave_fixture(), root.join("a.wav")).unwrap();

        let store = Arc::new(PostgresTrackRepository::new(pool.clone()));
        let catalogue = Arc::clone(&store) as Arc<dyn CatalogueWrite>;
        let sources = Arc::clone(&store) as Arc<dyn SourceLookup>;

        let report = scan(&catalogue, &sources, std::slice::from_ref(&root))
            .await
            .unwrap();
        assert_eq!(report.indexed, 1);

        let facts = stored_facts(&sources, &root, "a.wav").await;
        assert_eq!(facts.container.as_deref(), Some("wav"));
        assert_eq!(facts.content_type.as_deref(), Some("audio/wav"));
        assert_eq!(
            facts.codec, None,
            "a WAV container does not imply exactly one codec"
        );
        assert_eq!(facts.bitrate_kbps, Some(706));
        assert_eq!(facts.sample_rate_hz, Some(44_100));
        assert_eq!(facts.bit_depth, Some(16));
        assert_eq!(facts.channels, Some(1));
        assert_eq!(facts.facts_version, SOURCE_FACTS_VERSION);

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[cfg(feature = "postgres-integration")]
    #[sqlx::test]
    async fn a_second_scan_skips_unchanged_files_and_does_not_report_them_missing(
        pool: sqlx::PgPool,
    ) {
        let root = temp_root("skip");
        std::fs::copy(fixture(), root.join("a.flac")).unwrap();

        let store = Arc::new(PostgresTrackRepository::new(pool.clone()));
        let catalogue = Arc::clone(&store) as Arc<dyn CatalogueWrite>;
        let sources = Arc::clone(&store) as Arc<dyn SourceLookup>;

        let first = scan(&catalogue, &sources, std::slice::from_ref(&root))
            .await
            .unwrap();
        assert_eq!(first.indexed, 1);

        let second = scan(&catalogue, &sources, std::slice::from_ref(&root))
            .await
            .unwrap();
        assert_eq!(second.unchanged, 1);
        assert_eq!(second.indexed, 0);
        assert_eq!(
            second.missing, 0,
            "an unchanged file is still present on disk"
        );

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[cfg(feature = "postgres-integration")]
    #[sqlx::test]
    async fn a_missing_file_is_marked_and_counted_without_touching_the_track(pool: sqlx::PgPool) {
        let root = temp_root("missing");
        let file = root.join("a.flac");
        std::fs::copy(fixture(), &file).unwrap();
        std::fs::copy(fixture(), root.join("b.flac")).unwrap();

        let store = Arc::new(PostgresTrackRepository::new(pool.clone()));
        let catalogue = Arc::clone(&store) as Arc<dyn CatalogueWrite>;
        let sources = Arc::clone(&store) as Arc<dyn SourceLookup>;
        let first = scan(&catalogue, &sources, std::slice::from_ref(&root))
            .await
            .unwrap();
        assert_eq!(first.indexed, 2);

        std::fs::remove_file(&file).unwrap();
        let before = chrono::Utc::now();
        let second = scan(&catalogue, &sources, std::slice::from_ref(&root))
            .await
            .unwrap();
        let after = chrono::Utc::now();

        assert_eq!(second.missing, 1);
        assert_eq!(second.indexed, 0);

        let vanished = stored_facts(&sources, &root, "a.flac").await;
        let marked_at = vanished
            .missing_at
            .expect("a vanished file is marked missing");
        assert!(marked_at >= before && marked_at <= after);
        assert_eq!(
            stored_facts(&sources, &root, "b.flac").await.missing_at,
            None,
            "a file still on disk is not marked"
        );

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[cfg(feature = "postgres-integration")]
    #[sqlx::test]
    async fn an_already_missing_row_keeps_its_timestamp(pool: sqlx::PgPool) {
        let root = temp_root("already-missing");
        let file = root.join("a.flac");
        std::fs::copy(fixture(), &file).unwrap();
        std::fs::copy(fixture(), root.join("keep.flac")).unwrap();

        let store = Arc::new(PostgresTrackRepository::new(pool.clone()));
        let catalogue = Arc::clone(&store) as Arc<dyn CatalogueWrite>;
        let sources = Arc::clone(&store) as Arc<dyn SourceLookup>;
        scan(&catalogue, &sources, std::slice::from_ref(&root))
            .await
            .unwrap();

        std::fs::remove_file(&file).unwrap();
        let marked = scan(&catalogue, &sources, std::slice::from_ref(&root))
            .await
            .unwrap();
        assert_eq!(marked.missing, 1);
        let disappeared_at = stored_facts(&sources, &root, "a.flac")
            .await
            .missing_at
            .expect("marked by the scan that missed it");

        let again = scan(&catalogue, &sources, std::slice::from_ref(&root))
            .await
            .unwrap();

        assert_eq!(
            again.missing, 0,
            "the row was already marked by the earlier scan"
        );
        assert_eq!(
            stored_facts(&sources, &root, "a.flac").await.missing_at,
            Some(disappeared_at),
            "the disappearance date is a fact, not a heartbeat"
        );

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[cfg(feature = "postgres-integration")]
    #[sqlx::test]
    async fn a_reappeared_file_clears_the_mark_on_the_unchanged_fast_path(pool: sqlx::PgPool) {
        let root = temp_root("reappeared-unchanged");
        let file = root.join("a.flac");
        std::fs::copy(fixture(), &file).unwrap();
        std::fs::copy(fixture(), root.join("keep.flac")).unwrap();
        let indexed = std::fs::metadata(&file).unwrap();
        let bytes = std::fs::read(&file).unwrap();

        let store = Arc::new(PostgresTrackRepository::new(pool.clone()));
        let catalogue = Arc::clone(&store) as Arc<dyn CatalogueWrite>;
        let sources = Arc::clone(&store) as Arc<dyn SourceLookup>;
        scan(&catalogue, &sources, std::slice::from_ref(&root))
            .await
            .unwrap();
        assert_eq!(
            stored_facts(&sources, &root, "a.flac").await.size_bytes,
            indexed.len()
        );

        std::fs::remove_file(&file).unwrap();
        let missing = scan(&catalogue, &sources, std::slice::from_ref(&root))
            .await
            .unwrap();
        assert_eq!(missing.missing, 1);
        assert!(
            stored_facts(&sources, &root, "a.flac")
                .await
                .missing_at
                .is_some()
        );

        std::fs::write(&file, &bytes).unwrap();
        std::fs::File::options()
            .write(true)
            .open(&file)
            .unwrap()
            .set_modified(indexed.modified().unwrap())
            .unwrap();
        let restored = std::fs::metadata(&file).unwrap();
        assert_eq!(restored.len(), indexed.len());
        assert_eq!(restored.modified().unwrap(), indexed.modified().unwrap());

        let reappeared = scan(&catalogue, &sources, std::slice::from_ref(&root))
            .await
            .unwrap();

        assert_eq!(
            reappeared.unchanged, 2,
            "the restored file keeps its size and mtime, so it takes the fast path"
        );
        assert_eq!(reappeared.indexed, 0);
        assert_eq!(reappeared.missing, 0);

        let facts = stored_facts(&sources, &root, "a.flac").await;
        assert_eq!(
            facts.missing_at, None,
            "only the reconciliation can unmark an unchanged file"
        );
        assert_eq!(facts.size_bytes, indexed.len());
        assert_eq!(
            facts.modified_at.map(|time| time.timestamp_micros()),
            Some(
                chrono::DateTime::<chrono::Utc>::from(indexed.modified().unwrap())
                    .timestamp_micros()
            )
        );

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[cfg(feature = "postgres-integration")]
    #[sqlx::test]
    async fn a_reappeared_file_with_new_facts_clears_the_mark(pool: sqlx::PgPool) {
        let root = temp_root("reappeared-facts");
        let file = root.join("a.flac");
        std::fs::copy(fixture(), &file).unwrap();
        std::fs::copy(fixture(), root.join("keep.flac")).unwrap();

        let store = Arc::new(PostgresTrackRepository::new(pool.clone()));
        let catalogue = Arc::clone(&store) as Arc<dyn CatalogueWrite>;
        let sources = Arc::clone(&store) as Arc<dyn SourceLookup>;
        scan(&catalogue, &sources, std::slice::from_ref(&root))
            .await
            .unwrap();

        std::fs::remove_file(&file).unwrap();
        assert_eq!(
            scan(&catalogue, &sources, std::slice::from_ref(&root))
                .await
                .unwrap()
                .missing,
            1
        );

        std::fs::copy(wave_fixture(), &file).unwrap();
        let reappeared = scan(&catalogue, &sources, std::slice::from_ref(&root))
            .await
            .unwrap();

        assert_eq!(reappeared.indexed, 1);
        assert_eq!(reappeared.missing, 0);

        let facts = stored_facts(&sources, &root, "a.flac").await;
        assert_eq!(facts.missing_at, None);
        assert_eq!(facts.content_type.as_deref(), Some("audio/wav"));
        assert_eq!(facts.suffix.as_deref(), Some("flac"));

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[cfg(feature = "postgres-integration")]
    #[sqlx::test]
    async fn a_present_but_unusable_file_is_not_marked_missing(pool: sqlx::PgPool) {
        let root = temp_root("unusable");
        std::fs::copy(fixture(), root.join("a.flac")).unwrap();
        std::fs::copy(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/silence.flac"),
            root.join("silent.flac"),
        )
        .unwrap();
        std::fs::write(root.join("broken.flac"), b"not a container lofty can read").unwrap();

        let store = Arc::new(PostgresTrackRepository::new(pool.clone()));
        let catalogue = Arc::clone(&store) as Arc<dyn CatalogueWrite>;
        let sources = Arc::clone(&store) as Arc<dyn SourceLookup>;
        seed_local_row(&catalogue, &root, "silent.flac", None).await;
        seed_local_row(&catalogue, &root, "broken.flac", None).await;
        seed_local_row(&catalogue, &root, "gone.flac", None).await;

        let report = scan(&catalogue, &sources, std::slice::from_ref(&root))
            .await
            .unwrap();

        assert_eq!(report.indexed, 1);
        assert_eq!(report.untagged, 1);
        assert_eq!(report.failed, 1);
        assert_eq!(
            report.missing, 1,
            "only the path the walk did not see is missing"
        );
        assert_eq!(
            stored_facts(&sources, &root, "silent.flac")
                .await
                .missing_at,
            None,
            "an untagged file the walk saw is present, not missing"
        );
        assert_eq!(
            stored_facts(&sources, &root, "broken.flac")
                .await
                .missing_at,
            None,
            "an unreadable file the walk saw is present, not missing"
        );
        assert!(
            stored_facts(&sources, &root, "gone.flac")
                .await
                .missing_at
                .is_some()
        );

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[cfg(feature = "postgres-integration")]
    #[sqlx::test]
    async fn an_empty_root_marks_nothing(pool: sqlx::PgPool) {
        let root = temp_root("emptied");
        std::fs::copy(fixture(), root.join("a.flac")).unwrap();
        std::fs::copy(fixture(), root.join("b.flac")).unwrap();

        let store = Arc::new(PostgresTrackRepository::new(pool.clone()));
        let catalogue = Arc::clone(&store) as Arc<dyn CatalogueWrite>;
        let sources = Arc::clone(&store) as Arc<dyn SourceLookup>;
        let first = scan(&catalogue, &sources, std::slice::from_ref(&root))
            .await
            .unwrap();
        assert_eq!(first.indexed, 2);

        std::fs::remove_file(root.join("a.flac")).unwrap();
        std::fs::remove_file(root.join("b.flac")).unwrap();
        let emptied = scan(&catalogue, &sources, std::slice::from_ref(&root))
            .await
            .unwrap();

        assert_eq!(emptied.empty, 1);
        assert_eq!(emptied.unwalked, 0);
        assert_eq!(
            emptied.missing, 0,
            "a walk that found no file is not evidence that the root is gone"
        );
        assert_eq!(
            stored_facts(&sources, &root, "a.flac").await.missing_at,
            None
        );
        assert_eq!(
            stored_facts(&sources, &root, "b.flac").await.missing_at,
            None
        );

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[cfg(feature = "postgres-integration")]
    #[sqlx::test]
    async fn no_configured_root_is_a_no_op(pool: sqlx::PgPool) {
        let store = Arc::new(PostgresTrackRepository::new(pool));
        let catalogue = Arc::clone(&store) as Arc<dyn CatalogueWrite>;
        let sources = Arc::clone(&store) as Arc<dyn SourceLookup>;

        let report = scan(&catalogue, &sources, &[]).await.unwrap();

        assert_eq!(report, ScanReport::default());
    }

    #[cfg(feature = "postgres-integration")]
    #[sqlx::test]
    async fn an_unwalked_root_marks_nothing(pool: sqlx::PgPool) {
        let root = temp_root("walked");
        std::fs::copy(fixture(), root.join("a.flac")).unwrap();

        let holder = temp_root("unwalked");
        let blocker = holder.join("not-a-directory");
        std::fs::write(&blocker, b"not a directory").unwrap();
        let unreadable = blocker.join("music");

        let store = Arc::new(PostgresTrackRepository::new(pool.clone()));
        let catalogue = Arc::clone(&store) as Arc<dyn CatalogueWrite>;
        let sources = Arc::clone(&store) as Arc<dyn SourceLookup>;

        let already: chrono::DateTime<chrono::Utc> = "2026-09-01T00:00:00Z".parse().unwrap();
        seed_local_row(&catalogue, &unreadable, "absent.flac", Some(already)).await;
        seed_local_row(&catalogue, &unreadable, "present.flac", None).await;

        let report = scan(&catalogue, &sources, &[root.clone(), unreadable.clone()])
            .await
            .unwrap();

        assert_eq!(report.unwalked, 1);
        assert_eq!(report.missing, 0);
        assert_eq!(report.indexed, 1);
        assert_eq!(
            stored_facts(&sources, &unreadable, "absent.flac")
                .await
                .missing_at,
            Some(already)
        );
        assert_eq!(
            stored_facts(&sources, &unreadable, "present.flac")
                .await
                .missing_at,
            None
        );
        assert_eq!(
            stored_facts(&sources, &root, "a.flac").await.missing_at,
            None
        );

        std::fs::remove_dir_all(&root).unwrap();
        std::fs::remove_dir_all(&holder).unwrap();
    }

    #[cfg(feature = "postgres-integration")]
    #[sqlx::test]
    async fn a_root_that_disappears_entirely_marks_nothing(pool: sqlx::PgPool) {
        let kept = temp_root("kept-root");
        std::fs::copy(fixture(), kept.join("a.flac")).unwrap();
        let vanishing = temp_root("vanishing-root");
        std::fs::copy(fixture(), vanishing.join("b1.flac")).unwrap();
        std::fs::copy(fixture(), vanishing.join("b2.flac")).unwrap();

        let store = Arc::new(PostgresTrackRepository::new(pool.clone()));
        let catalogue = Arc::clone(&store) as Arc<dyn CatalogueWrite>;
        let sources = Arc::clone(&store) as Arc<dyn SourceLookup>;
        let roots = [kept.clone(), vanishing.clone()];

        let first = scan(&catalogue, &sources, &roots).await.unwrap();
        assert_eq!(first.indexed, 3);

        std::fs::remove_file(vanishing.join("b1.flac")).unwrap();
        let second = scan(&catalogue, &sources, &roots).await.unwrap();
        assert_eq!(second.missing, 1);
        let disappeared_at = stored_facts(&sources, &vanishing, "b1.flac")
            .await
            .missing_at
            .expect("marked before the root vanished");

        std::fs::remove_dir_all(&vanishing).unwrap();
        let third = scan(&catalogue, &sources, &roots).await.unwrap();

        assert_eq!(third.unwalked, 1);
        assert_eq!(third.missing, 0);
        assert_eq!(third.unchanged, 1);
        assert_eq!(third.indexed, 0);
        assert_eq!(
            stored_facts(&sources, &vanishing, "b1.flac")
                .await
                .missing_at,
            Some(disappeared_at),
            "a root that is gone is unknown, not missing"
        );
        assert_eq!(
            stored_facts(&sources, &vanishing, "b2.flac")
                .await
                .missing_at,
            None
        );
        assert_eq!(
            stored_facts(&sources, &kept, "a.flac").await.missing_at,
            None
        );

        std::fs::remove_dir_all(&kept).unwrap();
    }

    #[cfg(feature = "postgres-integration")]
    #[sqlx::test]
    async fn every_root_failing_is_an_error(pool: sqlx::PgPool) {
        let store = Arc::new(PostgresTrackRepository::new(pool));
        let catalogue = Arc::clone(&store) as Arc<dyn CatalogueWrite>;
        let sources = Arc::clone(&store) as Arc<dyn SourceLookup>;
        let absent = |label: &str| {
            std::env::temp_dir().join(format!("whio-{label}-{}", uuid::Uuid::now_v7()))
        };

        let result = scan(&catalogue, &sources, &[absent("gone-a"), absent("gone-b")]).await;

        assert!(matches!(result, Err(ScanError::Unreadable)));

        let single = scan(&catalogue, &sources, &[absent("gone-c")]).await;
        assert!(matches!(single, Err(ScanError::Unreadable)));
    }

    #[cfg(feature = "postgres-integration")]
    #[sqlx::test]
    async fn the_reconciliation_is_scoped_to_its_root_and_provider(pool: sqlx::PgPool) {
        let first = temp_root("scoped-first");
        std::fs::copy(fixture(), first.join("a.flac")).unwrap();
        std::fs::copy(fixture(), first.join("keep.flac")).unwrap();
        let second = temp_root("scoped-second");
        std::fs::copy(fixture(), second.join("b.flac")).unwrap();

        let store = Arc::new(PostgresTrackRepository::new(pool.clone()));
        let catalogue = Arc::clone(&store) as Arc<dyn CatalogueWrite>;
        let sources = Arc::clone(&store) as Arc<dyn SourceLookup>;

        let remote = SourceIdentity::new(
            ProviderId::new("youtube_music".to_owned()).unwrap(),
            SourceScope::Named(first.display().to_string()),
            "video-1".to_owned(),
        )
        .unwrap();
        catalogue
            .get_or_create(
                remote.clone(),
                TrackMetadata::from_names(
                    "Remote".to_owned(),
                    vec!["Remote Artist".to_owned()],
                    None,
                )
                .unwrap(),
            )
            .await
            .unwrap();

        let roots = [first.clone(), second.clone()];
        let initial = scan(&catalogue, &sources, &roots).await.unwrap();
        assert_eq!(initial.indexed, 3);

        std::fs::remove_file(first.join("a.flac")).unwrap();
        let report = scan(&catalogue, &sources, &roots).await.unwrap();

        assert_eq!(report.missing, 1);
        assert!(
            stored_facts(&sources, &first, "a.flac")
                .await
                .missing_at
                .is_some()
        );
        assert_eq!(
            stored_facts(&sources, &second, "b.flac").await.missing_at,
            None,
            "another root's rows are not this root's to mark"
        );

        let remote_facts = sources
            .find_source_file_facts(&remote)
            .await
            .unwrap()
            .expect("the provider source is recorded");
        assert_eq!(remote_facts.path, None);
        assert_eq!(remote_facts.missing_at, None);

        std::fs::remove_dir_all(&first).unwrap();
        std::fs::remove_dir_all(&second).unwrap();
    }

    #[cfg(feature = "postgres-integration")]
    #[sqlx::test]
    async fn one_scan_uses_one_timestamp(pool: sqlx::PgPool) {
        let first = temp_root("stamped-first");
        std::fs::copy(fixture(), first.join("a.flac")).unwrap();
        std::fs::copy(fixture(), first.join("keep.flac")).unwrap();
        let second = temp_root("stamped-second");
        std::fs::copy(fixture(), second.join("b.flac")).unwrap();
        std::fs::copy(fixture(), second.join("keep.flac")).unwrap();

        let store = Arc::new(PostgresTrackRepository::new(pool.clone()));
        let catalogue = Arc::clone(&store) as Arc<dyn CatalogueWrite>;
        let sources = Arc::clone(&store) as Arc<dyn SourceLookup>;
        let roots = [first.clone(), second.clone()];
        scan(&catalogue, &sources, &roots).await.unwrap();

        std::fs::remove_file(first.join("a.flac")).unwrap();
        std::fs::remove_file(second.join("b.flac")).unwrap();
        let before = chrono::Utc::now();
        let report = scan(&catalogue, &sources, &roots).await.unwrap();
        let after = chrono::Utc::now();

        assert_eq!(report.missing, 2);
        let left = stored_facts(&sources, &first, "a.flac")
            .await
            .missing_at
            .expect("marked");
        let right = stored_facts(&sources, &second, "b.flac")
            .await
            .missing_at
            .expect("marked");

        assert_eq!(left, right);
        assert!(left >= before && left <= after);

        std::fs::remove_dir_all(&first).unwrap();
        std::fs::remove_dir_all(&second).unwrap();
    }

    #[cfg(feature = "postgres-integration")]
    #[sqlx::test]
    async fn the_report_counts_agree_with_the_rows_that_changed(pool: sqlx::PgPool) {
        let root = temp_root("counts");
        for name in ["a.flac", "b.flac", "c.flac"] {
            std::fs::copy(fixture(), root.join(name)).unwrap();
        }
        let absent = std::env::temp_dir().join(format!("whio-gone-{}", uuid::Uuid::now_v7()));
        let roots = [root.clone(), absent];

        let store = Arc::new(PostgresTrackRepository::new(pool.clone()));
        let catalogue = Arc::clone(&store) as Arc<dyn CatalogueWrite>;
        let sources = Arc::clone(&store) as Arc<dyn SourceLookup>;
        let provider = ProviderId::new(LOCAL_PROVIDER.to_owned()).unwrap();
        let scope = SourceScope::Named(root.display().to_string());

        let initial = scan(&catalogue, &sources, &roots).await.unwrap();
        assert_eq!(initial.indexed, 3);
        assert_eq!(
            initial.unwalked, 1,
            "the second root never existed and is skipped"
        );

        std::fs::remove_file(root.join("a.flac")).unwrap();
        std::fs::remove_file(root.join("b.flac")).unwrap();
        let second = scan(&catalogue, &sources, &roots).await.unwrap();

        let marked = |rows: Vec<(SourceIdentity, SourceFileFacts)>| {
            rows.into_iter()
                .filter(|(_, facts)| facts.missing_at.is_some())
                .count()
        };
        let rows = sources.list_scanned_paths(&provider, &scope).await.unwrap();

        assert_eq!(second.missing, 2);
        assert_eq!(second.missing, marked(rows));
        assert_eq!(second.unwalked, 1);

        let third = scan(&catalogue, &sources, &roots).await.unwrap();
        let rows = sources.list_scanned_paths(&provider, &scope).await.unwrap();

        assert_eq!(third.missing, 0, "an already marked row changes nothing");
        assert_eq!(marked(rows), 2);
        assert_eq!(third.unwalked, 1);

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn the_fixture_reads_back_every_field_the_scanner_needs() {
        let tags = read_tags(&fixture()).unwrap();

        assert_eq!(tags.title, "Fixture Song");
        assert_eq!(tags.artists, vec!["Fixture Artist".to_owned()]);
        assert_eq!(tags.album.as_deref(), Some("Fixture Album"));
        assert_eq!(tags.album_artist.as_deref(), Some("Fixture Artist"));
        assert_eq!(tags.track_number, Some(3));
        assert_eq!(tags.disc_number, Some(1));
        assert_eq!(tags.year, Some(2024));
        assert_eq!(tags.genres, vec!["Test".to_owned()]);
        assert_eq!(
            tags.recording_id.as_deref(),
            Some("11111111-2222-3333-4444-555555555555")
        );
        assert!(tags.duration_ms.is_some_and(|ms| ms > 0));
        assert_eq!(tags.container.as_deref(), Some("flac"));
        assert_eq!(tags.codec.as_deref(), Some("flac"));
        assert_eq!(tags.bitrate_kbps, Some(123));
        assert_eq!(tags.sample_rate_hz, Some(44_100));
        assert_eq!(tags.bit_depth, Some(16));
        assert_eq!(tags.channels, Some(1));
    }

    #[cfg(feature = "postgres-integration")]
    #[sqlx::test]
    async fn an_untagged_file_is_counted_rather_than_indexed(pool: sqlx::PgPool) {
        let root = temp_root("untagged");
        std::fs::copy(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/silence.flac"),
            root.join("a.flac"),
        )
        .unwrap();
        let store = Arc::new(PostgresTrackRepository::new(pool.clone()));
        let catalogue = Arc::clone(&store) as Arc<dyn CatalogueWrite>;
        let sources = Arc::clone(&store) as Arc<dyn SourceLookup>;

        let report = scan(&catalogue, &sources, std::slice::from_ref(&root))
            .await
            .unwrap();

        assert_eq!(report.untagged, 1);
        assert_eq!(report.indexed, 0);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[tokio::test]
    async fn a_missing_root_is_an_error() {
        let missing = std::env::temp_dir().join(format!("whio-x-{}", uuid::Uuid::now_v7()));
        assert!(matches!(walk(&missing), Err(ScanError::Unreadable)));
    }

    #[cfg(feature = "postgres-integration")]
    #[sqlx::test]
    async fn a_scanned_file_populates_its_album_and_membership(pool: sqlx::PgPool) {
        let root = temp_root("album");
        std::fs::copy(fixture(), root.join("a.flac")).unwrap();
        let store = Arc::new(PostgresTrackRepository::new(pool.clone()));
        let catalogue = Arc::clone(&store) as Arc<dyn CatalogueWrite>;
        let sources = Arc::clone(&store) as Arc<dyn SourceLookup>;

        let report = scan(&catalogue, &sources, std::slice::from_ref(&root))
            .await
            .unwrap();
        assert_eq!(report.indexed, 1);

        let tags = read_tags(&fixture()).unwrap();
        let expected = album_metadata(&tags).unwrap().unwrap();
        let album = catalogue.resolve_or_create_album(&expected).await.unwrap();

        let album_sources = sources.find_album_sources(album.id()).await.unwrap();
        assert_eq!(album_sources.len(), 1);
        assert_eq!(album_sources[0].provider_id().as_str(), "local");

        let tracks = (store.as_ref() as &dyn LibraryRead)
            .find_album_entries(album.id(), None)
            .await
            .unwrap();
        assert_eq!(tracks.len(), 1);
        assert_eq!(tracks[0].membership().disc_number(), Some(1));
        assert_eq!(tracks[0].membership().track_number(), Some(3));

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_track_without_an_artist_tag_falls_back_to_the_album_artist() {
        use lofty::{
            config::WriteOptions,
            file::{AudioFile, TaggedFileExt},
            tag::ItemKey,
        };

        let root = temp_root("no-artist");
        let path = root.join("a.flac");
        std::fs::copy(fixture(), &path).unwrap();

        let mut tagged = lofty::probe::Probe::open(&path).unwrap().read().unwrap();
        {
            let has_primary = tagged.primary_tag_mut().is_some();
            let tag = if has_primary {
                tagged.primary_tag_mut().expect("checked above")
            } else {
                tagged.first_tag_mut().expect("the fixture carries a tag")
            };
            tag.take(&ItemKey::TrackArtist).count();
            tag.take(&ItemKey::TrackArtists).count();
        }
        tagged.save_to_path(&path, WriteOptions::default()).unwrap();

        let found = std::fs::metadata(&path).unwrap();
        let prepared = prepare(
            &std::collections::HashMap::new(),
            FoundFile {
                absolute: path,
                relative: "a.flac".to_owned(),
                size_bytes: found.len(),
                modified_at: found.modified().ok().map(chrono::DateTime::from),
                suffix: Some("flac".to_owned()),
            },
        );

        match prepared {
            Prepared::Ready(track) => {
                assert_eq!(
                    track
                        .metadata
                        .artists()
                        .iter()
                        .map(|artist| artist.name())
                        .collect::<Vec<_>>(),
                    ["Fixture Artist"]
                );
            }
            _ => panic!("expected a prepared track"),
        }

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_track_credit_carries_its_musicbrainz_id() {
        use lofty::{
            config::WriteOptions,
            file::{AudioFile, TaggedFileExt},
            tag::ItemKey,
        };

        let root = temp_root("credit-id");
        let path = root.join("a.flac");
        std::fs::copy(fixture(), &path).unwrap();

        let mut tagged = lofty::probe::Probe::open(&path).unwrap().read().unwrap();
        {
            let has_primary = tagged.primary_tag_mut().is_some();
            let tag = if has_primary {
                tagged.primary_tag_mut().expect("checked above")
            } else {
                tagged.first_tag_mut().expect("the fixture carries a tag")
            };
            tag.take(&ItemKey::MusicBrainzArtistId).count();
            tag.insert_text(ItemKey::MusicBrainzArtistId, "artist-mb-1".to_owned());
        }
        tagged.save_to_path(&path, WriteOptions::default()).unwrap();

        let found = std::fs::metadata(&path).unwrap();
        let prepared = prepare(
            &std::collections::HashMap::new(),
            FoundFile {
                absolute: path,
                relative: "a.flac".to_owned(),
                size_bytes: found.len(),
                modified_at: found.modified().ok().map(chrono::DateTime::from),
                suffix: Some("flac".to_owned()),
            },
        );

        match prepared {
            Prepared::Ready(track) => {
                let artists = track.metadata.artists();
                assert_eq!(artists.len(), 1);
                assert_eq!(artists[0].musicbrainz_artist_id(), Some("artist-mb-1"));
            }
            _ => panic!("expected a prepared track"),
        }

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[cfg(feature = "postgres-integration")]
    async fn store(pool: &sqlx::PgPool) -> (Arc<dyn CatalogueWrite>, Arc<dyn SourceLookup>) {
        let store = Arc::new(PostgresTrackRepository::new(pool.clone()));
        let catalogue = Arc::clone(&store) as Arc<dyn CatalogueWrite>;
        let sources = Arc::clone(&store) as Arc<dyn SourceLookup>;

        (catalogue, sources)
    }

    #[cfg(feature = "postgres-integration")]
    async fn track_id_for(pool: &sqlx::PgPool, relative: &str) -> Uuid {
        sqlx::query_scalar("SELECT track_id FROM track_sources WHERE external_id = $1")
            .bind(relative)
            .fetch_one(pool)
            .await
            .unwrap()
    }

    #[cfg(feature = "postgres-integration")]
    async fn stored_genres(pool: &sqlx::PgPool, track_id: Uuid) -> Vec<(i32, String)> {
        sqlx::query_as(
            "SELECT position, name FROM track_genres WHERE track_id = $1 ORDER BY position",
        )
        .bind(track_id)
        .fetch_all(pool)
        .await
        .unwrap()
    }

    #[cfg(feature = "postgres-integration")]
    async fn genre_projection(pool: &sqlx::PgPool, track_id: Uuid) -> Option<String> {
        sqlx::query_scalar("SELECT genre FROM tracks WHERE id = $1")
            .bind(track_id)
            .fetch_one(pool)
            .await
            .unwrap()
    }

    #[cfg(feature = "postgres-integration")]
    async fn stored_credits(
        pool: &sqlx::PgPool,
        track_id: Uuid,
    ) -> Vec<(i32, String, Option<String>)> {
        sqlx::query_as(
            "SELECT position, name, role FROM track_artist_credits WHERE track_id = $1 \
             ORDER BY position",
        )
        .bind(track_id)
        .fetch_all(pool)
        .await
        .unwrap()
    }

    #[cfg(feature = "postgres-integration")]
    async fn stored_credit_ids(pool: &sqlx::PgPool, track_id: Uuid) -> Vec<(i32, Option<String>)> {
        sqlx::query_as(
            "SELECT credits.position, artists.musicbrainz_artist_id \
             FROM track_artist_credits AS credits \
             JOIN artists ON artists.id = credits.artist_id \
             WHERE credits.track_id = $1 ORDER BY credits.position",
        )
        .bind(track_id)
        .fetch_all(pool)
        .await
        .unwrap()
    }

    #[cfg(feature = "postgres-integration")]
    async fn stored_dates(
        pool: &sqlx::PgPool,
        track_id: Uuid,
    ) -> (
        Option<chrono::NaiveDate>,
        Option<String>,
        Option<chrono::NaiveDate>,
        Option<String>,
    ) {
        sqlx::query_as(
            "SELECT recording_date, recording_date_precision, original_date, \
             original_date_precision FROM tracks WHERE id = $1",
        )
        .bind(track_id)
        .fetch_one(pool)
        .await
        .unwrap()
    }

    #[cfg(feature = "postgres-integration")]
    async fn album_facts(
        pool: &sqlx::PgPool,
        album_id: Uuid,
    ) -> (
        Option<chrono::NaiveDate>,
        Option<String>,
        Option<String>,
        Option<bool>,
        Option<i16>,
    ) {
        sqlx::query_as(
            "SELECT release_date, release_date_precision, release_country, compilation, year \
             FROM albums WHERE id = $1",
        )
        .bind(album_id)
        .fetch_one(pool)
        .await
        .unwrap()
    }

    #[cfg(feature = "postgres-integration")]
    async fn album_id_for(pool: &sqlx::PgPool, track_id: Uuid) -> Uuid {
        sqlx::query_scalar("SELECT album_id FROM album_tracks WHERE track_id = $1")
            .bind(track_id)
            .fetch_one(pool)
            .await
            .unwrap()
    }

    #[cfg(feature = "postgres-integration")]
    #[sqlx::test]
    async fn multi_valued_genres_round_trip(pool: sqlx::PgPool) {
        let root = temp_root("multi-genre");
        tagged_flac(
            &root.join("a.flac"),
            &[
                (ItemKey::Genre, "Rock"),
                (ItemKey::Genre, "Pop"),
                (ItemKey::Genre, "Jazz"),
            ],
        );
        let (catalogue, sources) = store(&pool).await;

        let report = scan(&catalogue, &sources, std::slice::from_ref(&root))
            .await
            .unwrap();
        assert_eq!(report.indexed, 1);

        let track_id = track_id_for(&pool, "a.flac").await;
        assert_eq!(
            stored_genres(&pool, track_id).await,
            vec![
                (0, "Rock".to_owned()),
                (1, "Pop".to_owned()),
                (2, "Jazz".to_owned())
            ]
        );
        assert_eq!(
            genre_projection(&pool, track_id).await.as_deref(),
            Some("Rock")
        );

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[cfg(feature = "postgres-integration")]
    #[sqlx::test]
    async fn retag_replaces_the_genre_set(pool: sqlx::PgPool) {
        let root = temp_root("retag");
        let file = root.join("a.flac");
        tagged_flac(&file, &[(ItemKey::Genre, "Rock"), (ItemKey::Genre, "Pop")]);
        let (catalogue, sources) = store(&pool).await;

        scan(&catalogue, &sources, std::slice::from_ref(&root))
            .await
            .unwrap();
        let track_id = track_id_for(&pool, "a.flac").await;
        assert_eq!(stored_genres(&pool, track_id).await.len(), 2);

        tagged_flac(&file, &[(ItemKey::Genre, "Jazz")]);
        scan(&catalogue, &sources, std::slice::from_ref(&root))
            .await
            .unwrap();
        assert_eq!(
            stored_genres(&pool, track_id).await,
            vec![(0, "Jazz".to_owned())]
        );
        assert_eq!(
            genre_projection(&pool, track_id).await.as_deref(),
            Some("Jazz")
        );

        tagged_flac(&file, &[]);
        scan(&catalogue, &sources, std::slice::from_ref(&root))
            .await
            .unwrap();
        assert!(stored_genres(&pool, track_id).await.is_empty());
        assert_eq!(genre_projection(&pool, track_id).await, None);

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[cfg(feature = "postgres-integration")]
    #[sqlx::test]
    async fn duplicate_normalised_genres_collapse(pool: sqlx::PgPool) {
        let root = temp_root("genre-dedupe");
        tagged_flac(
            &root.join("a.flac"),
            &[
                (ItemKey::Genre, "Rock"),
                (ItemKey::Genre, "rock "),
                (ItemKey::Genre, "ROCK"),
            ],
        );
        let (catalogue, sources) = store(&pool).await;

        scan(&catalogue, &sources, std::slice::from_ref(&root))
            .await
            .unwrap();
        let track_id = track_id_for(&pool, "a.flac").await;

        assert_eq!(
            stored_genres(&pool, track_id).await,
            vec![(0, "Rock".to_owned())]
        );
        assert_eq!(
            genre_projection(&pool, track_id).await.as_deref(),
            Some("Rock")
        );

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[cfg(feature = "postgres-integration")]
    #[sqlx::test]
    async fn credit_roles_are_stored_and_ordered(pool: sqlx::PgPool) {
        let root = temp_root("credit-roles");
        tagged_flac(
            &root.join("a.flac"),
            &[
                (ItemKey::TrackArtist, "Primary One"),
                (ItemKey::TrackArtist, "Primary Two"),
                (ItemKey::Composer, "Clara Schumann"),
                (ItemKey::Performer, "Bill Evans (piano)"),
                (ItemKey::Composer, "Bill Evans"),
                (ItemKey::Producer, "Bill Evans"),
            ],
        );
        let (catalogue, sources) = store(&pool).await;

        scan(&catalogue, &sources, std::slice::from_ref(&root))
            .await
            .unwrap();
        let track_id = track_id_for(&pool, "a.flac").await;

        assert_eq!(
            stored_credits(&pool, track_id).await,
            vec![
                (0, "Primary One".to_owned(), None),
                (1, "Primary Two".to_owned(), None),
                (2, "Clara Schumann".to_owned(), Some("composer".to_owned())),
                (3, "Bill Evans".to_owned(), Some("composer".to_owned())),
                (4, "Bill Evans".to_owned(), Some("performer".to_owned())),
                (5, "Bill Evans".to_owned(), Some("producer".to_owned())),
            ]
        );

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[cfg(feature = "postgres-integration")]
    #[sqlx::test]
    async fn a_multi_artist_file_credits_every_artist(pool: sqlx::PgPool) {
        let root = temp_root("multi-artist");
        let multi = root.join("a.flac");
        tagged_flac(
            &multi,
            &[
                (ItemKey::TrackArtist, "beabadoobee & Hayley Williams"),
                (ItemKey::TrackArtists, "beabadoobee"),
                (ItemKey::TrackArtists, "Hayley Williams"),
                (ItemKey::MusicBrainzArtistId, "mb-beabadoobee"),
                (ItemKey::MusicBrainzArtistId, "mb-hayley"),
            ],
        );
        tagged_flac(
            &root.join("b.flac"),
            &[
                (ItemKey::TrackArtists, "Alice Coltrane"),
                (ItemKey::TrackArtists, "Pharoah Sanders"),
                (ItemKey::MusicBrainzArtistId, "mb-alice"),
            ],
        );
        let (catalogue, sources) = store(&pool).await;

        scan(&catalogue, &sources, std::slice::from_ref(&root))
            .await
            .unwrap();

        let matched = track_id_for(&pool, "a.flac").await;
        assert_eq!(
            stored_credits(&pool, matched).await,
            vec![
                (0, "beabadoobee".to_owned(), None),
                (1, "Hayley Williams".to_owned(), None),
            ],
            "the credited display string is not a performer"
        );
        assert_eq!(
            stored_credit_ids(&pool, matched).await,
            vec![
                (0, Some("mb-beabadoobee".to_owned())),
                (1, Some("mb-hayley".to_owned())),
            ]
        );

        let mismatched = track_id_for(&pool, "b.flac").await;
        assert_eq!(
            stored_credits(&pool, mismatched).await,
            vec![
                (0, "Alice Coltrane".to_owned(), None),
                (1, "Pharoah Sanders".to_owned(), None),
            ]
        );
        assert_eq!(
            stored_credit_ids(&pool, mismatched).await,
            vec![(0, None), (1, None)],
            "an id count that does not match the performers leaves every performer unnamed"
        );

        tagged_flac(&multi, &[(ItemKey::TrackArtist, "beabadoobee")]);
        scan(&catalogue, &sources, std::slice::from_ref(&root))
            .await
            .unwrap();
        assert_eq!(
            stored_credits(&pool, matched).await,
            vec![(0, "beabadoobee".to_owned(), None)],
            "a rescan replaces the stored credits"
        );

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[cfg(feature = "postgres-integration")]
    #[sqlx::test]
    async fn unknown_role_tags_are_not_mapped(pool: sqlx::PgPool) {
        let root = temp_root("unknown-role");
        tagged_flac(
            &root.join("a.flac"),
            &[
                (ItemKey::TrackArtist, "Fixture Artist"),
                (ItemKey::Unknown("OTHERROLE".to_owned()), "Nobody"),
                (ItemKey::Unknown("MOOD".to_owned()), "chill"),
            ],
        );
        let (catalogue, sources) = store(&pool).await;

        scan(&catalogue, &sources, std::slice::from_ref(&root))
            .await
            .unwrap();
        let track_id = track_id_for(&pool, "a.flac").await;

        assert_eq!(
            stored_credits(&pool, track_id).await,
            vec![(0, "Fixture Artist".to_owned(), None)]
        );

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[cfg(feature = "postgres-integration")]
    #[sqlx::test]
    async fn dates_keep_their_precision(pool: sqlx::PgPool) {
        let root = temp_root("dates");
        tagged_flac(
            &root.join("a.flac"),
            &[
                (ItemKey::AlbumTitle, "Dated Album"),
                (ItemKey::RecordingDate, "1987"),
                (ItemKey::OriginalReleaseDate, "1979-05"),
                (ItemKey::ReleaseDate, "2024-05-06"),
            ],
        );
        tagged_flac(
            &root.join("b.flac"),
            &[
                (ItemKey::AlbumTitle, "Undated Album"),
                (ItemKey::RecordingDate, "sometime"),
            ],
        );
        let (catalogue, sources) = store(&pool).await;

        scan(&catalogue, &sources, std::slice::from_ref(&root))
            .await
            .unwrap();

        let dated = track_id_for(&pool, "a.flac").await;
        let (recording, recording_precision, original, original_precision) =
            stored_dates(&pool, dated).await;
        assert_eq!(
            recording,
            Some(chrono::NaiveDate::from_ymd_opt(1987, 1, 1).unwrap())
        );
        assert_eq!(
            recording_precision.as_deref(),
            Some("year"),
            "a year-only tag must not claim a day"
        );
        assert_eq!(
            original,
            Some(chrono::NaiveDate::from_ymd_opt(1979, 5, 1).unwrap())
        );
        assert_eq!(original_precision.as_deref(), Some("month"));

        let album = album_id_for(&pool, dated).await;
        let (release_date, release_precision, ..) = album_facts(&pool, album).await;
        assert_eq!(
            release_date,
            Some(chrono::NaiveDate::from_ymd_opt(2024, 5, 6).unwrap())
        );
        assert_eq!(release_precision.as_deref(), Some("day"));

        let undated = track_id_for(&pool, "b.flac").await;
        assert_eq!(
            stored_dates(&pool, undated).await,
            (None, None, None, None),
            "an unparsable date is absence, not a fabricated day"
        );

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[cfg(feature = "postgres-integration")]
    #[sqlx::test]
    async fn album_year_agrees_with_release_date(pool: sqlx::PgPool) {
        let root = temp_root("album-year");
        tagged_flac(
            &root.join("a.flac"),
            &[
                (ItemKey::AlbumTitle, "Dated Album"),
                (ItemKey::Year, "2024"),
                (ItemKey::ReleaseDate, "2024-05-06"),
            ],
        );
        tagged_flac(
            &root.join("b.flac"),
            &[
                (ItemKey::AlbumTitle, "Year Only Album"),
                (ItemKey::Year, "2019"),
            ],
        );
        let (catalogue, sources) = store(&pool).await;

        scan(&catalogue, &sources, std::slice::from_ref(&root))
            .await
            .unwrap();

        let dated = album_id_for(&pool, track_id_for(&pool, "a.flac").await).await;
        let (release_date, _, _, _, year) = album_facts(&pool, dated).await;
        assert_eq!(
            release_date,
            Some(chrono::NaiveDate::from_ymd_opt(2024, 5, 6).unwrap())
        );
        assert_eq!(year, Some(2024));

        let year_only = album_id_for(&pool, track_id_for(&pool, "b.flac").await).await;
        let (release_date, precision, _, _, year) = album_facts(&pool, year_only).await;
        assert_eq!(year, Some(2019));
        assert_eq!(release_date, None);
        assert_eq!(precision, None);

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[cfg(feature = "postgres-integration")]
    #[sqlx::test]
    async fn membership_totals_round_trip(pool: sqlx::PgPool) {
        let root = temp_root("totals");
        tagged_flac(
            &root.join("a.flac"),
            &[
                (ItemKey::AlbumTitle, "Totalled Album"),
                (ItemKey::TrackNumber, "3"),
                (ItemKey::DiscNumber, "1"),
                (ItemKey::TrackTotal, "12"),
                (ItemKey::DiscTotal, "2"),
            ],
        );
        tagged_flac(
            &root.join("b.flac"),
            &[
                (ItemKey::AlbumTitle, "Totalled Album"),
                (ItemKey::TrackNumber, "4"),
            ],
        );
        let (catalogue, sources) = store(&pool).await;
        let store = Arc::new(PostgresTrackRepository::new(pool.clone()));

        scan(&catalogue, &sources, std::slice::from_ref(&root))
            .await
            .unwrap();

        let album = album_id_for(&pool, track_id_for(&pool, "a.flac").await).await;
        let entries = (store.as_ref() as &dyn LibraryRead)
            .find_album_entries(
                &crate::features::music_library::domain::AlbumId::from_uuid(album),
                None,
            )
            .await
            .unwrap();
        let totalled = entries
            .iter()
            .find(|entry| {
                entry.track().title() == "Fixture Song"
                    && entry.membership().track_number() == Some(3)
            })
            .expect("the totalled membership");
        assert_eq!(totalled.membership().disc_total(), Some(2));
        assert_eq!(totalled.membership().track_total(), Some(12));

        let absent = entries
            .iter()
            .find(|entry| entry.membership().track_number() == Some(4))
            .expect("the membership without totals");
        assert_eq!(absent.membership().disc_total(), None);
        assert_eq!(absent.membership().track_total(), None);

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[cfg(feature = "postgres-integration")]
    #[sqlx::test]
    async fn country_and_compilation_are_persisted(pool: sqlx::PgPool) {
        let root = temp_root("album-country");
        tagged_flac(
            &root.join("a.flac"),
            &[
                (ItemKey::AlbumTitle, "Stated Album"),
                (ItemKey::Unknown("RELEASECOUNTRY".to_owned()), "GB"),
                (ItemKey::FlagCompilation, "1"),
            ],
        );
        tagged_flac(
            &root.join("b.flac"),
            &[(ItemKey::AlbumTitle, "Silent Album")],
        );
        let (catalogue, sources) = store(&pool).await;

        scan(&catalogue, &sources, std::slice::from_ref(&root))
            .await
            .unwrap();

        let stated = album_id_for(&pool, track_id_for(&pool, "a.flac").await).await;
        let (_, _, country, compilation, _) = album_facts(&pool, stated).await;
        assert_eq!(country.as_deref(), Some("GB"));
        assert_eq!(compilation, Some(true));

        let silent = album_id_for(&pool, track_id_for(&pool, "b.flac").await).await;
        let (_, _, country, compilation, _) = album_facts(&pool, silent).await;
        assert_eq!(country, None);
        assert_eq!(compilation, None, "an unstated compilation is not false");

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[cfg(feature = "postgres-integration")]
    #[sqlx::test]
    async fn absent_facts_stay_absent(pool: sqlx::PgPool) {
        let root = temp_root("absent-facts");
        tagged_flac(&root.join("a.flac"), &[(ItemKey::AlbumTitle, "Bare Album")]);
        let (catalogue, sources) = store(&pool).await;

        scan(&catalogue, &sources, std::slice::from_ref(&root))
            .await
            .unwrap();
        let track_id = track_id_for(&pool, "a.flac").await;

        assert!(stored_genres(&pool, track_id).await.is_empty());
        assert_eq!(genre_projection(&pool, track_id).await, None);
        assert_eq!(
            stored_credits(&pool, track_id).await,
            vec![(0, "Fixture Artist".to_owned(), None)],
            "no empty-string role and no invented credit"
        );
        assert_eq!(
            stored_dates(&pool, track_id).await,
            (None, None, None, None)
        );

        let album = album_id_for(&pool, track_id).await;
        let (release_date, precision, country, compilation, _) = album_facts(&pool, album).await;
        assert_eq!(release_date, None);
        assert_eq!(precision, None);
        assert_eq!(country, None);
        assert_eq!(compilation, None);

        let totals: (Option<i16>, Option<i16>) =
            sqlx::query_as("SELECT disc_total, track_total FROM album_tracks WHERE track_id = $1")
                .bind(track_id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(totals, (None, None));

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[cfg(feature = "postgres-integration")]
    #[sqlx::test]
    async fn the_new_facts_survive_an_unchanged_rescan(pool: sqlx::PgPool) {
        let root = temp_root("facts-backfill");
        tagged_flac(
            &root.join("a.flac"),
            &[
                (ItemKey::Genre, "Rock"),
                (ItemKey::Composer, "Clara Schumann"),
            ],
        );
        let (catalogue, sources) = store(&pool).await;
        let source = SourceIdentity::new(
            ProviderId::new(LOCAL_PROVIDER.to_owned()).unwrap(),
            SourceScope::Named(root.display().to_string()),
            "a.flac".to_owned(),
        )
        .unwrap();

        let first = scan(&catalogue, &sources, std::slice::from_ref(&root))
            .await
            .unwrap();
        assert_eq!(first.indexed, 1);
        let track_id = track_id_for(&pool, "a.flac").await;
        assert_eq!(stored_genres(&pool, track_id).await.len(), 1);

        let current = sources
            .find_source_file_facts(&source)
            .await
            .unwrap()
            .expect("the scanned file is recorded");
        assert_eq!(current.facts_version, SOURCE_FACTS_VERSION);
        assert_eq!(SOURCE_FACTS_VERSION, 3);

        catalogue
            .set_source_file_facts(
                &source,
                &SourceFileFacts {
                    facts_version: 2,
                    ..current
                },
            )
            .await
            .unwrap();
        sqlx::query("DELETE FROM track_genres WHERE track_id = $1")
            .bind(track_id)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("DELETE FROM track_artist_credits WHERE track_id = $1 AND role IS NOT NULL")
            .bind(track_id)
            .execute(&pool)
            .await
            .unwrap();

        let second = scan(&catalogue, &sources, std::slice::from_ref(&root))
            .await
            .unwrap();
        assert_eq!(
            second.indexed, 1,
            "a row below the extractor version is re-read"
        );
        assert_eq!(
            stored_genres(&pool, track_id).await,
            vec![(0, "Rock".to_owned())]
        );
        assert_eq!(
            stored_credits(&pool, track_id).await,
            vec![
                (0, "Fixture Artist".to_owned(), None),
                (1, "Clara Schumann".to_owned(), Some("composer".to_owned())),
            ]
        );

        let third = scan(&catalogue, &sources, std::slice::from_ref(&root))
            .await
            .unwrap();
        assert_eq!(third.unchanged, 1);
        assert_eq!(third.indexed, 0);

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[cfg(feature = "postgres-integration")]
    #[sqlx::test]
    async fn an_over_long_or_invalid_value_is_dropped_not_fatal(pool: sqlx::PgPool) {
        let root = temp_root("invalid-values");
        let over_long_genre = "g".repeat(201);
        let over_long_performer = "p".repeat(501);
        tagged_flac(
            &root.join("a.flac"),
            &[
                (ItemKey::Genre, over_long_genre.as_str()),
                (ItemKey::Genre, "Jazz"),
                (ItemKey::Performer, over_long_performer.as_str()),
                (ItemKey::Performer, " (piano)"),
                (ItemKey::Composer, "Clara Schumann"),
                (ItemKey::RecordingDate, "not a date"),
            ],
        );
        let (catalogue, sources) = store(&pool).await;

        let report = scan(&catalogue, &sources, std::slice::from_ref(&root))
            .await
            .unwrap();
        assert_eq!(report.indexed, 1, "an invalid value must not lose the row");
        assert_eq!(report.untagged, 0);

        let track_id = track_id_for(&pool, "a.flac").await;
        assert_eq!(
            stored_genres(&pool, track_id).await,
            vec![(0, "Jazz".to_owned())]
        );
        assert_eq!(
            stored_credits(&pool, track_id).await,
            vec![
                (0, "Fixture Artist".to_owned(), None),
                (1, "Clara Schumann".to_owned(), Some("composer".to_owned())),
            ]
        );
        assert_eq!(
            stored_dates(&pool, track_id).await,
            (None, None, None, None)
        );

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[cfg(feature = "postgres-integration")]
    #[sqlx::test]
    async fn a_genre_list_beyond_the_bound_is_truncated(pool: sqlx::PgPool) {
        let root = temp_root("genre-truncation");
        let names = (0..18)
            .map(|index| format!("genre-{index}"))
            .collect::<Vec<_>>();
        let items = names
            .iter()
            .map(|name| (ItemKey::Genre, name.as_str()))
            .collect::<Vec<_>>();
        tagged_flac(&root.join("a.flac"), &items);
        let (catalogue, sources) = store(&pool).await;

        scan(&catalogue, &sources, std::slice::from_ref(&root))
            .await
            .unwrap();
        let track_id = track_id_for(&pool, "a.flac").await;
        let stored = stored_genres(&pool, track_id).await;

        assert_eq!(stored.len(), 16);
        assert_eq!(stored[0], (0, "genre-0".to_owned()));
        assert_eq!(stored[15], (15, "genre-15".to_owned()));
        assert_eq!(
            genre_projection(&pool, track_id).await.as_deref(),
            Some("genre-0")
        );

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[cfg(feature = "postgres-integration")]
    #[sqlx::test]
    async fn a_re_observed_album_gains_the_new_facts(pool: sqlx::PgPool) {
        let root = temp_root("album-reobserve");
        tagged_flac(
            &root.join("a.flac"),
            &[
                (ItemKey::AlbumTitle, "Shared Album"),
                (ItemKey::Year, "2024"),
            ],
        );
        let (catalogue, sources) = store(&pool).await;

        scan(&catalogue, &sources, std::slice::from_ref(&root))
            .await
            .unwrap();
        let album = album_id_for(&pool, track_id_for(&pool, "a.flac").await).await;
        let (release_date, _, country, compilation, year) = album_facts(&pool, album).await;
        assert_eq!(release_date, None);
        assert_eq!(country, None);
        assert_eq!(compilation, None);
        assert_eq!(year, Some(2024));

        tagged_flac(
            &root.join("b.flac"),
            &[
                (ItemKey::AlbumTitle, "Shared Album"),
                (ItemKey::ReleaseDate, "1999-08-02"),
                (ItemKey::Unknown("RELEASECOUNTRY".to_owned()), "US"),
                (ItemKey::FlagCompilation, "1"),
            ],
        );
        scan(&catalogue, &sources, std::slice::from_ref(&root))
            .await
            .unwrap();

        let second = album_id_for(&pool, track_id_for(&pool, "b.flac").await).await;
        assert_eq!(second, album, "the text-matched album is re-observed");
        let (release_date, precision, country, compilation, year) = album_facts(&pool, album).await;
        assert_eq!(
            release_date,
            Some(chrono::NaiveDate::from_ymd_opt(1999, 8, 2).unwrap())
        );
        assert_eq!(precision.as_deref(), Some("day"));
        assert_eq!(country.as_deref(), Some("US"));
        assert_eq!(compilation, Some(true));
        assert_eq!(
            year,
            Some(1999),
            "the year is re-derived from the release date"
        );

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[cfg(feature = "postgres-integration")]
    #[sqlx::test]
    async fn a_re_observed_album_with_a_release_id_gains_the_new_facts(pool: sqlx::PgPool) {
        let root = temp_root("album-reobserve-id");
        tagged_flac(
            &root.join("a.flac"),
            &[
                (ItemKey::AlbumTitle, "Shared Album"),
                (ItemKey::Year, "2024"),
                (ItemKey::MusicBrainzReleaseId, "release-1"),
            ],
        );
        let (catalogue, sources) = store(&pool).await;

        scan(&catalogue, &sources, std::slice::from_ref(&root))
            .await
            .unwrap();
        let album = album_id_for(&pool, track_id_for(&pool, "a.flac").await).await;
        let (release_date, _, country, compilation, year) = album_facts(&pool, album).await;
        assert_eq!(release_date, None);
        assert_eq!(country, None);
        assert_eq!(compilation, None);
        assert_eq!(year, Some(2024));

        tagged_flac(
            &root.join("b.flac"),
            &[
                (ItemKey::AlbumTitle, "Shared Album"),
                (ItemKey::MusicBrainzReleaseId, "release-1"),
                (ItemKey::ReleaseDate, "1999-08-02"),
                (ItemKey::Unknown("RELEASECOUNTRY".to_owned()), "US"),
                (ItemKey::FlagCompilation, "1"),
            ],
        );
        scan(&catalogue, &sources, std::slice::from_ref(&root))
            .await
            .unwrap();

        let second = album_id_for(&pool, track_id_for(&pool, "b.flac").await).await;
        assert_eq!(second, album, "the release id matches the stored album");
        let (release_date, precision, country, compilation, year) = album_facts(&pool, album).await;
        assert_eq!(
            release_date,
            Some(chrono::NaiveDate::from_ymd_opt(1999, 8, 2).unwrap())
        );
        assert_eq!(precision.as_deref(), Some("day"));
        assert_eq!(country.as_deref(), Some("US"));
        assert_eq!(compilation, Some(true));
        assert_eq!(
            year,
            Some(1999),
            "the year is re-derived from the release date"
        );

        std::fs::remove_dir_all(&root).unwrap();
    }
}
