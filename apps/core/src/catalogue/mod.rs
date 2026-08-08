mod domain;
mod ports;

pub use domain::{CatalogueCandidate, ProviderId, SourceIdentity, Track, TrackId, ValidationError};
pub use ports::CatalogueResolver;
