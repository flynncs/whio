mod domain;
mod memory;
mod pg;
mod ports;

pub(crate) use domain::best_field_quality;
pub use domain::{
    Album, AlbumEntry, AlbumId, AlbumListFilter, AlbumMetadata, AlbumOrder, AlbumSummary,
    AlbumTrack, Artist, ArtistCredit, ArtistId, ArtistSummary, ArtworkRef, DuplicateCandidate,
    DuplicateSource, GenreSummary, KeepReason, LOCAL_PROVIDER_ID, LibraryState, MATCHER_VERSION,
    MAX_RATING, MIN_RATING, MatchDecision, MatchQuality, MatchRecord, MatchStatus, ParsedTitle,
    ProviderId, ProviderKind, RecordingIndex, SearchConfidence, SearchTerms, SourceIdentity,
    SourceScope, SplitArtist, SplitArtistRow, Starred, StateTarget, Track, TrackHit, TrackId,
    TrackMetadata, ValidationError, YOUTUBE_MUSIC_PROVIDER_ID, checked_rating, from_ytm,
};
pub use memory::InMemoryTrackRepository;
pub use pg::PostgresTrackRepository;
pub use ports::{
    CatalogueWrite, DuplicateReport, LibraryRead, SourceFileFacts, SourceLookup, StateStore,
    TrackRepository, TrackRepositoryError,
};
