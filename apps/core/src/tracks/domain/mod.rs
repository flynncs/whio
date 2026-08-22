mod error;
mod metadata;
mod source;
mod track;

pub use error::ValidationError;
pub use metadata::TrackMetadata;
pub use source::{ProviderId, SourceIdentity, SourceScope};
pub use track::{Track, TrackId};
