use bytes::Bytes;
use uuid::Uuid;

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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ArtworkRole {
    Portrait,
    Wide,
}

impl ArtworkRole {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Portrait => "portrait",
            Self::Wide => "wide",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "portrait" => Some(Self::Portrait),
            "wide" => Some(Self::Wide),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ArtworkVariant {
    Original,
    Px256,
    Px768,
    Px1600,
}

pub const ARTWORK_DERIVED_VARIANTS: [ArtworkVariant; 3] = [
    ArtworkVariant::Px256,
    ArtworkVariant::Px768,
    ArtworkVariant::Px1600,
];

impl ArtworkVariant {
    pub fn size(self) -> Option<u32> {
        match self {
            Self::Original => None,
            Self::Px256 => Some(256),
            Self::Px768 => Some(768),
            Self::Px1600 => Some(1600),
        }
    }

    pub fn from_size(size: u32) -> Self {
        match size {
            0..=256 => Self::Px256,
            257..=768 => Self::Px768,
            769..=1600 => Self::Px1600,
            _ => Self::Original,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Original => "original",
            Self::Px256 => "px256",
            Self::Px768 => "px768",
            Self::Px1600 => "px1600",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "original" => Some(Self::Original),
            "px256" => Some(Self::Px256),
            "px768" => Some(Self::Px768),
            "px1600" => Some(Self::Px1600),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtworkAssetDescriptor {
    pub id: Uuid,
    pub subject: ArtistId,
    pub role: ArtworkRole,
    pub source_provider_id: String,
    pub source_external_id: String,
    pub revision: i64,
    pub variant: ArtworkVariant,
    pub content_digest: String,
    pub content_type: String,
    pub width: u32,
    pub height: u32,
    pub byte_size: i64,
    pub file_name: String,
}

#[derive(Debug, Clone)]
pub struct FetchedImage {
    pub content_type: String,
    pub bytes: Bytes,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromotionOutcome {
    Promoted { revision: i64 },
    AlreadyReady,
    Refused(RefusalReason),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PortraitGate {
    Servable,
    ReferenceOnly,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefusalReason {
    Undecodable,
    TooManyPixels,
    TooManyBytes,
    NoCacheDir,
    NotServable,
    DisallowedReference,
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

    #[test]
    fn variants_round_trip_and_map_requests_to_the_nearest_target() {
        for variant in [ArtworkVariant::Original]
            .into_iter()
            .chain(ARTWORK_DERIVED_VARIANTS)
        {
            assert_eq!(ArtworkVariant::parse(variant.as_str()), Some(variant));
        }
        assert_eq!(ArtworkVariant::parse("px1024"), None);

        assert_eq!(ArtworkVariant::from_size(16), ArtworkVariant::Px256);
        assert_eq!(ArtworkVariant::from_size(256), ArtworkVariant::Px256);
        assert_eq!(ArtworkVariant::from_size(257), ArtworkVariant::Px768);
        assert_eq!(ArtworkVariant::from_size(768), ArtworkVariant::Px768);
        assert_eq!(ArtworkVariant::from_size(769), ArtworkVariant::Px1600);
        assert_eq!(ArtworkVariant::from_size(1600), ArtworkVariant::Px1600);
        assert_eq!(ArtworkVariant::from_size(1601), ArtworkVariant::Original);
        assert_eq!(ArtworkVariant::from_size(5000), ArtworkVariant::Original);

        assert_eq!(ArtworkVariant::Original.size(), None);
        assert_eq!(ArtworkVariant::Px256.size(), Some(256));
        assert_eq!(ArtworkVariant::Px768.size(), Some(768));
        assert_eq!(ArtworkVariant::Px1600.size(), Some(1600));
    }

    #[test]
    fn roles_round_trip_through_their_stored_names() {
        assert_eq!(
            ArtworkRole::parse(ArtworkRole::Portrait.as_str()),
            Some(ArtworkRole::Portrait)
        );
        assert_eq!(
            ArtworkRole::parse(ArtworkRole::Wide.as_str()),
            Some(ArtworkRole::Wide)
        );
        assert_eq!(ArtworkRole::parse("cover"), None);
    }
}
