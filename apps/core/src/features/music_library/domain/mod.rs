mod album;
mod artist;
mod artist_information;
mod artwork;
mod dedup;
mod duplicate_report;
mod enrichment;
mod error;
mod history;
mod metadata;
mod page;
mod playlist;
mod provider_state;
mod query;
mod queue;
mod retention;
mod search;
mod source;
mod source_kind;
mod state;
mod text;
mod track;

pub use album::{
    Album, AlbumEntry, AlbumId, AlbumListFilter, AlbumListScope, AlbumMetadata, AlbumOrder,
    AlbumPage, AlbumSummary, AlbumTrack, DatePrecision, PartialDate,
};
pub use artist::{
    Artist, ArtistCredit, ArtistId, ArtistListFilter, ArtistPage, ArtistSort, ArtistSummary,
    CreditRole,
};
pub use artist_information::{ArtistInformation, RefreshDemand, RefreshStatus};
pub use artwork::ArtworkRef;
pub use dedup::{
    MATCHER_VERSION, MatchDecision, MatchRecord, MatchStatus, ParsedTitle, RecordingIndex,
    strict_recording_match,
};
pub use duplicate_report::{
    DuplicateCandidate, DuplicateSource, KeepReason, SplitArtist, SplitArtistRow,
};
pub use enrichment::{EnrichmentSubject, identity_revision};
pub use error::ValidationError;
pub use history::{
    ListenCount, ListenCoverage, ListenExtent, ListenOrder, ListenOrigin, ListenOutcome,
    ListenPage, ListenQuery, ListenRecord, ListenSessionId, ListenSubmission, ListenWindow,
};
pub use metadata::TrackMetadata;
pub use page::{MAX_PAGE_LIMIT, PageLimit, TrackListFilter, TrackListScope, TrackOrder, TrackPage};
pub use playlist::{
    CommentPatch, MoveTarget, OccurrenceSelector, Playlist, PlaylistEntryChange,
    PlaylistEntryError, PlaylistEntryId, PlaylistId, PlaylistMetadataPatch, PlaylistOccurrence,
    PlaylistOrder, PlaylistWrite, PlaylistWriteOutcome,
};
pub use provider_state::ProviderEntityState;
pub use query::{
    FullTextSelector, IdSet, MAX_SELECTOR_IDS, MAX_SELECTOR_TEXT, RandomSeed, Range, Resource,
    RoleSelector, RoleSet, SelectorText, SortDirection,
};
pub use queue::{
    RestoreQueueEntry, RestoreQueueEntryId, RestoreQueueSnapshot, RestoreQueueValidationError,
    RestoreQueueWrite, RestoreQueueWriteOutcome,
};
pub use retention::{
    AlbumRefetch, ArtistMerge, DuplicateKind, EvictionReport, ProviderDuplicate,
    ProviderDuplicateReport, RetentionReport, SkipReason, SkippedDuplicate, TrackMerge,
};
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
pub use track::{SourceRepresentationFacts, Track, TrackHit, TrackId, TrackView};
