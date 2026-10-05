use async_trait::async_trait;

use crate::features::music_library::{
    domain::{
        Album, AlbumId, Artist, ArtistId, ArtworkRef, ProviderEntityState, ProviderId,
        SourceIdentity, SourceScope, TrackId,
    },
    storage::contracts::{SourceFileFacts, TrackRepositoryError},
};

use crate::features::music_library::storage::contracts::SourceLookup;

use super::PgStore;
use super::decode::{
    ScannedPathRow, SourceFileFactsRow, decode_file_facts, decode_scanned_path, decode_source,
    encode_source_scope, repository_error,
};

#[async_trait]
impl SourceLookup for PgStore {
    async fn find_sources(
        &self,
        track_id: &TrackId,
    ) -> Result<Vec<SourceIdentity>, TrackRepositoryError> {
        let rows = sqlx::query_as::<_, (String, String, String)>(
            r#"SELECT provider_id, source_scope, external_id
            FROM track_sources
            WHERE track_id = $1
            ORDER BY created_at, id
            "#,
        )
        .bind(track_id.as_uuid())
        .fetch_all(&self.pool)
        .await
        .map_err(repository_error)?;

        rows.into_iter()
            .map(|(provider_id, source_scope, external_id)| {
                decode_source(provider_id, source_scope, external_id)
            })
            .collect()
    }

    async fn find_album_sources(
        &self,
        album_id: &AlbumId,
    ) -> Result<Vec<SourceIdentity>, TrackRepositoryError> {
        let rows = sqlx::query_as::<_, (String, String, String)>(
            r#"
            SELECT provider_id, source_scope, external_id
            FROM album_sources
            WHERE album_id = $1
            ORDER BY created_at, id
            "#,
        )
        .bind(album_id.as_uuid())
        .fetch_all(&self.pool)
        .await
        .map_err(repository_error)?;

        rows.into_iter()
            .map(|(provider_id, source_scope, external_id)| {
                decode_source(provider_id, source_scope, external_id)
            })
            .collect()
    }

    async fn find_artist_sources(
        &self,
        artist_id: &ArtistId,
    ) -> Result<Vec<SourceIdentity>, TrackRepositoryError> {
        let rows = sqlx::query_as::<_, (String, String, String)>(
            r#"
            SELECT provider_id, source_scope, external_id
            FROM artist_sources
            WHERE artist_id = $1
            ORDER BY created_at, id
            "#,
        )
        .bind(artist_id.as_uuid())
        .fetch_all(&self.pool)
        .await
        .map_err(repository_error)?;

        rows.into_iter()
            .map(|(provider_id, source_scope, external_id)| {
                decode_source(provider_id, source_scope, external_id)
            })
            .collect()
    }

    async fn find_artist_by_source(
        &self,
        source: &SourceIdentity,
    ) -> Result<Option<Artist>, TrackRepositoryError> {
        super::catalogue::find_artist_by_source(&self.pool, source).await
    }

    async fn find_album_by_source(
        &self,
        source: &SourceIdentity,
    ) -> Result<Option<Album>, TrackRepositoryError> {
        super::catalogue::find_album_by_source(&self.pool, source).await
    }

    async fn find_provider_entity_state(
        &self,
        source: &SourceIdentity,
    ) -> Result<Option<ProviderEntityState>, TrackRepositoryError> {
        let row =
            sqlx::query_as::<_, (chrono::DateTime<chrono::Utc>, Option<i32>, Option<String>)>(
                r#"
            SELECT fetched_at, reported_count, last_error
            FROM provider_entity_state
            WHERE provider_id = $1 AND source_scope = $2 AND external_id = $3
            "#,
            )
            .bind(source.provider_id().as_str())
            .bind(encode_source_scope(source.source_scope()))
            .bind(source.external_id())
            .fetch_optional(&self.pool)
            .await
            .map_err(repository_error)?;

        row.map(|(fetched_at, reported_count, last_error)| {
            let reported_count = reported_count
                .map(u32::try_from)
                .transpose()
                .map_err(|_| TrackRepositoryError::InvalidData)?;

            ProviderEntityState::new(fetched_at, reported_count, last_error)
                .map_err(|_| TrackRepositoryError::InvalidData)
        })
        .transpose()
    }

