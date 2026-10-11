mod completion;
mod dedup;
mod domain;
mod error;
mod library;
mod materialize;
mod ports;
mod provider_error;
mod search;

pub(crate) const CANDIDATE_LIMIT: u32 = 500;

pub use dedup::DedupMode;

pub use completion::CompletionService;
pub use domain::{
    CatalogueCandidate, CataloguePage, CatalogueSearch, FetchedAlbum, MAX_SEARCH_OFFSET,
    SearchLimit, SearchOffset, SearchQuery, ValidationError,
};
pub use error::CatalogueError;
pub use library::{AlbumDetail, ArtistDetail, LibraryService};
pub use ports::CatalogueProvider;
pub use provider_error::ProviderError;
pub use search::{
    Discovery, SearchHit, SearchKind, SearchOutcome, SearchRequest, SearchService, SearchWindow,
    SearchWindows, TrackSearch,
};

pub use crate::features::music_library::domain::{
    AlbumListFilter, AlbumListScope, AlbumOrder, AlbumPage, ArtistListFilter, ArtistSort, IdSet,
    MAX_PAGE_LIMIT, MAX_SELECTOR_IDS, MAX_SELECTOR_TEXT, PageLimit, RandomSeed, Range, Resource,
    RoleSelector, RoleSet, SelectorText, SortDirection, TrackListFilter, TrackListScope,
    TrackOrder, TrackPage,
};
