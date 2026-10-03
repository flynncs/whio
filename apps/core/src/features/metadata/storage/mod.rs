pub mod contracts;
pub mod postgres;

pub use contracts::{CommitOutcome, MetadataRepositoryError, MetadataStore};
pub use postgres::PostgresMetadataStore;

#[cfg(all(test, feature = "postgres-integration"))]
#[path = "tests.rs"]
mod tests;
