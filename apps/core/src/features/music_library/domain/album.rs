use std::{
    fmt::{self, Display},
    str::FromStr,
};

use uuid::Uuid;

use super::{
    Artist, ArtistCredit, ArtistId, LibraryState, MatchQuality, SourceIdentity, Track,
    ValidationError,
    text::{optional, required},
};

const MAX_TITLE: usize = 500;
const MAX_TEXT: usize = 500;
const MAX_IDENTIFIER: usize = 64;
const MIN_YEAR: i64 = 1000;
const MAX_YEAR: i64 = 3000;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct AlbumId(Uuid);

impl AlbumId {
    pub fn new() -> Self {
        Self(Uuid::now_v7())
    }

    pub fn from_uuid(value: Uuid) -> Self {
        Self(value)
    }

    pub fn as_uuid(&self) -> &Uuid {
        &self.0
    }
}

impl Default for AlbumId {
    fn default() -> Self {
        Self::new()
    }
}

impl Display for AlbumId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}", self.0)
    }
}

impl FromStr for AlbumId {
    type Err = uuid::Error;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Ok(Self(Uuid::parse_str(value)?))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Album {
    id: AlbumId,
    title: String,
    album_artist_id: Option<ArtistId>,
    year: Option<i32>,
    release_type: Option<String>,
    label: Option<String>,
    musicbrainz_release_group_id: Option<String>,
    musicbrainz_release_id: Option<String>,
}

impl Album {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        id: AlbumId,
        title: String,
        album_artist_id: Option<ArtistId>,
        year: Option<i32>,
        release_type: Option<String>,
        label: Option<String>,
        musicbrainz_release_group_id: Option<String>,
        musicbrainz_release_id: Option<String>,
    ) -> Result<Self, ValidationError> {
        Ok(Self {
            id,
            title: required("title", title, MAX_TITLE)?,
            album_artist_id,
            year: checked_year(year)?,
            release_type: optional("release_type", release_type, MAX_IDENTIFIER)?,
            label: optional("label", label, MAX_TEXT)?,
            musicbrainz_release_group_id: optional(
                "musicbrainz_release_group_id",
                musicbrainz_release_group_id,
                MAX_IDENTIFIER,
            )?,
            musicbrainz_release_id: optional(
                "musicbrainz_release_id",
                musicbrainz_release_id,
                MAX_IDENTIFIER,
            )?,
        })
    }

    pub fn id(&self) -> &AlbumId {
        &self.id
    }

    pub fn title(&self) -> &str {
        &self.title
    }

    pub fn album_artist_id(&self) -> Option<&ArtistId> {
        self.album_artist_id.as_ref()
    }

    pub fn year(&self) -> Option<i32> {
        self.year
    }

    pub fn release_type(&self) -> Option<&str> {
        self.release_type.as_deref()
    }

    pub fn label(&self) -> Option<&str> {
        self.label.as_deref()
    }

    pub fn musicbrainz_release_group_id(&self) -> Option<&str> {
        self.musicbrainz_release_group_id.as_deref()
    }

    pub fn musicbrainz_release_id(&self) -> Option<&str> {
        self.musicbrainz_release_id.as_deref()
    }
}

/// what a writer knows about an album before the repository decides whether it
/// already exists.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct AlbumMetadata {
    title: String,
    album_artist: Option<ArtistCredit>,
    year: Option<i32>,
    release_type: Option<String>,
    label: Option<String>,
    musicbrainz_release_group_id: Option<String>,
    musicbrainz_release_id: Option<String>,
    provider: Option<SourceIdentity>,
}

impl AlbumMetadata {
    pub fn new(title: String) -> Result<Self, ValidationError> {
        Ok(Self {
            title: required("title", title, MAX_TITLE)?,
            album_artist: None,
            year: None,
            release_type: None,
            label: None,
            musicbrainz_release_group_id: None,
            musicbrainz_release_id: None,
            provider: None,
        })
    }

    pub fn with_provider(mut self, provider: Option<SourceIdentity>) -> Self {
        self.provider = provider;
        self
    }

    pub fn provider(&self) -> Option<&SourceIdentity> {
        self.provider.as_ref()
    }

    pub fn with_album_artist(mut self, album_artist: ArtistCredit) -> Self {
        self.album_artist = Some(album_artist);
        self
    }

    pub fn with_year(mut self, year: Option<i32>) -> Result<Self, ValidationError> {
        self.year = checked_year(year)?;
        Ok(self)
    }

