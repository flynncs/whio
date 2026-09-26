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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct EvictionReport {
    pub tracks: u64,
    pub albums: u64,
    pub artists: u64,
}
