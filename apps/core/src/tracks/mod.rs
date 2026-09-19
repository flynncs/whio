mod domain;
mod in_memory_repository;
mod postgres_repository;
mod repository;

pub use domain::{
    Album, AlbumEntry, AlbumId, AlbumListFilter, AlbumMetadata, AlbumOrder, AlbumSummary,
    AlbumTrack, Artist, ArtistCredit, ArtistId, ArtistSummary, ArtworkRef, GenreSummary,
    LOCAL_PROVIDER_ID, LibraryState, MAX_RATING, MIN_RATING, ProviderId, ProviderKind,
    SourceIdentity, SourceScope, Starred, StateTarget, Track, TrackHit, TrackId, TrackMetadata,
    ValidationError, YOUTUBE_MUSIC_PROVIDER_ID, checked_rating,
};
pub use in_memory_repository::InMemoryTrackRepository;
pub use postgres_repository::PostgresTrackRepository;
pub use repository::{SourceFileFacts, TrackRepository, TrackRepositoryError};