    pub fn with_release_type(
        mut self,
        release_type: Option<String>,
    ) -> Result<Self, ValidationError> {
        self.release_type = optional("release_type", release_type, MAX_IDENTIFIER)?;
        Ok(self)
    }

    pub fn with_label(mut self, label: Option<String>) -> Result<Self, ValidationError> {
        self.label = optional("label", label, MAX_TEXT)?;
        Ok(self)
    }

    pub fn with_release_group_id(
        mut self,
        release_group_id: Option<String>,
    ) -> Result<Self, ValidationError> {
        self.musicbrainz_release_group_id = optional(
            "musicbrainz_release_group_id",
            release_group_id,
            MAX_IDENTIFIER,
        )?;
        Ok(self)
    }

    pub fn with_release_id(mut self, release_id: Option<String>) -> Result<Self, ValidationError> {
        self.musicbrainz_release_id =
            optional("musicbrainz_release_id", release_id, MAX_IDENTIFIER)?;
        Ok(self)
    }

    pub fn title(&self) -> &str {
        &self.title
    }

    pub fn album_artist(&self) -> Option<&ArtistCredit> {
        self.album_artist.as_ref()
    }

    pub fn year(&self) -> Option<i32> {
        self.year
    }

    pub fn release_type(&self) -> Option<&str> {
        self.release_type.as_deref()
    }

    pub fn label(&self) -> Option<&str> {
        self.label.as_deref()
    }

    pub fn musicbrainz_release_group_id(&self) -> Option<&str> {
        self.musicbrainz_release_group_id.as_deref()
    }

    pub fn musicbrainz_release_id(&self) -> Option<&str> {
        self.musicbrainz_release_id.as_deref()
    }
}

/// a track's place on an album.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AlbumTrack {
    disc_number: Option<u16>,
    track_number: Option<u16>,
    position: u32,
}

impl AlbumTrack {
    pub fn new(
        disc_number: Option<u16>,
        track_number: Option<u16>,
        position: u32,
    ) -> Result<Self, ValidationError> {
        for (field, value) in [("disc_number", disc_number), ("track_number", track_number)] {
            if value == Some(0) {
                return Err(ValidationError::OutOfRange {
                    field,
                    min: 1,
                    max: i64::from(u16::MAX),
                });
            }
        }

        Ok(Self {
            disc_number,
            track_number,
            position,
        })
    }

    pub fn unplaced() -> Self {
        Self {
            disc_number: None,
            track_number: None,
            position: 0,
        }
    }

    pub fn disc_number(&self) -> Option<u16> {
        self.disc_number
    }

    pub fn track_number(&self) -> Option<u16> {
        self.track_number
    }

