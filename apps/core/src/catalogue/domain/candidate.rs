use super::{SourceIdentity, TrackMetadata};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CatalogueCandidate {
    source: SourceIdentity,
    metadata: TrackMetadata,
}

impl CatalogueCandidate {
    pub fn new(source: SourceIdentity, metadata: TrackMetadata) -> Self {
        Self { source, metadata }
    }

    pub fn source(&self) -> &SourceIdentity {
        &self.source
    }

    pub fn metadata(&self) -> &TrackMetadata {
        &self.metadata
    }

    pub fn into_parts(self) -> (SourceIdentity, TrackMetadata) {
        (self.source, self.metadata)
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
}
