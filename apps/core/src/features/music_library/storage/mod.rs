mod construction;
pub use construction::{
    ArtworkCacheWiring, DeliveryWiring, LibraryWiring, MetadataWiring, MusicServices,
    W4Dependencies, construct,
};
pub(crate) mod contracts;
pub(crate) mod postgres;

pub(crate) fn maintenance(pool: sqlx::PgPool) -> super::maintenance::MaintenanceService {
    use contracts::{DuplicateReport, Retention};
    use std::sync::Arc;
    let store = Arc::new(postgres::PostgresTrackRepository::new(pool));
    super::maintenance::MaintenanceService::new(
        Arc::clone(&store) as Arc<dyn DuplicateReport>,
        store as Arc<dyn Retention>,
    )
}
