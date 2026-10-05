use std::{
    fmt::{self, Display},
    str::FromStr,
};

use uuid::Uuid;

use super::{
    IdSet, LibraryState, MatchQuality, Range, RoleSet, SelectorText, SortDirection, SourceIdentity,
    ValidationError,
    text::{optional, required},
};
use crate::features::music_library::library::LibraryId;

const MAX_NAME: usize = 500;
const MAX_IDENTIFIER: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ArtistId(Uuid);

impl ArtistId {
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

impl Default for ArtistId {
    fn default() -> Self {
        Self::new()
    }
}

impl Display for ArtistId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}", self.0)
    }
}

impl FromStr for ArtistId {
    type Err = uuid::Error;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Ok(Self(Uuid::parse_str(value)?))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Artist {
    id: ArtistId,
    name: String,
    sort_name: Option<String>,
    musicbrainz_artist_id: Option<String>,
}

impl Artist {
    pub fn new(
        id: ArtistId,
        name: String,
        sort_name: Option<String>,
        musicbrainz_artist_id: Option<String>,
    ) -> Result<Self, ValidationError> {
        Ok(Self {
            id,
            name: required("name", name, MAX_NAME)?,
            sort_name: optional("sort_name", sort_name, MAX_NAME)?,
            musicbrainz_artist_id: optional(
                "musicbrainz_artist_id",
                musicbrainz_artist_id,
                MAX_IDENTIFIER,
            )?,
        })
    }

