use std::{
    fmt::{self, Display},
    matches,
    str::FromStr,
};

use thiserror::Error;
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ValidationError {
    #[error("{field} must not be empty")]
    Empty { field: &'static str },

    #[error("{field} must not be too long")]
    TooLong { field: &'static str },

    #[error("provider id has an invalid format")]
    InvalidProviderId,

    #[error("duration_ms must not be negative")]
    InvalidDuration,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ProviderId(String);

impl ProviderId {
    pub fn new(value: String) -> Result<Self, ValidationError> {
        if value.is_empty() {
            return Err(ValidationError::Empty {
                field: "provider_id",
            });
        }

        if value.chars().count() > 64 {
            return Err(ValidationError::TooLong {
                field: "provider_id",
            });
        }

        let mut chars = value.chars();

        if !chars
            .next()
            .is_some_and(|character| character.is_ascii_lowercase())
        {
            return Err(ValidationError::InvalidProviderId);
        }

        let mut was_previous_char_separator = false;

        for character in chars {
            if character.is_ascii_lowercase() || character.is_ascii_digit() {
                was_previous_char_separator = false;
            } else if matches!(character, '.' | '_' | '-') {
                if was_previous_char_separator {
                    return Err(ValidationError::InvalidProviderId);
                }
                was_previous_char_separator = true;
            } else {
                return Err(ValidationError::InvalidProviderId);
            }
        }

        if was_previous_char_separator {
            return Err(ValidationError::InvalidProviderId);
        }

        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl FromStr for ProviderId {
    type Err = ValidationError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::new(s.to_owned())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SourceIdentity {
    provider_id: ProviderId,
    external_id: String,
}

impl SourceIdentity {
    pub fn new(provider_id: ProviderId, external_id: String) -> Result<Self, ValidationError> {
        if external_id.is_empty() {
            return Err(ValidationError::Empty {
                field: "external_id",
            });
        }

        if external_id.chars().count() > 512 {
            return Err(ValidationError::TooLong {
                field: "external_id",
            });
        }

        Ok(Self {
            provider_id,
            external_id,
        })
    }

    pub fn provider_id(&self) -> &ProviderId {
        &self.provider_id
    }

    pub fn external_id(&self) -> &str {
        &self.external_id
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TrackId(Uuid);

impl TrackId {
    pub fn new() -> Self {
        Self(Uuid::now_v7())
    }

    pub fn from_uuid(value: Uuid) -> Self {
        Self(value)
    }
}

impl Default for TrackId {
    fn default() -> Self {
        Self::new()
    }
}

impl FromStr for TrackId {
    type Err = uuid::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(Self(Uuid::parse_str(s)?))
    }
}

impl Display for TrackId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}", self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CatalogueCandidate {
    source: SourceIdentity,
    title: String,
    artists: Vec<String>,
    duration_ms: Option<u64>,
}

impl CatalogueCandidate {
    pub fn new(
        source: SourceIdentity,
        title: String,
        artists: Vec<String>,
        duration_ms: Option<u64>,
    ) -> Result<Self, ValidationError> {
        let title = Self::validate_title(title)?;
        let artists = Self::validate_artists(artists)?;

        Ok(Self {
            source,
            title,
            artists,
            duration_ms,
        })
    }

    fn validate_title(title: String) -> Result<String, ValidationError> {
        if title.is_empty() {
            return Err(ValidationError::Empty { field: "title" });
        }

        if title.chars().count() > 500 {
            return Err(ValidationError::TooLong { field: "title" });
        }

        Ok(title)
    }

    fn validate_artists(artists: Vec<String>) -> Result<Vec<String>, ValidationError> {
        if artists.is_empty() {
            return Err(ValidationError::Empty { field: "artists" });
        }

        for artist in &artists {
            if artist.is_empty() {
                return Err(ValidationError::Empty { field: "artist" });
            }

            if artist.chars().count() > 500 {
                return Err(ValidationError::TooLong { field: "artist" });
            }
        }

        Ok(artists)
    }

    pub fn title(&self) -> &str {
        &self.title
    }

    pub fn artists(&self) -> &[String] {
        &self.artists
    }

    pub fn source(&self) -> &SourceIdentity {
        &self.source
    }

    pub fn duration_ms(&self) -> Option<u64> {
        self.duration_ms
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Track {
    id: TrackId,
    title: String,
    artists: Vec<String>,
    duration_ms: Option<u64>,
}

impl Track {
    pub fn new(
        id: TrackId,
        title: String,
        artists: Vec<String>,
        duration_ms: Option<u64>,
    ) -> Result<Self, ValidationError> {
        let title = Self::validate_title(title)?;
        let artists = Self::validate_artists(artists)?;

        Ok(Self {
            id,
            title,
            artists,
            duration_ms,
        })
    }

    fn validate_title(title: String) -> Result<String, ValidationError> {
        if title.is_empty() {
            return Err(ValidationError::Empty { field: "title" });
        }

        if title.chars().count() > 500 {
            return Err(ValidationError::TooLong { field: "title" });
        }

        Ok(title)
    }

    fn validate_artists(artists: Vec<String>) -> Result<Vec<String>, ValidationError> {
        if artists.is_empty() {
            return Err(ValidationError::Empty { field: "artists" });
        }

        for artist in &artists {
            if artist.is_empty() {
                return Err(ValidationError::Empty { field: "artist" });
            }

            if artist.chars().count() > 500 {
                return Err(ValidationError::TooLong { field: "artist" });
            }
        }

        Ok(artists)
    }

    pub fn title(&self) -> &str {
        &self.title
    }

    pub fn artists(&self) -> &[String] {
        &self.artists
    }

    pub fn id(&self) -> &TrackId {
        &self.id
    }

    pub fn duration_ms(&self) -> Option<u64> {
        self.duration_ms
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source() -> SourceIdentity {
        SourceIdentity::new(
            ProviderId::new("youtube_music".to_owned()).unwrap(),
            "abc123".to_owned(),
        )
        .unwrap()
    }

    #[test]
    fn track_id_round_trips_through_display_and_parsing() {
        let original = TrackId::new();
        let text = original.to_string();
        let parsed = text.parse::<TrackId>().unwrap();

        assert_eq!(parsed, original);
    }

    #[test]
    fn provider_id_accepts_contract_identifiers() {
        for value in ["youtube_music", "catalogue.search", "provider-2"] {
            assert!(ProviderId::new(value.to_owned()).is_ok(), "{value}");
        }
    }

    #[test]
    fn provider_id_rejects_invalid_identifiers() {
        for value in ["", "YouTube", "_leading", "trailing-", "two..dots", "a/b"] {
            assert!(ProviderId::new(value.to_owned()).is_err(), "{value}");
        }

        let too_long = "a".repeat(65);
        assert!(matches!(
            ProviderId::new(too_long),
            Err(ValidationError::TooLong {
                field: "provider_id"
            })
        ));
    }

    #[test]
    fn source_identity_validates_external_id() {
        assert!(matches!(
            SourceIdentity::new(
                ProviderId::new("youtube_music".to_owned()).unwrap(),
                String::new(),
            ),
            Err(ValidationError::Empty {
                field: "external_id"
            })
        ));

        let too_long = "x".repeat(513);
        assert!(matches!(
            SourceIdentity::new(
                ProviderId::new("youtube_music".to_owned()).unwrap(),
                too_long,
            ),
            Err(ValidationError::TooLong {
                field: "external_id"
            })
        ));
    }

    #[test]
    fn catalogue_candidate_requires_title_and_artist() {
        assert!(matches!(
            CatalogueCandidate::new(source(), String::new(), vec!["Artist".to_owned()], None),
            Err(ValidationError::Empty { field: "title" })
        ));

        assert!(matches!(
            CatalogueCandidate::new(source(), "Title".to_owned(), vec![], None),
            Err(ValidationError::Empty { field: "artists" })
        ));
    }

    #[test]
    fn catalogue_candidate_keeps_normalized_metadata() {
        let candidate = CatalogueCandidate::new(
            source(),
            "Instant Crush".to_owned(),
            vec!["Daft Punk".to_owned()],
            Some(337_000),
        )
        .unwrap();

        assert_eq!(candidate.source().external_id(), "abc123");
        assert_eq!(candidate.title(), "Instant Crush");
        assert_eq!(candidate.artists(), ["Daft Punk"]);
        assert_eq!(candidate.duration_ms(), Some(337_000));
    }

    #[test]
    fn catalogue_candidate_rejects_invalid_metadata_lengths() {
        let too_long_title = "a".repeat(501);
        assert!(matches!(
            CatalogueCandidate::new(source(), too_long_title, vec!["Artist".to_owned()], None,),
            Err(ValidationError::TooLong { field: "title" })
        ));

        let too_long_artist = "a".repeat(501);
        assert!(matches!(
            CatalogueCandidate::new(source(), "Title".to_owned(), vec![too_long_artist], None,),
            Err(ValidationError::TooLong { field: "artist" })
        ));

        assert!(matches!(
            CatalogueCandidate::new(source(), "Title".to_owned(), vec![String::new()], None,),
            Err(ValidationError::Empty { field: "artist" })
        ));
    }

    #[test]
    fn track_rejects_empty_artist_names() {
        assert!(matches!(
            Track::new(
                TrackId::new(),
                "Title".to_owned(),
                vec![String::new()],
                None,
            ),
            Err(ValidationError::Empty { field: "artist" })
        ));
    }

    #[test]
    fn track_has_whio_owned_identity_and_metadata() {
        let id = TrackId::new();
        let track = Track::new(
            id.clone(),
            "Instant Crush".to_owned(),
            vec!["Daft Punk".to_owned()],
            Some(337_000),
        )
        .unwrap();

        assert_eq!(track.id(), &id);
        assert_eq!(track.title(), "Instant Crush");
        assert_eq!(track.artists(), ["Daft Punk"]);
        assert_eq!(track.duration_ms(), Some(337_000));
    }
}
