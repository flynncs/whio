mod album;
mod artist;
mod artwork;
mod error;
mod metadata;
mod source;
mod text;
mod track;

pub use album::{Album, AlbumEntry, AlbumId, AlbumMetadata, AlbumSummary, AlbumTrack};
pub use artist::{Artist, ArtistCredit, ArtistId, ArtistSummary};
pub use artwork::ArtworkRef;
pub use error::ValidationError;
pub use metadata::TrackMetadata;
pub use source::{ProviderId, ProviderKind, SourceIdentity, SourceScope};
pub(crate) use text::normalize_for_matching;
pub use track::{Track, TrackId};