    pub fn id(&self) -> &ArtistId {
        &self.id
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn sort_name(&self) -> Option<&str> {
        self.sort_name.as_deref()
    }

    pub fn musicbrainz_artist_id(&self) -> Option<&str> {
        self.musicbrainz_artist_id.as_deref()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtistSummary {
    artist: Artist,
    album_count: u32,
    song_count: Option<u32>,
    state: LibraryState,
    match_quality: Option<MatchQuality>,
}

impl ArtistSummary {
    pub fn new(artist: Artist, album_count: u32) -> Self {
        Self {
            artist,
            album_count,
            song_count: None,
            state: LibraryState::default(),
            match_quality: None,
        }
    }

    pub fn with_song_count(mut self, song_count: u32) -> Self {
        self.song_count = Some(song_count);
        self
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

    pub fn artist(&self) -> &Artist {
        &self.artist
    }

    pub fn album_count(&self) -> u32 {
        self.album_count
    }

    pub fn song_count(&self) -> Option<u32> {
        self.song_count
    }

    pub fn state(&self) -> LibraryState {
        self.state
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ArtistSort {
    #[default]
    Name,
    AlbumCount,
    SongCount,
    StarredAt,
    Rating,
    PlayCount,
}

impl ArtistSort {
    pub const ALL: [ArtistSort; 6] = [
        ArtistSort::Name,
        ArtistSort::AlbumCount,
        ArtistSort::SongCount,
        ArtistSort::StarredAt,
        ArtistSort::Rating,
        ArtistSort::PlayCount,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Name => "name",
            Self::AlbumCount => "album_count",
            Self::SongCount => "song_count",
            Self::StarredAt => "starred_at",
            Self::Rating => "rating",
            Self::PlayCount => "play_count",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|sort| sort.as_str() == value)
    }

    pub fn default_direction(self) -> SortDirection {
        match self {
            Self::Name => SortDirection::Ascending,
            Self::AlbumCount
            | Self::SongCount
            | Self::StarredAt
            | Self::Rating
            | Self::PlayCount => SortDirection::Descending,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ArtistListFilter {
    name: Option<SelectorText>,
    role: Option<RoleSet>,
    starred: Option<bool>,
    has_rating: Option<bool>,
    rating: Option<Range<u8>>,
    library_id: Option<IdSet<LibraryId>>,
    missing: Option<bool>,
    sort_direction: Option<SortDirection>,
    provider_identity: bool,
}

impl ArtistListFilter {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_name(mut self, name: Option<SelectorText>) -> Self {
        self.name = name;
        self
    }

    pub fn with_role(mut self, role: Option<RoleSet>) -> Self {
        self.role = role;
        self
    }

    pub fn with_starred(mut self, starred: Option<bool>) -> Self {
        self.starred = starred;
        self
    }

    pub fn with_has_rating(mut self, has_rating: Option<bool>) -> Self {
        self.has_rating = has_rating;
        self
    }

    pub fn with_rating(mut self, rating: Option<Range<u8>>) -> Self {
        self.rating = rating;
        self
    }

    pub fn with_library_id(mut self, library_id: Option<IdSet<LibraryId>>) -> Self {
        self.library_id = library_id;
        self
    }

    pub fn with_missing(mut self, missing: Option<bool>) -> Self {
        self.missing = missing;
        self
    }

    pub fn with_sort_direction(mut self, sort_direction: Option<SortDirection>) -> Self {
        self.sort_direction = sort_direction;
        self
    }

    pub fn with_provider_identity(mut self, provider_identity: bool) -> Self {
        self.provider_identity = provider_identity;
        self
    }

    pub fn name(&self) -> Option<&SelectorText> {
        self.name.as_ref()
    }

    pub fn role(&self) -> Option<&RoleSet> {
        self.role.as_ref()
    }

    pub fn starred(&self) -> Option<bool> {
        self.starred
    }

    pub fn has_rating(&self) -> Option<bool> {
        self.has_rating
    }

    pub fn rating(&self) -> Option<Range<u8>> {
        self.rating
    }

    pub fn library_id(&self) -> Option<&IdSet<LibraryId>> {
        self.library_id.as_ref()
    }

    pub fn missing(&self) -> Option<bool> {
        self.missing
    }

    pub fn sort_direction(&self) -> Option<SortDirection> {
        self.sort_direction
    }

    pub fn provider_identity(&self) -> bool {
        self.provider_identity
    }

    pub fn resolved_direction(&self, sort: ArtistSort) -> SortDirection {
        self.sort_direction
            .unwrap_or_else(|| sort.default_direction())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CreditRole {
    Arranger,
    Composer,
    Conductor,
    Djmixer,
    Director,
    Engineer,
    Lyricist,
    Mixer,
    Performer,
    Producer,
    Remixer,
}

impl CreditRole {
    pub const ALL: [CreditRole; 11] = [
        CreditRole::Arranger,
        CreditRole::Composer,
        CreditRole::Conductor,
        CreditRole::Djmixer,
        CreditRole::Director,
        CreditRole::Engineer,
        CreditRole::Lyricist,
        CreditRole::Mixer,
        CreditRole::Performer,
        CreditRole::Producer,
        CreditRole::Remixer,
    ];

    pub fn as_str(&self) -> &'static str {
        match self {
            CreditRole::Arranger => "arranger",
            CreditRole::Composer => "composer",
            CreditRole::Conductor => "conductor",
            CreditRole::Djmixer => "djmixer",
            CreditRole::Director => "director",
            CreditRole::Engineer => "engineer",
            CreditRole::Lyricist => "lyricist",
            CreditRole::Mixer => "mixer",
            CreditRole::Performer => "performer",
            CreditRole::Producer => "producer",
            CreditRole::Remixer => "remixer",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        let value = value.trim();

        Self::ALL
            .into_iter()
            .find(|role| role.as_str().eq_ignore_ascii_case(value))
    }
}

/// a name as a track or album credits it, with the identifier when the source
/// supplied one.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ArtistCredit {
    name: String,
    musicbrainz_artist_id: Option<String>,
    provider: Option<SourceIdentity>,
    role: Option<CreditRole>,
}

impl ArtistCredit {
    pub fn new(
        name: String,
        musicbrainz_artist_id: Option<String>,
    ) -> Result<Self, ValidationError> {
        Ok(Self {
            name: required("artist", name, MAX_NAME)?,
            musicbrainz_artist_id: optional(
                "musicbrainz_artist_id",
                musicbrainz_artist_id,
                MAX_IDENTIFIER,
            )?,
            provider: None,
            role: None,
        })
    }

    pub fn with_provider(mut self, provider: Option<SourceIdentity>) -> Self {
        self.provider = provider;
        self
    }

    pub fn with_role(mut self, role: CreditRole) -> Self {
        self.role = Some(role);
        self
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn musicbrainz_artist_id(&self) -> Option<&str> {
        self.musicbrainz_artist_id.as_deref()
    }

    pub fn provider(&self) -> Option<&SourceIdentity> {
        self.provider.as_ref()
    }

    pub fn role(&self) -> Option<CreditRole> {
        self.role
    }
}

impl From<&Artist> for ArtistCredit {
    fn from(artist: &Artist) -> Self {
        Self {
            name: artist.name.clone(),
            musicbrainz_artist_id: artist.musicbrainz_artist_id.clone(),
            provider: None,
            role: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn artist_identity_round_trips() {
        let id = ArtistId::new();
        assert_eq!(id.to_string().parse::<ArtistId>().unwrap(), id);
    }

    #[test]
    fn artist_trims_and_rejects_invalid_names() {
        let artist = Artist::new(ArtistId::new(), "  Daft Punk ".to_owned(), None, None).unwrap();
        assert_eq!(artist.name(), "Daft Punk");
        assert_eq!(artist.musicbrainz_artist_id(), None);

        assert!(matches!(
            Artist::new(ArtistId::new(), "  ".to_owned(), None, None),
            Err(ValidationError::Empty { field: "name" })
        ));
    }

    #[test]
    fn credit_keeps_a_blank_identifier_as_absent() {
        let credit = ArtistCredit::new("Daft Punk".to_owned(), Some("  ".to_owned())).unwrap();
        assert_eq!(credit.musicbrainz_artist_id(), None);
    }

    #[test]
    fn a_primary_credit_carries_no_role() {
        let credit = ArtistCredit::new("Daft Punk".to_owned(), None).unwrap();
        assert_eq!(credit.role(), None);

        let composed = credit.with_role(CreditRole::Composer);
        assert_eq!(composed.role(), Some(CreditRole::Composer));
    }

    #[test]
    fn credit_roles_round_trip_through_the_stored_form() {
        for role in CreditRole::ALL {
            assert_eq!(CreditRole::parse(role.as_str()), Some(role));

            let credit = ArtistCredit::new("A".to_owned(), None)
                .unwrap()
                .with_role(role);
            assert_eq!(credit.role().unwrap().as_str(), role.as_str());
        }

        assert_eq!(CreditRole::parse(" Composer "), Some(CreditRole::Composer));
        assert_eq!(CreditRole::parse("djmixer"), Some(CreditRole::Djmixer));
        assert_eq!(CreditRole::parse("other"), None);
        assert_eq!(CreditRole::parse(""), None);
    }

    #[test]
    fn artist_sorts_round_trip_through_their_native_names() {
        for sort in ArtistSort::ALL {
            assert_eq!(ArtistSort::parse(sort.as_str()), Some(sort));
        }

        assert_eq!(ArtistSort::parse("albumCount"), None);
        assert_eq!(ArtistSort::parse("songCount"), None);
        assert_eq!(ArtistSort::parse("playCount"), None);
        assert_eq!(ArtistSort::parse("Name"), None);
        assert_eq!(ArtistSort::parse("random"), None);
        assert_eq!(ArtistSort::parse("updatedAt"), None);
        assert_eq!(ArtistSort::parse(""), None);
    }

    #[test]
    fn artist_sorts_carry_their_natural_direction() {
        assert_eq!(
            ArtistSort::Name.default_direction(),
            SortDirection::Ascending
        );

        for sort in [
            ArtistSort::AlbumCount,
            ArtistSort::SongCount,
            ArtistSort::StarredAt,
            ArtistSort::Rating,
            ArtistSort::PlayCount,
        ] {
            assert_eq!(
                sort.default_direction(),
                SortDirection::Descending,
                "{sort:?}"
            );
        }
    }

    #[test]
    fn artist_filters_default_to_an_absent_filter() {
        let filter = ArtistListFilter::new();

        assert!(filter.name().is_none());
        assert!(filter.role().is_none());
        assert!(filter.starred().is_none());
        assert!(filter.has_rating().is_none());
        assert!(filter.rating().is_none());
        assert!(filter.library_id().is_none());
        assert!(filter.missing().is_none());
        assert!(filter.sort_direction().is_none());
        assert_eq!(
            filter.resolved_direction(ArtistSort::Name),
            SortDirection::Ascending
        );
        assert_eq!(
            filter.resolved_direction(ArtistSort::PlayCount),
            SortDirection::Descending
        );
        assert_eq!(
            filter
                .with_sort_direction(Some(SortDirection::Ascending))
                .resolved_direction(ArtistSort::PlayCount),
            SortDirection::Ascending
        );
    }
}
