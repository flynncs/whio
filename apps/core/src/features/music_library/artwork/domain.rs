use crate::features::music_library::domain::{AlbumId, ArtistId, TrackId};

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

#[cfg(test)]
mod tests {
    use super::*;

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
}
