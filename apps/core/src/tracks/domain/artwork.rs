use super::{AlbumId, ArtistId, TrackId, ValidationError};

/// where one source keeps its cover art.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ArtworkRef {
    /// an upstream cover-art id the owning adapter resolves at request time.
    Upstream(String),
    /// a public image URL the core fetches and proxies to clients.
    Url(String),
}

impl ArtworkRef {
    pub fn upstream(value: String) -> Result<Self, ValidationError> {
        validate(&value, 512, "artwork_id")?;
        Ok(Self::Upstream(value))
    }

    pub fn url(value: String) -> Result<Self, ValidationError> {
        validate(&value, 2048, "artwork_url")?;
        Ok(Self::Url(value))
    }

    pub fn from_stored(kind: &str, reference: String) -> Result<Self, ValidationError> {
        match kind {
            "upstream" => Self::upstream(reference),
            "url" => Self::url(reference),
            _ => Err(ValidationError::InvalidArtworkKind),
        }
    }

    pub fn reference(&self) -> &str {
        match self {
            Self::Upstream(value) | Self::Url(value) => value,
        }
    }

    pub fn kind(&self) -> &'static str {
        match self {
            Self::Upstream(_) => "upstream",
            Self::Url(_) => "url",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum CoverArtTarget {
    Track(TrackId),
    Album(AlbumId),
    Artist(ArtistId),
}

impl CoverArtTarget {
    pub fn song(id: &TrackId) -> Self {
        Self::Track(id.clone())
    }

    pub fn album(id: &AlbumId) -> Self {
        Self::Album(id.clone())
    }

    pub fn artist(id: &ArtistId) -> Self {
        Self::Artist(id.clone())
    }

    pub fn encode(&self) -> String {
        match self {
            Self::Track(id) => id.to_string(),
            Self::Album(id) => format!("al-{id}"),
            Self::Artist(id) => format!("ar-{id}"),
        }
    }

    pub fn parse(value: &str) -> Result<Self, uuid::Error> {
        if let Some(id) = value.strip_prefix("al-") {
            return id.parse::<AlbumId>().map(Self::Album);
        }
        if let Some(id) = value.strip_prefix("ar-") {
            return id.parse::<ArtistId>().map(Self::Artist);
        }

        value.parse::<TrackId>().map(Self::Track)
    }
}

fn validate(value: &str, max: usize, field: &'static str) -> Result<(), ValidationError> {
    if value.trim().is_empty() {
        return Err(ValidationError::Empty { field });
    }

    if value.chars().count() > max {
        return Err(ValidationError::TooLong { field });
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn artwork_references_round_trip_through_storage() {
        for artwork in [
            ArtworkRef::upstream("cover-1".to_owned()).unwrap(),
            ArtworkRef::url("https://art.test/cover.jpg".to_owned()).unwrap(),
        ] {
            let restored = ArtworkRef::from_stored(artwork.kind(), artwork.reference().to_owned());
            assert_eq!(restored.unwrap(), artwork);
        }
    }

    #[test]
    fn cover_art_ids_round_trip_through_their_prefixes() {
        let track = CoverArtTarget::song(&TrackId::new());
        let album = CoverArtTarget::album(&AlbumId::new());
        let artist = CoverArtTarget::artist(&ArtistId::new());

        assert!(!track.encode().contains("al-"));
        assert!(album.encode().starts_with("al-"));
        assert!(artist.encode().starts_with("ar-"));

        for target in [track, album, artist] {
            assert_eq!(CoverArtTarget::parse(&target.encode()).unwrap(), target);
        }

        assert!(CoverArtTarget::parse("al-not-a-uuid").is_err());
        assert!(CoverArtTarget::parse("ar-not-a-uuid").is_err());
        assert!(CoverArtTarget::parse("nonsense").is_err());
    }

    #[test]
    fn artwork_references_reject_empty_and_unknown_kinds() {
        assert!(matches!(
            ArtworkRef::upstream("  ".to_owned()),
            Err(ValidationError::Empty {
                field: "artwork_id"
            })
        ));
        assert!(matches!(
            ArtworkRef::url("a".repeat(2049)),
            Err(ValidationError::TooLong {
                field: "artwork_url"
            })
        ));
        assert!(matches!(
            ArtworkRef::from_stored("unknown", "value".to_owned()),
            Err(ValidationError::InvalidArtworkKind)
        ));
    }
}
