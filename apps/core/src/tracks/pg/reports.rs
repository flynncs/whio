use async_trait::async_trait;
use uuid::Uuid;

use crate::tracks::{
    ArtistId, DuplicateCandidate, DuplicateSource, LOCAL_PROVIDER_ID, SplitArtist, SplitArtistRow,
    TrackId, TrackRepositoryError,
};

use crate::tracks::DuplicateReport;

use super::PgStore;
use super::decode::{decode_source, repository_error};

#[async_trait]
impl DuplicateReport for PgStore {
    async fn duplicate_candidates(
        &self,
        limit: u32,
    ) -> Result<Vec<DuplicateCandidate>, TrackRepositoryError> {
        let rows = sqlx::query_as::<
            _,
            (
                Uuid,
                String,
                Option<i64>,
                String,
                String,
                String,
                String,
                String,
                Option<i64>,
                Option<String>,
            ),
        >(
            r#"
            WITH duplicated AS (
                SELECT track_id
                FROM track_sources
                WHERE provider_id = $2
                GROUP BY track_id
                HAVING count(*) > 1
                ORDER BY track_id
                LIMIT $1
            )
            SELECT t.id, t.title, t.duration_ms,
                   coalesce((
                       SELECT string_agg(credits.name, '; ' ORDER BY credits.position)
                       FROM track_artist_credits AS credits
                       WHERE credits.track_id = t.id
                   ), ''),
                   s.provider_id, s.source_scope, s.external_id,
                   coalesce(s.path, ''), s.size_bytes, s.suffix
            FROM tracks AS t
            JOIN track_sources AS s ON s.track_id = t.id
            WHERE s.provider_id = $2
              AND t.id IN (SELECT track_id FROM duplicated)
            ORDER BY t.title_normalized, t.id, s.path
            "#,
        )
        .bind(i64::from(limit))
        .bind(LOCAL_PROVIDER_ID)
        .fetch_all(&self.pool)
        .await
        .map_err(repository_error)?;

        let mut candidates: Vec<DuplicateCandidate> = Vec::new();

        for row in rows {
            let track_id = TrackId::from_uuid(row.0);
            let source = DuplicateSource::new(
                decode_source(row.4, row.5, row.6)?,
                row.7,
                row.8.unwrap_or_default().max(0) as u64,
                row.9,
            );

            match candidates.last_mut() {
                Some(last) if last.track_id() == &track_id => last.push_source(source),
                _ => candidates.push(DuplicateCandidate::new(
                    track_id,
                    row.1,
                    row.2.and_then(|ms| u64::try_from(ms).ok()),
                    row.3
                        .split("; ")
                        .filter(|name| !name.is_empty())
                        .map(str::to_owned)
                        .collect(),
                    vec![source],
                )),
            }
        }

        Ok(candidates)
    }

    async fn split_artists(&self, limit: u32) -> Result<Vec<SplitArtist>, TrackRepositoryError> {
        let rows = sqlx::query_as::<_, (String, Uuid, String, bool, i64, i64)>(
            r#"
            WITH split AS (
                SELECT name_normalized
                FROM artists
                GROUP BY name_normalized
                HAVING count(*) > 1
                ORDER BY name_normalized
                LIMIT $1
            )
            SELECT ar.name_normalized, ar.id, ar.name,
                   ar.musicbrainz_artist_id IS NOT NULL,
                   (SELECT count(*) FROM track_artist_credits AS credits
                    WHERE credits.artist_id = ar.id),
                   (SELECT count(*) FROM albums AS releases
                    WHERE releases.album_artist_id = ar.id)
            FROM artists AS ar
            WHERE ar.name_normalized IN (SELECT name_normalized FROM split)
            ORDER BY ar.name_normalized, ar.musicbrainz_artist_id NULLS LAST
            "#,
        )
        .bind(i64::from(limit))
        .fetch_all(&self.pool)
        .await
        .map_err(repository_error)?;

        let mut artists: Vec<SplitArtist> = Vec::new();

        for (normalized, artist_id, name, identified, track_count, album_count) in rows {
            let row = SplitArtistRow::new(
                ArtistId::from_uuid(artist_id),
                name,
                identified,
                track_count.max(0) as u64,
                album_count.max(0) as u64,
            );

            match artists.last_mut() {
                Some(last) if last.name() == normalized => last.push_row(row),
                _ => artists.push(SplitArtist::new(normalized, vec![row])),
            }
        }

        Ok(artists)
    }
}
