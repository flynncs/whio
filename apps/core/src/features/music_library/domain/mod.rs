mod album;
mod artist;
mod artwork;
mod dedup;
mod duplicate_report;
mod error;
mod metadata;
mod page;
mod playlist;
mod provider_state;
mod retention;
mod search;
mod source;
mod source_kind;
mod state;
mod text;
mod track;

pub use album::{
    Album, AlbumEntry, AlbumId, AlbumListFilter, AlbumMetadata, AlbumOrder, AlbumSummary,
    AlbumTrack,
};
pub use artist::{Artist, ArtistCredit, ArtistId, ArtistSummary};
pub use artwork::ArtworkRef;
pub use dedup::{
    MATCHER_VERSION, MatchDecision, MatchRecord, MatchStatus, ParsedTitle, RecordingIndex,
};
pub use duplicate_report::{
    DuplicateCandidate, DuplicateSource, KeepReason, SplitArtist, SplitArtistRow,
};
pub use error::ValidationError;
pub use metadata::TrackMetadata;
pub use page::{MAX_PAGE_LIMIT, PageLimit, TrackListFilter, TrackListScope, TrackOrder, TrackPage};
pub use playlist::{Playlist, PlaylistId};
pub use provider_state::ProviderEntityState;
pub use retention::{EvictionReport, RetentionReport};
pub use search::{MatchQuality, SearchConfidence, SearchTerms};
pub(crate) use search::{album_quality, best_field_quality, track_quality};
pub use source::{
    LOCAL_PROVIDER_ID, ProviderId, ProviderKind, SourceIdentity, SourceScope,
    YOUTUBE_MUSIC_PROVIDER_ID,
};
pub use source_kind::{Availability, SourceKind};
pub use state::{
    GenreSummary, LibraryState, MAX_RATING, MIN_RATING, Starred, StateTarget, checked_rating,
};
pub(crate) use text::normalize_for_matching;
pub use track::{Track, TrackHit, TrackId, TrackView};
