use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use rayon::prelude::*;

use crate::tracks::{
    AlbumMetadata, AlbumTrack, ArtistCredit, ProviderId, SourceFileFacts, SourceIdentity,
    SourceScope, TrackMetadata, TrackRepository, TrackRepositoryError,
};

use super::tags::{TagError, identity_for, read_tags};

const AUDIO_EXTENSIONS: &[&str] = &[
    "flac", "mp3", "m4a", "mp4", "ogg", "oga", "opus", "wav", "aiff", "aif", "ape", "wv", "mpc",
];

pub const LOCAL_PROVIDER: &str = "local";

#[derive(Debug, Default, PartialEq, Eq)]
pub struct ScanReport {
    pub indexed: usize,
    pub unchanged: usize,
    pub untagged: usize,
    pub failed: usize,
    pub missing: usize,
}

#[derive(Debug, thiserror::Error)]
pub enum ScanError {
    #[error("media root could not be read")]
    Unreadable,

    #[error("track repository failed")]
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
    Unchanged { relative: String },
    Untagged,
    Failed,
    Ready(Box<PreparedTrack>),
}

struct PreparedTrack {
    relative: String,
    facts: SourceFileFacts,
    external_id: String,
    metadata: TrackMetadata,
    album: Option<AlbumMetadata>,
    membership: Option<AlbumTrack>,
}

pub async fn scan(
    repository: &Arc<dyn TrackRepository>,
    roots: &[PathBuf],
) -> Result<ScanReport, ScanError> {
    let provider = ProviderId::new(LOCAL_PROVIDER.to_owned())
        .expect("the local provider id is a valid identifier");
    let mut report = ScanReport::default();

    for root in roots {
        let scope = SourceScope::Named(root.display().to_string());
        let known = known_paths(repository, &provider, &scope).await?;
        let found = walk(root)?;
        let mut seen = Vec::with_capacity(found.len());

        let prepared: Vec<Prepared> = found
            .into_par_iter()
            .map(|file| prepare(&known, file))
            .collect();

        for item in prepared {
            match item {
                Prepared::Unchanged { relative } => {
                    seen.push(relative);
                    report.unchanged += 1;
                }
                Prepared::Untagged => report.untagged += 1,
                Prepared::Failed => report.failed += 1,
                Prepared::Ready(track) => {
                    let PreparedTrack {
                        relative,
                        facts,
                        external_id,
                        metadata,
                        album,
                        membership,
                    } = *track;
                    seen.push(relative);

                    let source = SourceIdentity::new(provider.clone(), scope.clone(), external_id)
                        .expect("scanned identity is non-empty");
                    let stored = repository.get_or_create(source.clone(), metadata).await?;
                    repository.set_source_file_facts(&source, &facts).await?;

                    if let Some(album) = &album {
                        let album = repository.resolve_or_create_album(album).await?;
                        if let Some(membership) = membership {
                            repository
                                .set_album_track(album.id(), stored.id(), &membership)
                                .await?;
                        }
                        repository.set_album_source(album.id(), &source).await?;
                    }

                    report.indexed += 1;
                }
            }
        }

        report.missing += mark_missing(&known, &seen);
    }

    Ok(report)
}

