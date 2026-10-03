use async_trait::async_trait;
use sqlx::PgConnection;
use uuid::Uuid;

use crate::features::music_library::{
    domain::{ArtistId, EnrichmentSubject},
    storage::contracts::{EnrichmentGuard, TrackRepositoryError},
};

use super::PgStore;
use super::decode::{decode_source, repository_error};

#[async_trait]
impl EnrichmentGuard for PgStore {
    async fn enrichment_subject(
        &self,
        tx: &mut PgConnection,
        artist_id: &ArtistId,
    ) -> Result<Option<EnrichmentSubject>, TrackRepositoryError> {
        let lock_clause = " FOR SHARE";

        let row = sqlx::query_as::<_, (Uuid, String, Option<String>)>(&format!(
            r#"
            SELECT id, name, musicbrainz_artist_id
            FROM artists
            WHERE id = $1{lock_clause}
            "#
        ))
        .bind(artist_id.as_uuid())
        .fetch_optional(&mut *tx)
        .await
        .map_err(repository_error)?;

        let Some((id, name, musicbrainz_artist_id)) = row else {
            return Ok(None);
        };

        let identities = sqlx::query_as::<_, (String, String, String)>(
            r#"
            SELECT provider_id, source_scope, external_id
            FROM artist_sources
            WHERE artist_id = $1
            ORDER BY provider_id, source_scope, external_id
            "#,
        )
        .bind(id)
        .fetch_all(&mut *tx)
        .await
        .map_err(repository_error)?
        .into_iter()
        .map(|(provider_id, source_scope, external_id)| {
            decode_source(provider_id, source_scope, external_id)
        })
        .collect::<Result<Vec<_>, _>>()?;

        Ok(Some(EnrichmentSubject::new(
            ArtistId::from_uuid(id),
            name,
            musicbrainz_artist_id,
            identities,
        )))
    }
}
