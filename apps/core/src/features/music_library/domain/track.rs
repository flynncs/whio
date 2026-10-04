use std::{
    fmt::{self, Display},
    str::FromStr,
};

use uuid::Uuid;

use super::{
    Album, AlbumTrack, ArtistCredit, LibraryState, MatchQuality, SourceIdentity, TrackMetadata,
};

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
    match_quality: Option<MatchQuality>,
}

impl TrackHit {
    pub fn new(track: Track, album: Option<Album>, sources: Vec<SourceIdentity>) -> Self {
        Self {
            track,
            album,
            sources,
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

    pub fn state(&self) -> LibraryState {
        self.state
    }

    pub fn match_quality(&self) -> Option<MatchQuality> {
        self.match_quality
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
        if other.match_quality.is_some() {
            self.match_quality = other.match_quality;
        }
        for source in other.sources {
            if !self.sources.contains(&source) {
                self.sources.push(source);
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceRepresentationFacts {
    pub source: SourceIdentity,
    pub content_type: Option<String>,
    pub suffix: Option<String>,
    pub container: Option<String>,
    pub codec: Option<String>,
    pub size_bytes: Option<u64>,
    pub bitrate_kbps: Option<u32>,
    pub sample_rate_hz: Option<u32>,
    pub bit_depth: Option<u8>,
    pub channels: Option<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrackView {
    track: Track,
    album: Option<Album>,
    membership: Option<AlbumTrack>,
    album_artist: Option<String>,
    state: LibraryState,
    sources: Vec<SourceIdentity>,
    representation: Option<SourceRepresentationFacts>,
}

impl TrackView {
    pub fn new(
        track: Track,
        album: Option<Album>,
        membership: Option<AlbumTrack>,
        album_artist: Option<String>,
        state: LibraryState,
        sources: Vec<SourceIdentity>,
    ) -> Self {
        Self {
            track,
            album,
            membership,
            album_artist,
            state,
            sources,
            representation: None,
        }
    }

    pub fn with_representation(
        mut self,
        representation: Option<SourceRepresentationFacts>,
    ) -> Self {
        self.representation = representation;
        self
    }

    pub fn representation(&self) -> Option<&SourceRepresentationFacts> {
        self.representation.as_ref()
    }

    pub fn track(&self) -> &Track {
        &self.track
    }

    pub fn album(&self) -> Option<&Album> {
        self.album.as_ref()
    }

    pub fn membership(&self) -> Option<&AlbumTrack> {
        self.membership.as_ref()
    }

    pub fn album_artist(&self) -> Option<&str> {
        self.album_artist.as_deref()
    }

    pub fn state(&self) -> &LibraryState {
        &self.state
    }

    pub fn sources(&self) -> &[SourceIdentity] {
        &self.sources
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
