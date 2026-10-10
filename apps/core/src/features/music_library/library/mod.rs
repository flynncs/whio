mod registration;
mod scan_service;
mod scanner;
mod tags;
mod watcher;

pub use registration::{
    DEFAULT_LABEL, Library, LibraryId, LibraryRegistration, LibraryRegistrationError,
    LibraryRegistry, MAX_LABEL_LENGTH,
};
pub(crate) use registration::{deduplicated_roots, is_label_token};
pub use scan_service::{ScanRequestError, ScanService, ScanStatus};
pub use scanner::{LOCAL_PROVIDER, ScanError, ScanMode, ScanReport, scan, scan_with};
pub use tags::{EmbeddedPicture, TagError, TrackTags, read_cover_art, read_tags};
pub(crate) use tags::{LOCAL_MAX_PICTURE_BYTES, sniff_picture};

#[cfg(all(test, feature = "postgres-integration"))]
pub(crate) use tags::write_pictured_flac;
pub use watcher::{MediaWatcher, WatchError, watch};
