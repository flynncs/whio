use std::{
    fmt::{self, Display},
    str::FromStr,
};

use uuid::Uuid;

use super::{Album, ArtistCredit, LibraryState, SourceIdentity, TrackMetadata};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TrackId(Uuid);

impl TrackId {
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

    pub fn artists(&self) -> &[ArtistCredit] {
        self.metadata.artists()
    }

    pub fn duration_ms(&self) -> Option<u64> {
        self.metadata.duration_ms()
    }

    pub fn musicbrainz_recording_id(&self) -> Option<&str> {
        self.metadata.musicbrainz_recording_id()
    }

    pub fn isrc(&self) -> Option<&str> {
        self.metadata.isrc()
    }

    pub fn into_parts(self) -> (TrackId, TrackMetadata) {
        (self.id, self.metadata)
    }
}

#[derive(Debug, Clone)]
pub struct TrackHit {
    track: Track,
    album: Option<Album>,
    sources: Vec<SourceIdentity>,
    state: LibraryState,
}

impl TrackHit {
    pub fn new(track: Track, album: Option<Album>, sources: Vec<SourceIdentity>) -> Self {
        Self {
            track,
            album,
            sources,
            state: LibraryState::default(),
        }
    }

    pub fn with_state(mut self, state: LibraryState) -> Self {
        self.state = state;
        self
    }

    pub fn state(&self) -> LibraryState {
        self.state
    }

    pub fn track(&self) -> &Track {
        &self.track
    }

    pub fn album(&self) -> Option<&Album> {
        self.album.as_ref()
    }

    pub fn into_track(self) -> Track {
        self.track
    }

    pub fn sources(&self) -> &[SourceIdentity] {
        &self.sources
    }

    pub(crate) fn merge(&mut self, other: TrackHit) {
        self.track = other.track;
        if other.album.is_some() {
            self.album = other.album;
        }
        for source in other.sources {
            if !self.sources.contains(&source) {
                self.sources.push(source);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn metadata() -> TrackMetadata {
        TrackMetadata::from_names(
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
        assert_eq!(
            track
                .artists()
                .iter()
                .map(|artist| artist.name())
                .collect::<Vec<_>>(),
            ["Daft Punk"]
        );
        assert_eq!(track.duration_ms(), Some(337_000));
    }
}
