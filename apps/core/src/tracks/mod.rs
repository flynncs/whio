mod domain;
mod in_memory_repository;
mod postgres_repository;
mod repository;

pub(crate) use domain::best_field_quality;
pub use domain::{
    Album, AlbumEntry, AlbumId, AlbumListFilter, AlbumMetadata, AlbumOrder, AlbumSummary,
    AlbumTrack, Artist, ArtistCredit, ArtistId, ArtistSummary, ArtworkRef, DuplicateCandidate,
    DuplicateSource, GenreSummary, KeepReason, LOCAL_PROVIDER_ID, LibraryState, MAX_RATING,
    MIN_RATING, MatchQuality, ProviderId, ProviderKind, SearchConfidence, SearchTerms,
    SourceIdentity, SourceScope, SplitArtist, SplitArtistRow, Starred, StateTarget, Track,
    TrackHit, TrackId, TrackMetadata, ValidationError, YOUTUBE_MUSIC_PROVIDER_ID, checked_rating,
};
pub use in_memory_repository::InMemoryTrackRepository;
pub use postgres_repository::PostgresTrackRepository;
pub use repository::{SourceFileFacts, TrackRepository, TrackRepositoryError};
