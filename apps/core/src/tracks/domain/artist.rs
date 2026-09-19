use std::{
    fmt::{self, Display},
    str::FromStr,
};

use uuid::Uuid;

use super::{
    LibraryState, ValidationError,
    text::{optional, required},
};

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
    state: LibraryState,
}

impl ArtistSummary {
    pub fn new(artist: Artist, album_count: u32) -> Self {
        Self {
            artist,
            album_count,
            state: LibraryState::default(),
        }
    }

    pub fn with_state(mut self, state: LibraryState) -> Self {
        self.state = state;
        self
    }

    pub fn artist(&self) -> &Artist {
        &self.artist
    }

    pub fn album_count(&self) -> u32 {
        self.album_count
    }

    pub fn state(&self) -> LibraryState {
        self.state
    }
}

/// a name as a track or album credits it, with the identifier when the source
/// supplied one.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ArtistCredit {
    name: String,
    musicbrainz_artist_id: Option<String>,
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
        })
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn musicbrainz_artist_id(&self) -> Option<&str> {
        self.musicbrainz_artist_id.as_deref()
    }
}

impl From<&Artist> for ArtistCredit {
    fn from(artist: &Artist) -> Self {
        Self {
            name: artist.name.clone(),
            musicbrainz_artist_id: artist.musicbrainz_artist_id.clone(),
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
}
