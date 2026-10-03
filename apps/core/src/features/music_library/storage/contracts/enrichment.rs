use async_trait::async_trait;

use crate::features::music_library::domain::{ArtistId, EnrichmentSubject};

use super::TrackRepositoryError;

#[async_trait]
pub trait EnrichmentGuard: Send + Sync {
    async fn enrichment_subject(
        &self,
        tx: &mut sqlx::PgConnection,
        artist_id: &ArtistId,
    ) -> Result<Option<EnrichmentSubject>, TrackRepositoryError>;
}
