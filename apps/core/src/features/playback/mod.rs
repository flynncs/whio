pub mod activity;
pub(crate) mod api;
pub mod delivery;
mod domain;
pub mod media;
mod next;
mod ports;
mod prefetch;
pub mod providers;
pub mod representation;
mod service;
mod source;

pub use activity::{ActivityError, PlaybackActivityService};
pub use delivery::{DeliveryError, DeliveryResult};
pub use domain::{
    ActivityInputError, ActivitySession, LocalMedia, MediaMetadata, PlayableMedia, PlaybackClient,
    PlaybackRate, PlaybackReport, PlaybackState, PlaybackUrl, ValidationError,
};
pub use next::{CatalogueNextTracks, NextTracks};
pub use ports::{PlaybackDelivery, PlaybackProvider, TrackDurationError, TrackDurations};
pub use representation::{
    AacMode, ClientFormat, ClientFormats, Representation, SourceAudio, StreamRequest,
};
pub use service::{PlaybackError, ResolvedPlayback, StreamingService};
pub use source::{LocalSource, PlaybackSource, SourceKey, YoutubeSource};
