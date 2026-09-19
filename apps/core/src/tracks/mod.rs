mod domain;
mod in_memory_repository;
mod postgres_repository;
mod repository;

pub use domain::{
    Album, AlbumId, AlbumMetadata, AlbumTrack, Artist, ArtistCredit, ArtistId, ArtworkRef,
    ProviderId, ProviderKind, SourceIdentity, SourceScope, Track, TrackId, TrackMetadata,
    ValidationError,
};
pub use in_memory_repository::InMemoryTrackRepository;
pub use postgres_repository::PostgresTrackRepository;
pub use repository::{SourceFileFacts, TrackRepository, TrackRepositoryError};
