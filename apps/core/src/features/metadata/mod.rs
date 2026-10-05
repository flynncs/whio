pub mod capabilities;
pub mod domain;
pub mod freshness;
pub mod licences;
pub mod providers;
pub mod refresh;
pub mod selection;
pub mod service;
pub mod storage;

pub use capabilities::{CapabilityError, CapabilitySnapshot, CapabilitySource, Snapshot};
pub use freshness::{Jitter, NoJitter};
pub use providers::{DocumentProvider, DocumentProviders, DuplicateProvider};
pub use refresh::{DocumentCandidate, DocumentError, DocumentOutcome, MetadataDocuments};
pub use service::{
    ARTIST_DISPLAY_FIELDS, ARTIST_DISPLAY_PROFILE, METADATA_ADAPTER_VERSION, MetadataError,
    MetadataProfile, MetadataService, RefreshOutcome, RefreshPhase, RefreshReason, RefreshState,
    SubjectSource,
};
pub use storage::{MetadataStore, PostgresMetadataStore};

#[cfg(test)]
#[path = "providers_tests.rs"]
mod providers_tests;

#[cfg(test)]
#[path = "capabilities_tests.rs"]
mod capabilities_tests;

#[cfg(test)]
#[path = "freshness_tests.rs"]
mod freshness_tests;

#[cfg(test)]
#[path = "selection_tests.rs"]
mod selection_tests;

#[cfg(test)]
#[path = "service_tests.rs"]
mod service_tests;

#[cfg(all(test, feature = "postgres-integration"))]
#[path = "service_postgres_tests.rs"]
mod service_postgres_tests;
