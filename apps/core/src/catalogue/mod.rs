mod domain;
mod ports;
mod service;

pub use domain::{
    CatalogueCandidate, CatalogueSearch, MAX_SEARCH_OFFSET, SearchLimit, SearchOffset, SearchQuery,
    ValidationError,
};
pub use ports::CatalogueProvider;
pub use service::{CatalogueError, CatalogueService, SearchOutcome};
