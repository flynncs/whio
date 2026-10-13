use crate::features::music_library::domain::{
    AlbumMetadata, AlbumTrack, ArtistCredit, ArtworkRef, SourceIdentity, TrackMetadata,
    ValidationError,
};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CatalogueCandidate {
    source: SourceIdentity,
    alternates: Vec<SourceIdentity>,
    metadata: TrackMetadata,
    album: Option<AlbumMetadata>,
    artwork: Option<ArtworkRef>,
    track_number: Option<u16>,
    disc_number: Option<u16>,
}

impl CatalogueCandidate {
    pub fn new(source: SourceIdentity, metadata: TrackMetadata) -> Self {
        Self {
            source,
            alternates: Vec::new(),
            metadata,
            album: None,
            artwork: None,
            track_number: None,
            disc_number: None,
        }
    }

    pub fn with_alternates(mut self, alternates: Vec<SourceIdentity>) -> Self {
        self.alternates = alternates;
        self
    }

    pub fn with_album(mut self, album: Option<AlbumMetadata>) -> Self {
        self.album = album;
        self
    }

    pub fn with_artwork(mut self, artwork: Option<ArtworkRef>) -> Self {
        self.artwork = artwork;
        self
    }

    pub fn with_track_numbers(
        mut self,
        disc_number: Option<u16>,
        track_number: Option<u16>,
    ) -> Result<Self, ValidationError> {
        AlbumTrack::new(disc_number, track_number, 0)?;
        self.disc_number = disc_number;
        self.track_number = track_number;
        Ok(self)
    }

    pub fn album_track(&self, position: u32) -> AlbumTrack {
        AlbumTrack::new(self.disc_number, self.track_number, position)
            .expect("candidate numbers are validated")
    }

    pub fn source(&self) -> &SourceIdentity {
        &self.source
    }

    pub fn alternates(&self) -> &[SourceIdentity] {
        &self.alternates
    }

    pub fn metadata(&self) -> &TrackMetadata {
        &self.metadata
    }

    pub fn album(&self) -> Option<&AlbumMetadata> {
        self.album.as_ref()
    }

    pub fn artwork(&self) -> Option<&ArtworkRef> {
        self.artwork.as_ref()
    }

    pub fn into_parts(self) -> (SourceIdentity, TrackMetadata) {
        (self.source, self.metadata)
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
}
