use std::{
    fmt::{self, Display},
    str::FromStr,
};

use uuid::Uuid;

use super::TrackMetadata;

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

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Ok(Self(Uuid::parse_str(value)?))
    }
}

impl Display for TrackId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}", self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Track {
    id: TrackId,
    metadata: TrackMetadata,
}

impl Track {
    pub fn new(id: TrackId, metadata: TrackMetadata) -> Self {
        Self { id, metadata }
    }

    pub fn id(&self) -> &TrackId {
        &self.id
    }

    pub fn metadata(&self) -> &TrackMetadata {
        &self.metadata
    }

    pub fn title(&self) -> &str {
        self.metadata.title()
    }

    pub fn artists(&self) -> &[String] {
        self.metadata.artists()
    }

    pub fn duration_ms(&self) -> Option<u64> {
        self.metadata.duration_ms()
    }

    pub fn into_parts(self) -> (TrackId, TrackMetadata) {
        (self.id, self.metadata)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn metadata() -> TrackMetadata {
        TrackMetadata::new(
            "Instant Crush".to_owned(),
            vec!["Daft Punk".to_owned()],
            Some(337_000),
        )
        .unwrap()
    }

    #[test]
    fn track_id_round_trips_through_display_and_parsing() {
        let original = TrackId::new();
        let parsed = original.to_string().parse::<TrackId>().unwrap();

        assert_eq!(parsed, original);
    }

    #[test]
    fn track_has_whio_owned_identity_and_valid_metadata() {
        let id = TrackId::new();
        let track = Track::new(id.clone(), metadata());

        assert_eq!(track.id(), &id);
        assert_eq!(track.title(), "Instant Crush");
        assert_eq!(track.artists(), ["Daft Punk"]);
        assert_eq!(track.duration_ms(), Some(337_000));
    }
}
