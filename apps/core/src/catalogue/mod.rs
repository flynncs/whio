mod completion;
mod dedup;
mod domain;
mod error;
mod library;
mod materialize;
mod ports;
mod search;

pub use dedup::DedupMode;

pub use completion::CompletionService;
pub use domain::{
    CatalogueCandidate, CatalogueSearch, FetchedAlbum, MAX_SEARCH_OFFSET, SearchLimit,
    SearchOffset, SearchQuery, ValidationError,
};
pub use error::CatalogueError;
pub use library::{AlbumDetail, ArtistDetail, LibraryService};
pub use ports::CatalogueProvider;
pub use search::{SearchHit, SearchOutcome, SearchService};
