mod completion;
mod dedup;
mod domain;
mod error;
mod library;
mod materialize;
mod ports;
mod provider_error;
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
pub use provider_error::ProviderError;
pub use search::{
    SearchHit, SearchOutcome, SearchRequest, SearchService, SearchWindow, SearchWindows,
    TrackSearch,
};

pub use crate::features::music_library::domain::{
    AlbumListFilter, AlbumListScope, AlbumOrder, ArtistListFilter, ArtistSort, IdSet,
    MAX_PAGE_LIMIT, MAX_SELECTOR_IDS, MAX_SELECTOR_TEXT, PageLimit, RandomSeed, Range, Resource,
    RoleSelector, RoleSet, SelectorText, SortDirection, TrackListFilter, TrackListScope,
    TrackOrder, TrackPage,
};
