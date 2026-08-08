mod domain;
mod ports;
mod service;

pub use domain::{CatalogueCandidate, CatalogueSearch, SearchLimit, SearchQuery, ValidationError};
pub use ports::CatalogueResolver;
pub use service::{CatalogueError, CatalogueService};
