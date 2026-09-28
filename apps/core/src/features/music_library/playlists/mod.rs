mod service;

#[cfg(all(test, feature = "postgres-integration"))]
#[path = "tests.rs"]
mod tests;

pub use service::{PlaylistDetail, PlaylistEdit, PlaylistEntry, PlaylistError, PlaylistService};
