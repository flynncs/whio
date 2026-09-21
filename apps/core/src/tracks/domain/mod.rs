mod album;
mod artist;
mod artwork;
mod duplicates;
mod error;
mod metadata;
mod source;
mod state;
mod text;
mod track;

pub use album::{
    Album, AlbumEntry, AlbumId, AlbumListFilter, AlbumMetadata, AlbumOrder, AlbumSummary,
    AlbumTrack,
};
pub use artist::{Artist, ArtistCredit, ArtistId, ArtistSummary};
pub use artwork::ArtworkRef;
pub use duplicates::{
    DuplicateCandidate, DuplicateSource, KeepReason, SplitArtist, SplitArtistRow,
};
pub use error::ValidationError;
pub use metadata::TrackMetadata;
pub use source::{
    LOCAL_PROVIDER_ID, ProviderId, ProviderKind, SourceIdentity, SourceScope,
    YOUTUBE_MUSIC_PROVIDER_ID,
};
pub use state::{
    GenreSummary, LibraryState, MAX_RATING, MIN_RATING, Starred, StateTarget, checked_rating,
};
pub(crate) use text::normalize_for_matching;
pub use track::{Track, TrackHit, TrackId};
