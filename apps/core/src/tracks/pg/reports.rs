use async_trait::async_trait;
use uuid::Uuid;

use crate::tracks::{
    ArtistId, DuplicateCandidate, DuplicateSource, EvictionReport, LOCAL_PROVIDER_ID,
    RetentionReport, SplitArtist, SplitArtistRow, TrackId, TrackRepositoryError,
};

use crate::tracks::{DuplicateReport, Retention};

use super::PgStore;
use super::decode::{decode_source, repository_error};

fn track_engaged(id: &str) -> String {
    format!(
        "(EXISTS (SELECT 1 FROM track_sources AS ts WHERE ts.track_id = {id} AND ts.provider_id = ANY($1))
          OR EXISTS (SELECT 1 FROM track_favorites AS tf WHERE tf.track_id = {id})
          OR EXISTS (SELECT 1 FROM track_ratings AS tr WHERE tr.track_id = {id})
          OR EXISTS (SELECT 1 FROM track_play_stats AS tp WHERE tp.track_id = {id} AND tp.play_count > 0)
          OR EXISTS (SELECT 1 FROM playlist_entries AS pe WHERE pe.track_id = {id}))"
    )
}

fn track_engaged_without_plays(id: &str) -> String {
    format!(
        "(EXISTS (SELECT 1 FROM track_sources AS ts WHERE ts.track_id = {id} AND ts.provider_id = ANY($1))
          OR EXISTS (SELECT 1 FROM track_favorites AS tf WHERE tf.track_id = {id})
          OR EXISTS (SELECT 1 FROM track_ratings AS tr WHERE tr.track_id = {id})
          OR EXISTS (SELECT 1 FROM playlist_entries AS pe WHERE pe.track_id = {id}))"
    )
}

fn album_unengaged(member_engaged: &str) -> String {
    format!(
        "(NOT EXISTS (SELECT 1 FROM album_tracks AS amt WHERE amt.album_id = a.id AND {member_engaged})
          AND NOT EXISTS (SELECT 1 FROM album_favorites AS af WHERE af.album_id = a.id)
          AND NOT EXISTS (SELECT 1 FROM album_ratings AS arv WHERE arv.album_id = a.id))"
    )
}

fn artist_unengaged(member_engaged: &str) -> String {
    format!(
        "(NOT EXISTS (SELECT 1 FROM track_artist_credits AS ac WHERE ac.artist_id = ar.id AND {member_engaged})
          AND NOT EXISTS (SELECT 1 FROM artist_favorites AS af WHERE af.artist_id = ar.id)
          AND NOT EXISTS (SELECT 1 FROM artist_ratings AS arv WHERE arv.artist_id = ar.id))"
    )
}

#[async_trait]
impl Retention for PgStore {
    async fn retention_report(&self) -> Result<RetentionReport, TrackRepositoryError> {
        let engaged = track_engaged("t.id");
        let engaged_without_plays = track_engaged_without_plays("t.id");
        let album_evictable = album_unengaged(&track_engaged("amt.track_id"));
        let album_play_held = album_unengaged(&track_engaged_without_plays("amt.track_id"));
        let artist_evictable = artist_unengaged(&track_engaged("ac.track_id"));
        let artist_play_held = artist_unengaged(&track_engaged_without_plays("ac.track_id"));

        let query = format!(
            r#"
            SELECT
                (SELECT count(*) FROM tracks AS t WHERE NOT {engaged})::BIGINT,
                (SELECT count(*) FROM albums AS a WHERE {album_evictable})::BIGINT,
                (SELECT count(*) FROM artists AS ar WHERE {artist_evictable})::BIGINT,
                (SELECT count(*) FROM tracks AS t
                 WHERE NOT {engaged_without_plays}
                   AND EXISTS (SELECT 1 FROM track_play_stats AS tp
                               WHERE tp.track_id = t.id AND tp.play_count > 0))::BIGINT,
                (SELECT count(*) FROM albums AS a
                 WHERE {album_play_held}
                   AND EXISTS (SELECT 1 FROM album_tracks AS amt
                               JOIN track_play_stats AS tp
                                 ON tp.track_id = amt.track_id AND tp.play_count > 0
                               WHERE amt.album_id = a.id))::BIGINT,
                (SELECT count(*) FROM artists AS ar
                 WHERE {artist_play_held}
                   AND EXISTS (SELECT 1 FROM track_artist_credits AS ac
                               JOIN track_play_stats AS tp
                                 ON tp.track_id = ac.track_id AND tp.play_count > 0
                               WHERE ac.artist_id = ar.id))::BIGINT,
                (SELECT min(tp.last_played_at) FROM track_play_stats AS tp
                 JOIN tracks AS t ON t.id = tp.track_id
                 WHERE tp.play_count > 0 AND NOT {engaged_without_plays}),
                (SELECT max(tp.last_played_at) FROM track_play_stats AS tp
                 JOIN tracks AS t ON t.id = tp.track_id
                 WHERE tp.play_count > 0 AND NOT {engaged_without_plays})
            "#
        );

        let row = sqlx::query_as::<
            _,
            (
                i64,
                i64,
                i64,
                i64,
                i64,
                i64,
                Option<chrono::DateTime<chrono::Utc>>,
                Option<chrono::DateTime<chrono::Utc>>,
            ),
        >(&query)
        .bind(self.library_provider_ids())
        .fetch_one(&self.pool)
        .await
        .map_err(repository_error)?;

        Ok(RetentionReport {
            evictable_tracks: row.0.max(0) as u64,
            evictable_albums: row.1.max(0) as u64,
            evictable_artists: row.2.max(0) as u64,
            play_held_tracks: row.3.max(0) as u64,
            play_held_albums: row.4.max(0) as u64,
            play_held_artists: row.5.max(0) as u64,
            oldest_played_at: row.6,
            newest_played_at: row.7,
        })
    }

    async fn evict_unengaged(&self) -> Result<EvictionReport, TrackRepositoryError> {
        let library_ids = self.library_provider_ids();
        let mut transaction = self.pool.begin().await.map_err(repository_error)?;

        let tracks = sqlx::query(&format!(
            "DELETE FROM tracks AS t WHERE NOT {}",
            track_engaged("t.id")
        ))
        .bind(&library_ids)
        .execute(&mut *transaction)
        .await
        .map_err(repository_error)?
        .rows_affected();

        let albums = sqlx::query(&format!(
            "DELETE FROM albums AS a WHERE {}",
            album_unengaged(&track_engaged("amt.track_id"))
        ))
        .bind(&library_ids)
        .execute(&mut *transaction)
        .await
        .map_err(repository_error)?
        .rows_affected();

        let artists = sqlx::query(&format!(
            "DELETE FROM artists AS ar WHERE {}",
            artist_unengaged(&track_engaged("ac.track_id"))
        ))
        .bind(&library_ids)
        .execute(&mut *transaction)
        .await
        .map_err(repository_error)?
        .rows_affected();

        transaction.commit().await.map_err(repository_error)?;

        Ok(EvictionReport {
            tracks,
            albums,
            artists,
        })
    }
}

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
