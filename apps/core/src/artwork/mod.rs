mod domain;
mod service;
pub(crate) mod thumbnail;

pub use domain::CoverArtTarget;
pub use service::{Artwork, ArtworkError, ArtworkOutcome, ArtworkService};