fn prepare(known: &HashMap<String, SourceFileFacts>, file: FoundFile) -> Prepared {
    let mut facts = SourceFileFacts {
        path: Some(file.relative.clone()),
        size_bytes: file.size_bytes,
        modified_at: file.modified_at,
        missing_at: None,
        content_type: None,
        suffix: file.suffix,
        acoustid_fingerprint: None,
    };

    if let Some(previous) = known.get(&file.relative)
        && previous.size_bytes == file.size_bytes
        && previous.modified_at == file.modified_at
    {
        return Prepared::Unchanged {
            relative: file.relative,
        };
    }

    match read_tags(&file.absolute) {
        Ok(tags) => {
            let (_, external_id) = identity_for(&tags, &file.relative);
            let album = match album_metadata(&tags) {
                Ok(album) => album,
                Err(()) => return Prepared::Untagged,
            };
            let membership = album_track(&tags);
            let acoustid_fingerprint = tags.acoustid_fingerprint.clone();

            let Ok(metadata) = TrackMetadata::new(tags.title, tags.artists, tags.duration_ms)
                .and_then(|metadata| metadata.with_recording_id(tags.recording_id))
                .and_then(|metadata| metadata.with_isrc(tags.isrc))
            else {
                return Prepared::Untagged;
            };

            facts.acoustid_fingerprint = acoustid_fingerprint;

            Prepared::Ready(Box::new(PreparedTrack {
                relative: file.relative,
                facts,
                external_id,
                metadata,
                album,
                membership,
            }))
        }
        Err(TagError::Untagged) => Prepared::Untagged,
        Err(TagError::Unreadable) => Prepared::Failed,
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
        .map(Some)
        .map_err(|_| ())
}

fn album_track(tags: &super::TrackTags) -> Option<AlbumTrack> {
    let track_number = u16::try_from(tags.track_number?).ok()?;
    let disc_number = tags.disc_number.and_then(|disc| u16::try_from(disc).ok());
    let position = u32::from(track_number).saturating_sub(1);

    AlbumTrack::new(disc_number, Some(track_number), position).ok()
}

async fn known_paths(
    repository: &Arc<dyn TrackRepository>,
    provider: &ProviderId,
    scope: &SourceScope,
) -> Result<HashMap<String, SourceFileFacts>, ScanError> {
    let recorded = repository.list_scanned_paths(provider, scope).await?;
    Ok(recorded
        .into_iter()
        .filter_map(|(_, facts)| facts.path.clone().map(|path| (path, facts)))
        .collect())
}

fn mark_missing(known: &HashMap<String, SourceFileFacts>, seen: &[String]) -> usize {
    let present: std::collections::HashSet<&str> = seen.iter().map(String::as_str).collect();
    known
        .iter()
        .filter(|(path, facts)| !present.contains(path.as_str()) && facts.missing_at.is_none())
        .count()
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
    use super::super::IdentityRung;
    use super::*;
    use crate::tracks::InMemoryTrackRepository;

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

    #[tokio::test]
    async fn a_second_scan_skips_unchanged_files_and_does_not_report_them_missing() {
        let root = temp_root("skip");
        std::fs::copy(fixture(), root.join("a.flac")).unwrap();

        let repository: Arc<dyn TrackRepository> = Arc::new(InMemoryTrackRepository::default());

        let first = scan(&repository, std::slice::from_ref(&root))
            .await
            .unwrap();
        assert_eq!(first.indexed, 1);

        let second = scan(&repository, std::slice::from_ref(&root))
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

    #[tokio::test]
    async fn a_vanished_file_is_counted_missing_without_touching_the_track() {
        let root = temp_root("missing");
        let file = root.join("a.flac");
        std::fs::copy(fixture(), &file).unwrap();

        let repository: Arc<dyn TrackRepository> = Arc::new(InMemoryTrackRepository::default());
        let first = scan(&repository, std::slice::from_ref(&root))
            .await
            .unwrap();
        assert_eq!(first.indexed, 1);

        std::fs::remove_file(&file).unwrap();
        let second = scan(&repository, std::slice::from_ref(&root))
            .await
            .unwrap();
        assert_eq!(second.missing, 1);
        assert_eq!(second.indexed, 0);

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
        assert_eq!(tags.genre.as_deref(), Some("Test"));
        assert_eq!(
            tags.recording_id.as_deref(),
            Some("11111111-2222-3333-4444-555555555555")
        );
        assert!(tags.duration_ms.is_some_and(|ms| ms > 0));

        let (rung, id) = identity_for(&tags, "a.flac");
        assert_eq!(rung, IdentityRung::MusicBrainzRecording);
        assert_eq!(id, "11111111-2222-3333-4444-555555555555");
    }

    #[tokio::test]
    async fn an_untagged_file_is_counted_rather_than_indexed() {
        let root = temp_root("untagged");
        std::fs::copy(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/silence.flac"),
            root.join("a.flac"),
        )
        .unwrap();
        let repository: Arc<dyn TrackRepository> = Arc::new(InMemoryTrackRepository::default());

        let report = scan(&repository, std::slice::from_ref(&root))
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

    #[tokio::test]
    async fn a_scanned_file_populates_its_album_and_membership() {
        let root = temp_root("album");
        std::fs::copy(fixture(), root.join("a.flac")).unwrap();
        let repository: Arc<dyn TrackRepository> = Arc::new(InMemoryTrackRepository::default());

        let report = scan(&repository, std::slice::from_ref(&root))
            .await
            .unwrap();
        assert_eq!(report.indexed, 1);

        let tags = read_tags(&fixture()).unwrap();
        let expected = album_metadata(&tags).unwrap().unwrap();
        let album = repository.resolve_or_create_album(&expected).await.unwrap();

        let sources = repository.find_album_sources(album.id()).await.unwrap();
        assert_eq!(sources.len(), 1);
        assert_eq!(sources[0].provider_id().as_str(), "local");

        let tracks = repository.find_album_tracks(album.id()).await.unwrap();
        assert_eq!(tracks.len(), 1);
        assert_eq!(tracks[0].1.disc_number(), Some(1));
        assert_eq!(tracks[0].1.track_number(), Some(3));

        std::fs::remove_dir_all(&root).unwrap();
    }
}
