use crate::features::music_library::domain::{
    AlbumMetadata, ArtistCredit, ArtworkRef, SourceIdentity, TrackMetadata,
};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CatalogueCandidate {
    source: SourceIdentity,
    metadata: TrackMetadata,
    album: Option<AlbumMetadata>,
    artwork: Option<ArtworkRef>,
}

impl CatalogueCandidate {
    pub fn new(source: SourceIdentity, metadata: TrackMetadata) -> Self {
        Self {
            source,
            metadata,
            album: None,
            artwork: None,
        }
    }

    pub fn with_album(mut self, album: Option<AlbumMetadata>) -> Self {
        self.album = album;
        self
    }

    pub fn with_artwork(mut self, artwork: Option<ArtworkRef>) -> Self {
        self.artwork = artwork;
        self
    }

    pub fn source(&self) -> &SourceIdentity {
        &self.source
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
