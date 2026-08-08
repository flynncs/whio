mod candidate;
mod error;
mod metadata;
mod search;
mod source;
mod track;

pub use candidate::CatalogueCandidate;
pub use error::ValidationError;
pub use metadata::TrackMetadata;
pub use search::{CatalogueSearch, SearchLimit, SearchQuery};
pub use source::{ProviderId, SourceIdentity};
pub use track::{Track, TrackId};
