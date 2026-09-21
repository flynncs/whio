use async_trait::async_trait;

use crate::tracks::{
    AlbumId, ArtworkRef, ProviderId, SourceFileFacts, SourceIdentity, SourceScope, TrackId,
    TrackRepositoryError,
};

use crate::tracks::SourceLookup;

use super::PgStore;
use super::decode::{decode_source, encode_source_scope, repository_error};

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
        let row = sqlx::query_as::<
            _,
            (
                Option<String>,
                Option<i64>,
                Option<chrono::DateTime<chrono::Utc>>,
                Option<chrono::DateTime<chrono::Utc>>,
                Option<String>,
                Option<String>,
                Option<String>,
            ),
        >(
            r#"SELECT path, size_bytes, modified_at, missing_at, content_type, suffix,
                      acoustid_fingerprint
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

        Ok(row.map(
            |(path, size_bytes, modified_at, missing_at, content_type, suffix, fingerprint)| {
                SourceFileFacts {
                    path,
                    size_bytes: size_bytes.unwrap_or_default().max(0) as u64,
                    modified_at,
                    missing_at,
                    content_type,
                    suffix,
                    acoustid_fingerprint: fingerprint,
                }
            },
        ))
    }

    async fn list_scanned_paths(
        &self,
        provider_id: &ProviderId,
        source_scope: &SourceScope,
    ) -> Result<Vec<(SourceIdentity, SourceFileFacts)>, TrackRepositoryError> {
        let rows = sqlx::query_as::<
            _,
            (
                String,
                String,
                String,
                Option<String>,
                Option<i64>,
                Option<chrono::DateTime<chrono::Utc>>,
                Option<chrono::DateTime<chrono::Utc>>,
                Option<String>,
                Option<String>,
                Option<String>,
            ),
        >(
            r#"SELECT provider_id, source_scope, external_id,
                      path, size_bytes, modified_at, missing_at, content_type, suffix,
                      acoustid_fingerprint
            FROM track_sources
            WHERE provider_id = $1 AND source_scope = $2 AND path IS NOT NULL
            "#,
        )
        .bind(provider_id.as_str())
        .bind(encode_source_scope(source_scope))
        .fetch_all(&self.pool)
        .await
        .map_err(repository_error)?;

        let mut scanned = Vec::with_capacity(rows.len());
        for (
            provider,
            scope,
            external_id,
            path,
            size_bytes,
            modified_at,
            missing_at,
            content_type,
            suffix,
            fingerprint,
        ) in rows
        {
            let source = decode_source(provider, scope, external_id)?;
            scanned.push((
                source,
                SourceFileFacts {
                    path,
                    size_bytes: size_bytes.unwrap_or_default().max(0) as u64,
                    modified_at,
                    missing_at,
                    content_type,
                    suffix,
                    acoustid_fingerprint: fingerprint,
                },
            ));
        }

        Ok(scanned)
    }
}
