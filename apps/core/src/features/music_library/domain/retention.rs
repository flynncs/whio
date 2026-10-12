use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DuplicateKind {
    Track,
    Album,
    Artist,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderDuplicate {
    pub kind: DuplicateKind,
    pub id: Uuid,
    pub name: String,
    pub duplicate_of: Uuid,
    pub listens: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkipReason {
    PlaylistEntry,
    QueueEntry,
    OverlappingListens,
    AcquisitionOrigin,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkippedDuplicate {
    pub kind: DuplicateKind,
    pub id: Uuid,
    pub name: String,
    pub reason: SkipReason,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtistMerge {
    pub id: Uuid,
    pub name: String,
    pub into: Uuid,
    pub credits: u64,
    pub albums_moved: u64,
    pub albums_merged: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrackMerge {
    pub id: Uuid,
    pub name: String,
    pub into: Uuid,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AlbumRefetch {
    pub id: Uuid,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ProviderDuplicateReport {
    pub merges: Vec<ArtistMerge>,
    pub track_merges: Vec<TrackMerge>,
    pub refetch: Vec<AlbumRefetch>,
    pub duplicates: Vec<ProviderDuplicate>,
    pub skipped: Vec<SkippedDuplicate>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RetentionReport {
    pub evictable_tracks: u64,
    pub evictable_albums: u64,
    pub evictable_artists: u64,
    pub play_held_tracks: u64,
    pub play_held_albums: u64,
    pub play_held_artists: u64,
    pub oldest_played_at: Option<chrono::DateTime<chrono::Utc>>,
    pub newest_played_at: Option<chrono::DateTime<chrono::Utc>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct EvictionReport {
    pub tracks: u64,
    pub albums: u64,
    pub artists: u64,
    pub evicted_artist_ids: Vec<Uuid>,
}
