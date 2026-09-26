mod domain;
pub(crate) mod remote;
pub(crate) mod response;
mod service;
pub(crate) mod thumbnail;

pub use domain::CoverArtTarget;
pub use service::{Artwork, ArtworkError, ArtworkOutcome, ArtworkService};

pub const DEFAULT_HOST_SUFFIXES: &[&str] = &["googleusercontent.com", "ytimg.com"];

pub fn default_host_suffixes() -> Vec<String> {
    DEFAULT_HOST_SUFFIXES
        .iter()
        .map(|suffix| (*suffix).to_owned())
        .collect()
}