    async fn set_source_artwork(
        &self,
        source: &SourceIdentity,
        artwork: &ArtworkRef,
    ) -> Result<(), TrackRepositoryError> {
        sqlx::query(
            r#"UPDATE track_sources
            SET artwork_ref = $1
            WHERE provider_id = $2 AND source_scope = $3 AND external_id = $4
            "#,
        )
        .bind(artwork.reference())
        .bind(source.provider_id().as_str())
        .bind(encode_source_scope(source.source_scope()))
        .bind(source.external_id())
        .execute(&self.pool)
        .await
        .map(|_| ())
        .map_err(repository_error)
    }

    async fn find_artwork(
        &self,
        track_id: &TrackId,
    ) -> Result<Option<(SourceIdentity, ArtworkRef)>, TrackRepositoryError> {
        let rows = sqlx::query_as::<_, (String, String, String, String)>(
            r#"SELECT provider_id, source_scope, external_id, artwork_ref
            FROM track_sources
            WHERE track_id = $1
              AND artwork_ref IS NOT NULL
            ORDER BY created_at, id
            "#,
        )
        .bind(track_id.as_uuid())
        .fetch_all(&self.pool)
        .await
        .map_err(repository_error)?;

        let mut best: Option<(SourceIdentity, ArtworkRef)> = None;
        for (provider_id, source_scope, external_id, reference) in rows {
            let source = decode_source(provider_id, source_scope, external_id)?;
            let artwork = ArtworkRef::from_stored(reference)
                .map_err(|_| TrackRepositoryError::InvalidData)?;

            let outranks = best.as_ref().is_none_or(|(current, _)| {
                source.provider_id().precedence() < current.provider_id().precedence()
            });

            if outranks {
                best = Some((source, artwork));
            }
        }

        Ok(best)
    }

    async fn find_source_file_facts(
        &self,
        source: &SourceIdentity,
    ) -> Result<Option<SourceFileFacts>, TrackRepositoryError> {
        let row = sqlx::query_as::<_, SourceFileFactsRow>(
            r#"SELECT path, size_bytes, modified_at, missing_at, content_type, suffix,
                      container, codec, bitrate_kbps, sample_rate_hz, bit_depth, channels,
                      facts_version, acoustid_fingerprint
            FROM track_sources
            WHERE provider_id = $1 AND source_scope = $2 AND external_id = $3
            "#,
        )
        .bind(source.provider_id().as_str())
        .bind(encode_source_scope(source.source_scope()))
        .bind(source.external_id())
        .fetch_optional(&self.pool)
        .await
        .map_err(repository_error)?;

        row.map(decode_file_facts).transpose()
    }

    async fn list_scanned_paths(
        &self,
        provider_id: &ProviderId,
        source_scope: &SourceScope,
    ) -> Result<Vec<(SourceIdentity, SourceFileFacts)>, TrackRepositoryError> {
        let rows = sqlx::query_as::<_, ScannedPathRow>(
            r#"SELECT provider_id, source_scope, external_id,
                      path, size_bytes, modified_at, missing_at, content_type, suffix,
                      container, codec, bitrate_kbps, sample_rate_hz, bit_depth, channels,
                      facts_version, acoustid_fingerprint
            FROM track_sources
            WHERE provider_id = $1 AND source_scope = $2 AND path IS NOT NULL
            "#,
        )
        .bind(provider_id.as_str())
        .bind(encode_source_scope(source_scope))
        .fetch_all(&self.pool)
        .await
        .map_err(repository_error)?;

        rows.into_iter().map(decode_scanned_path).collect()
    }
}