    pub fn position(&self) -> u32 {
        self.position
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AlbumSummary {
    album: Album,
    album_artist: Option<Artist>,
    track_count: u32,
    duration_ms: u64,
    created_at: chrono::DateTime<chrono::Utc>,
    state: LibraryState,
    match_quality: Option<MatchQuality>,
}

impl AlbumSummary {
    pub fn new(
        album: Album,
        album_artist: Option<Artist>,
        track_count: u32,
        duration_ms: u64,
        created_at: chrono::DateTime<chrono::Utc>,
    ) -> Self {
        Self {
            album,
            album_artist,
            track_count,
            duration_ms,
            created_at,
            state: LibraryState::default(),
            match_quality: None,
        }
    }

    pub fn with_state(mut self, state: LibraryState) -> Self {
        self.state = state;
        self
    }

    pub fn with_match_quality(mut self, match_quality: Option<MatchQuality>) -> Self {
        self.match_quality = match_quality;
        self
    }

    pub fn match_quality(&self) -> Option<MatchQuality> {
        self.match_quality
    }

    pub fn state(&self) -> LibraryState {
        self.state
    }

    pub fn album(&self) -> &Album {
        &self.album
    }

    pub fn album_artist(&self) -> Option<&Artist> {
        self.album_artist.as_ref()
    }

    pub fn track_count(&self) -> u32 {
        self.track_count
    }

    pub fn duration_ms(&self) -> u64 {
        self.duration_ms
    }

    pub fn created_at(&self) -> chrono::DateTime<chrono::Utc> {
        self.created_at
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AlbumOrder {
    #[default]
    Title,
    Artist,
    RecentlyAdded,
    Random,
    Frequent,
    Recent,
    HighestRated,
    Starred,
    YearAscending,
    YearDescending,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AlbumListFilter {
    starred: bool,
    genre: Option<String>,
    year_from: Option<i32>,
    year_to: Option<i32>,
}

impl AlbumListFilter {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn starred(mut self) -> Self {
        self.starred = true;
        self
    }

    pub fn with_genre(mut self, genre: Option<String>) -> Self {
        self.genre = genre
            .map(|genre| genre.trim().to_owned())
            .filter(|genre| !genre.is_empty());
        self
    }

    pub fn with_year(mut self, from: Option<i32>, to: Option<i32>) -> Self {
        self.year_from = from;
        self.year_to = to;
        self
    }

    pub fn is_starred(&self) -> bool {
        self.starred
    }

    pub fn genre(&self) -> Option<&str> {
        self.genre.as_deref()
    }

    pub fn year_from(&self) -> Option<i32> {
        self.year_from
    }

    pub fn year_to(&self) -> Option<i32> {
        self.year_to
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AlbumEntry {
    track: Track,
    membership: AlbumTrack,
    state: LibraryState,
    sources: Vec<SourceIdentity>,
}

impl AlbumEntry {
    pub fn new(track: Track, membership: AlbumTrack) -> Self {
        Self {
            track,
            membership,
            state: LibraryState::default(),
            sources: Vec::new(),
        }
    }

    pub fn with_state(mut self, state: LibraryState) -> Self {
        self.state = state;
        self
    }

    pub fn with_sources(mut self, sources: Vec<SourceIdentity>) -> Self {
        self.sources = sources;
        self
    }

    pub fn state(&self) -> LibraryState {
        self.state
    }

    pub fn sources(&self) -> &[SourceIdentity] {
        &self.sources
    }

    pub fn track(&self) -> &Track {
        &self.track
    }

    pub fn into_track(self) -> Track {
        self.track
    }

    pub fn membership(&self) -> &AlbumTrack {
        &self.membership
    }
}

fn checked_year(year: Option<i32>) -> Result<Option<i32>, ValidationError> {
    if let Some(year) = year
        && !(MIN_YEAR..=MAX_YEAR).contains(&i64::from(year))
    {
        return Err(ValidationError::OutOfRange {
            field: "year",
            min: MIN_YEAR,
            max: MAX_YEAR,
        });
    }

    Ok(year)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn album_identity_round_trips() {
        let id = AlbumId::new();
        assert_eq!(id.to_string().parse::<AlbumId>().unwrap(), id);
    }

    #[test]
    fn album_metadata_trims_and_bounds_optional_fields() {
        let metadata = AlbumMetadata::new("  Discovery ".to_owned())
            .unwrap()
            .with_album_artist(ArtistCredit::new("Daft Punk".to_owned(), None).unwrap())
            .with_year(Some(2001))
            .unwrap()
            .with_release_type(Some(" album ".to_owned()))
            .unwrap()
            .with_label(Some("  ".to_owned()))
            .unwrap()
            .with_release_group_id(Some("rg-1".to_owned()))
            .unwrap();

        assert_eq!(metadata.title(), "Discovery");
        assert_eq!(metadata.album_artist().unwrap().name(), "Daft Punk");
        assert_eq!(metadata.year(), Some(2001));
        assert_eq!(metadata.release_type(), Some("album"));
        assert_eq!(metadata.label(), None);
        assert_eq!(metadata.musicbrainz_release_group_id(), Some("rg-1"));
    }

    #[test]
    fn album_rejects_empty_titles_and_out_of_range_years() {
        assert!(matches!(
            AlbumMetadata::new("  ".to_owned()),
            Err(ValidationError::Empty { field: "title" })
        ));
        assert!(matches!(
            AlbumMetadata::new("Album".to_owned())
                .unwrap()
                .with_year(Some(999)),
            Err(ValidationError::OutOfRange { field: "year", .. })
        ));
    }

    #[test]
    fn album_track_rejects_zero_numbering() {
        assert!(matches!(
            AlbumTrack::new(Some(0), Some(1), 0),
            Err(ValidationError::OutOfRange {
                field: "disc_number",
                ..
            })
        ));
        assert!(matches!(
            AlbumTrack::new(None, Some(0), 0),
            Err(ValidationError::OutOfRange {
                field: "track_number",
                ..
            })
        ));
        assert_eq!(AlbumTrack::new(None, Some(3), 2).unwrap().position(), 2);
    }
}
