pub mod capabilities;
pub mod documents;
pub mod domain;
pub mod freshness;
pub mod licences;
pub mod refresh;
pub mod selection;
pub mod service;
pub mod storage;

pub use capabilities::{
    CapabilityError, CapabilitySnapshot, CapabilitySource, ResolverCapabilities, Snapshot,
};
pub use documents::{DocumentRouting, RoutingCapabilities};
pub use freshness::{Jitter, NoJitter};
pub use refresh::{DocumentCandidate, DocumentError, DocumentOutcome, MetadataDocuments};
pub use service::{
    ARTIST_DISPLAY_FIELDS, ARTIST_DISPLAY_PROFILE, METADATA_ADAPTER_VERSION, MetadataError,
    MetadataProfile, MetadataService, RefreshOutcome, RefreshPhase, RefreshReason, RefreshState,
    SubjectSource,
};
pub use storage::{MetadataStore, PostgresMetadataStore};

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
