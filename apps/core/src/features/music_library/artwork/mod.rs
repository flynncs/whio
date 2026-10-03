mod cache;
mod domain;
pub(crate) mod remote;
pub(crate) mod response;
mod service;
pub(crate) mod thumbnail;

#[cfg(any(test, feature = "postgres-integration"))]
pub mod test_support {
    use std::sync::Arc;

    use crate::features::music_library::storage::contracts::{
        LibraryRead, SourceLookup, StateStore,
    };

    use super::ArtworkService;

    pub struct Ports {
        pub library: Arc<dyn LibraryRead>,
        pub sources: Arc<dyn SourceLookup>,
        pub state: Arc<dyn StateStore>,
    }

    impl Ports {
        pub fn new(
            library: Arc<dyn LibraryRead>,
            sources: Arc<dyn SourceLookup>,
            state: Arc<dyn StateStore>,
        ) -> Self {
            Self {
                library,
                sources,
                state,
            }
        }

        pub fn artwork(&self, host_suffixes: Vec<String>) -> ArtworkService {
            ArtworkService::new(
                Arc::clone(&self.library),
                Arc::clone(&self.sources),
                host_suffixes,
            )
        }
    }
}

pub use cache::{ArtworkCache, ReadyAsset};
pub use domain::{
    ARTWORK_DERIVED_VARIANTS, ArtworkAssetDescriptor, ArtworkRole, ArtworkVariant, CoverArtTarget,
    FetchedImage, PortraitGate, PromotionOutcome, RefusalReason,
};
pub use service::{Artwork, ArtworkError, ArtworkOutcome, ArtworkService};

pub const DEFAULT_HOST_SUFFIXES: &[&str] = &["googleusercontent.com", "ytimg.com"];

pub fn default_host_suffixes() -> Vec<String> {
    DEFAULT_HOST_SUFFIXES
        .iter()
        .map(|suffix| (*suffix).to_owned())
        .collect()
}
