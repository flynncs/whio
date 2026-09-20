mod scan_service;
mod scanner;
mod tags;
mod watcher;

pub use scan_service::{ScanService, ScanStatus};
pub use scanner::{LOCAL_PROVIDER, ScanError, ScanMode, ScanReport, scan, scan_with};
pub use tags::{
    EmbeddedPicture, IdentityRung, TagError, TrackTags, identity_for, read_cover_art, read_tags,
};

#[cfg(test)]
pub(crate) use tags::write_pictured_flac;
pub use watcher::{MediaWatcher, WatchError, watch};
