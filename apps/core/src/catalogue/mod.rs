mod dedup;
mod domain;
mod error;
mod library;
mod ports;
mod search;

pub use dedup::DedupMode;

pub use domain::{
    CatalogueCandidate, CatalogueSearch, MAX_SEARCH_OFFSET, SearchLimit, SearchOffset, SearchQuery,
    ValidationError,
};
pub use error::CatalogueError;
pub use library::{AlbumDetail, ArtistDetail, LibraryService};
pub use ports::CatalogueProvider;
pub use search::{SearchHit, SearchOutcome, SearchService};
