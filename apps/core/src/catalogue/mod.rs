mod domain;
mod ports;
mod service;

pub use domain::{
    CatalogueCandidate, CatalogueSearch, ProviderId, SearchLimit, SearchQuery, SourceIdentity,
    Track, TrackId, TrackMetadata, ValidationError,
};
pub use ports::CatalogueResolver;
pub use service::{CatalogueError, CatalogueService};
