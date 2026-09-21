mod domain;
mod memory;
mod pg;
mod ports;

pub use domain::{
    Album, AlbumEntry, AlbumId, AlbumListFilter, AlbumMetadata, AlbumOrder, AlbumSummary,
    AlbumTrack, Artist, ArtistCredit, ArtistId, ArtistSummary, ArtworkRef, DuplicateCandidate,
    DuplicateSource, GenreSummary, KeepReason, LOCAL_PROVIDER_ID, LibraryState, MATCHER_VERSION,
    MAX_RATING, MIN_RATING, MatchDecision, MatchQuality, MatchRecord, MatchStatus, ParsedTitle,
    ProviderDescriptor, ProviderId, ProviderKind, ProviderRegistry, RecordingIndex,
    SearchConfidence, SearchTerms, SourceIdentity, SourceKind, SourceScope, SplitArtist,
    SplitArtistRow, Starred, StateTarget, Track, TrackHit, TrackId, TrackMetadata, ValidationError,
    YOUTUBE_MUSIC_PROVIDER_ID, checked_rating, from_ytm,
};
pub(crate) use domain::{best_field_quality, normalize_for_matching};
pub use memory::InMemoryTrackRepository;
pub use pg::PostgresTrackRepository;
pub use ports::{
    CatalogueWrite, DuplicateReport, LibraryRead, SourceFileFacts, SourceLookup, StateStore,
    TrackRepository, TrackRepositoryError,
};
