use super::{SourceIdentity, TrackId};

const LOSSLESS_SUFFIXES: &[&str] = &["flac", "wav", "aiff", "aif", "ape", "wv"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DuplicateSource {
    source: SourceIdentity,
    path: String,
    size_bytes: u64,
    suffix: Option<String>,
}

impl DuplicateSource {
    pub fn new(
        source: SourceIdentity,
        path: String,
        size_bytes: u64,
        suffix: Option<String>,
    ) -> Self {
        Self {
            source,
            path,
            size_bytes,
            suffix,
        }
    }

    pub fn source(&self) -> &SourceIdentity {
        &self.source
    }

    pub fn path(&self) -> &str {
        &self.path
    }

    pub fn size_bytes(&self) -> u64 {
        self.size_bytes
    }

    pub fn suffix(&self) -> Option<&str> {
        self.suffix.as_deref()
    }

    pub fn is_lossless(&self) -> bool {
        self.suffix.as_deref().is_some_and(|suffix| {
            LOSSLESS_SUFFIXES
                .iter()
                .any(|candidate| candidate.eq_ignore_ascii_case(suffix))
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeepReason {
    LosslessOverLossy,
    LargerFile,
    FirstPath,
}

impl KeepReason {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::LosslessOverLossy => "lossless over lossy",
            Self::LargerFile => "larger file",
            Self::FirstPath => "path order",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DuplicateCandidate {
    track_id: TrackId,
    title: String,
    duration_ms: Option<u64>,
    artists: Vec<String>,
    sources: Vec<DuplicateSource>,
}

impl DuplicateCandidate {
    pub fn new(
        track_id: TrackId,
        title: String,
        duration_ms: Option<u64>,
        artists: Vec<String>,
        sources: Vec<DuplicateSource>,
    ) -> Self {
        Self {
            track_id,
            title,
            duration_ms,
            artists,
            sources,
        }
    }

    pub fn track_id(&self) -> &TrackId {
        &self.track_id
    }

    pub fn title(&self) -> &str {
        &self.title
    }

    pub fn duration_ms(&self) -> Option<u64> {
        self.duration_ms
    }

    pub fn artists(&self) -> &[String] {
        &self.artists
    }

    pub fn sources(&self) -> &[DuplicateSource] {
        &self.sources
    }

    pub fn push_source(&mut self, source: DuplicateSource) {
        self.sources.push(source);
    }

    pub fn keep(&self) -> Option<(&DuplicateSource, KeepReason)> {
        let any_lossy = self.sources.iter().any(|source| !source.is_lossless());

        if any_lossy {
            let mut best: Option<&DuplicateSource> = None;
            for source in self.sources.iter().filter(|source| source.is_lossless()) {
                if best.is_none_or(|current| source.size_bytes() > current.size_bytes()) {
                    best = Some(source);
                }
            }

            if let Some(source) = best {
                return Some((source, KeepReason::LosslessOverLossy));
            }
        }

        let largest = self
            .sources
            .iter()
            .max_by_key(|source| source.size_bytes())?;
        let tied = self
            .sources
            .iter()
            .filter(|source| source.size_bytes() == largest.size_bytes())
            .count();

        if tied == 1 {
            return Some((largest, KeepReason::LargerFile));
        }

        self.sources
            .iter()
            .filter(|source| source.size_bytes() == largest.size_bytes())
            .min_by(|left, right| left.path().cmp(right.path()))
            .map(|source| (source, KeepReason::FirstPath))
    }

    pub fn wasted_bytes(&self) -> u64 {
        let Some((keep, _)) = self.keep() else {
            return 0;
        };

        self.sources
            .iter()
            .filter(|source| source.source() != keep.source())
            .map(DuplicateSource::size_bytes)
            .sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::features::music_library::domain::{ProviderId, SourceScope};

    fn source(path: &str, size: u64, suffix: &str) -> DuplicateSource {
        DuplicateSource::new(
            SourceIdentity::new(
                ProviderId::new("local".to_owned()).unwrap(),
                SourceScope::Named("/music".to_owned()),
                path.to_owned(),
            )
            .unwrap(),
            path.to_owned(),
            size,
            Some(suffix.to_owned()),
        )
    }

    fn candidate(sources: Vec<DuplicateSource>) -> DuplicateCandidate {
        DuplicateCandidate::new(
            TrackId::new(),
            "Song".to_owned(),
            Some(1_000),
            vec!["Artist".to_owned()],
            sources,
        )
    }

    #[test]
    fn a_lossless_copy_outranks_a_larger_lossy_one() {
        let candidate = candidate(vec![
            source("a.mp3", 900, "mp3"),
            source("b.flac", 100, "flac"),
        ]);

        let (keep, reason) = candidate.keep().unwrap();
        assert_eq!(keep.path(), "b.flac");
        assert_eq!(reason, KeepReason::LosslessOverLossy);
        assert_eq!(candidate.wasted_bytes(), 900);
    }

    #[test]
    fn the_largest_lossless_copy_is_kept() {
        let candidate = candidate(vec![
            source("a.flac", 100, "flac"),
            source("b.flac", 400, "flac"),
            source("c.mp3", 900, "mp3"),
        ]);

        let (keep, reason) = candidate.keep().unwrap();
        assert_eq!(keep.path(), "b.flac");
        assert_eq!(reason, KeepReason::LosslessOverLossy);
        assert_eq!(candidate.wasted_bytes(), 1_000);
    }

    #[test]
    fn without_lossless_the_largest_file_is_kept() {
        let candidate = candidate(vec![
            source("a.mp3", 100, "mp3"),
            source("b.mp3", 400, "mp3"),
        ]);

        let (keep, reason) = candidate.keep().unwrap();
        assert_eq!(keep.path(), "b.mp3");
        assert_eq!(reason, KeepReason::LargerFile);
        assert_eq!(candidate.wasted_bytes(), 100);
    }

    #[test]
    fn equal_sizes_fall_back_to_path_order() {
        let candidate = candidate(vec![
            source("b.flac", 400, "flac"),
            source("a.flac", 400, "flac"),
        ]);

        let (keep, reason) = candidate.keep().unwrap();
        assert_eq!(keep.path(), "a.flac");
        assert_eq!(reason, KeepReason::FirstPath);
        assert_eq!(candidate.wasted_bytes(), 400);
    }

    #[test]
    fn a_single_source_keeps_itself() {
        let candidate = candidate(vec![source("a.flac", 400, "flac")]);

        let (keep, reason) = candidate.keep().unwrap();
        assert_eq!(keep.path(), "a.flac");
        assert_eq!(reason, KeepReason::LargerFile);
        assert_eq!(candidate.wasted_bytes(), 0);
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SplitArtistRow {
    artist_id: super::ArtistId,
    name: String,
    identified: bool,
    track_count: u64,
    album_count: u64,
}

impl SplitArtistRow {
    pub fn new(
        artist_id: super::ArtistId,
        name: String,
        identified: bool,
        track_count: u64,
        album_count: u64,
    ) -> Self {
        Self {
            artist_id,
            name,
            identified,
            track_count,
            album_count,
        }
    }

    pub fn artist_id(&self) -> &super::ArtistId {
        &self.artist_id
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn is_identified(&self) -> bool {
        self.identified
    }

    pub fn track_count(&self) -> u64 {
        self.track_count
    }

    pub fn album_count(&self) -> u64 {
        self.album_count
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SplitArtist {
    name: String,
    rows: Vec<SplitArtistRow>,
}

impl SplitArtist {
    pub fn new(name: String, rows: Vec<SplitArtistRow>) -> Self {
        Self { name, rows }
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn rows(&self) -> &[SplitArtistRow] {
        &self.rows
    }

    pub fn push_row(&mut self, row: SplitArtistRow) {
        self.rows.push(row);
    }

    pub fn has_identifier(&self) -> bool {
        self.rows.iter().any(SplitArtistRow::is_identified)
    }
}
