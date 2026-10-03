use crate::features::metadata::domain::MetadataRecord;
use crate::features::metadata::{RefreshOutcome, RefreshState};
use crate::features::music_library::artwork::ArtworkAssetDescriptor;
use crate::features::music_library::catalogue::ArtistDetail;

pub struct ArtistInformation {
    pub artist: ArtistDetail,
    pub metadata: MetadataRecord,
    pub portraits: Vec<ArtworkAssetDescriptor>,
    pub cover_art: String,
    pub refresh: RefreshStatus,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefreshStatus {
    pub state: RefreshState,
    pub outcome: Option<RefreshOutcome>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefreshDemand {
    ReadOnly,
    RequestIfDue,
    Force,
}
