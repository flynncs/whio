mod domain;
mod in_memory_repository;
mod postgres_repository;
mod repository;

pub use domain::{
    Album, AlbumEntry, AlbumId, AlbumMetadata, AlbumOrder, AlbumSummary, AlbumTrack, Artist,
    ArtistCredit, ArtistId, ArtistSummary, ArtworkRef, LOCAL_PROVIDER_ID, ProviderId, ProviderKind,
    SourceIdentity, SourceScope, Track, TrackHit, TrackId, TrackMetadata, ValidationError,
    YOUTUBE_MUSIC_PROVIDER_ID,
};
pub use in_memory_repository::InMemoryTrackRepository;
pub use postgres_repository::PostgresTrackRepository;
pub use repository::{SourceFileFacts, TrackRepository, TrackRepositoryError};
