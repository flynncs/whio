use std::collections::HashMap;

use async_trait::async_trait;
use sqlx::{PgConnection, PgPool};
use uuid::Uuid;

use crate::tracks::{
    Album, AlbumEntry, AlbumId, AlbumListFilter, AlbumMetadata, AlbumOrder, AlbumSummary,
    AlbumTrack, Artist, ArtistCredit, ArtistId, ArtistSummary, ArtworkRef, GenreSummary,
    LOCAL_PROVIDER_ID, LibraryState, ProviderId, SourceFileFacts, SourceIdentity, SourceScope,
    Starred, StateTarget, Track, TrackHit, TrackId, TrackMetadata, TrackRepository,
    TrackRepositoryError,
};

use super::domain::normalize_for_matching;

pub struct PostgresTrackRepository {
    pool: PgPool,
}

impl PostgresTrackRepository {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl TrackRepository for PostgresTrackRepository {
    async fn get_or_create(
        &self,
        source: SourceIdentity,
        metadata: TrackMetadata,
    ) -> Result<Track, TrackRepositoryError> {
        let credits = resolve_credits(&self.pool, metadata.artists()).await?;

        let mut tx = self.pool.begin().await.map_err(repository_error)?;
        let track_id = get_or_create_in(&mut tx, &source, &metadata, &credits).await?;
        tx.commit().await.map_err(repository_error)?;

        Ok(Track::new(track_id, metadata))
    }

    async fn index_source(
        &self,
        source: SourceIdentity,
        metadata: TrackMetadata,
        facts: &SourceFileFacts,
    ) -> Result<Track, TrackRepositoryError> {
        let credits = resolve_credits(&self.pool, metadata.artists()).await?;

        let mut tx = self.pool.begin().await.map_err(repository_error)?;
        let track_id = get_or_create_in(&mut tx, &source, &metadata, &credits).await?;
        set_source_file_facts_in(&mut tx, &source, facts).await?;
        tx.commit().await.map_err(repository_error)?;

        Ok(Track::new(track_id, metadata))
    }

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

    async fn find_track(&self, track_id: &TrackId) -> Result<Option<Track>, TrackRepositoryError> {
        let row: Option<(
            String,
            Option<i64>,
            Option<String>,
            Option<String>,
            Option<String>,
        )> = sqlx::query_as(
            r#"SELECT title, duration_ms, musicbrainz_recording_id, isrc, genre
            FROM tracks
            WHERE id = $1
            "#,
        )
        .bind(track_id.as_uuid())
        .fetch_optional(&self.pool)
        .await
        .map_err(repository_error)?;

        let Some((title, duration_ms, recording_id, isrc, genre)) = row else {
            return Ok(None);
        };

        let credits: Vec<(String, Option<String>)> = sqlx::query_as(
            r#"SELECT credits.name, artists.musicbrainz_artist_id
            FROM track_artist_credits AS credits
            LEFT JOIN artists ON artists.id = credits.artist_id
            WHERE credits.track_id = $1
            ORDER BY credits.position
            "#,
        )
        .bind(track_id.as_uuid())
        .fetch_all(&self.pool)
        .await
        .map_err(repository_error)?;

        let artists = credits
            .into_iter()
            .map(|(name, identifier)| ArtistCredit::new(name, identifier))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| TrackRepositoryError::InvalidData)?;

        Ok(Some(decode_track(
            *track_id.as_uuid(),
            title,
            duration_ms,
            recording_id,
            isrc,
            genre,
            artists,
        )?))
    }

    async fn find_track_state(
        &self,
        track_id: &TrackId,
        user: Uuid,
    ) -> Result<LibraryState, TrackRepositoryError> {
        let states = track_states(
            &self.pool,
            Some(user),
            std::slice::from_ref(track_id.as_uuid()),
        )
        .await?;

        Ok(states.get(track_id.as_uuid()).copied().unwrap_or_default())
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

    async fn set_source_file_facts(
        &self,
        source: &SourceIdentity,
        facts: &SourceFileFacts,
    ) -> Result<(), TrackRepositoryError> {
        let mut connection = self.pool.acquire().await.map_err(repository_error)?;

        set_source_file_facts_in(&mut connection, source, facts).await
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

    async fn resolve_or_create_artist(
        &self,
        credit: &ArtistCredit,
    ) -> Result<Artist, TrackRepositoryError> {
        resolve_artist(&self.pool, credit).await
    }

    async fn resolve_or_create_album(
        &self,
        metadata: &AlbumMetadata,
    ) -> Result<Album, TrackRepositoryError> {
        resolve_album(&self.pool, metadata).await
    }

    async fn set_album_source(
        &self,
        album_id: &AlbumId,
        source: &SourceIdentity,
    ) -> Result<(), TrackRepositoryError> {
        sqlx::query(
            r#"
            INSERT INTO album_sources (id, album_id, provider_id, source_scope, external_id)
            VALUES ($1, $2, $3, $4, $5)
            ON CONFLICT (provider_id, source_scope, external_id) DO NOTHING
            "#,
        )
        .bind(Uuid::now_v7())
        .bind(album_id.as_uuid())
        .bind(source.provider_id().as_str())
        .bind(encode_source_scope(source.source_scope()))
        .bind(source.external_id())
        .execute(&self.pool)
        .await
        .map(|_| ())
        .map_err(repository_error)
    }

    async fn set_album_track(
        &self,
        album_id: &AlbumId,
        track_id: &TrackId,
        membership: &AlbumTrack,
    ) -> Result<(), TrackRepositoryError> {
        let mut tx = self.pool.begin().await.map_err(repository_error)?;

        sqlx::query("DELETE FROM album_tracks WHERE track_id = $1 AND album_id <> $2")
            .bind(track_id.as_uuid())
            .bind(album_id.as_uuid())
            .execute(&mut *tx)
            .await
            .map_err(repository_error)?;

        sqlx::query(
            r#"
            INSERT INTO album_tracks (album_id, track_id, disc_number, track_number, position)
            VALUES ($1, $2, $3, $4, $5)
            ON CONFLICT (album_id, track_id) DO UPDATE
            SET disc_number = EXCLUDED.disc_number,
                track_number = EXCLUDED.track_number,
                position = EXCLUDED.position
            "#,
        )
        .bind(album_id.as_uuid())
        .bind(track_id.as_uuid())
        .bind(encode_smallint(membership.disc_number())?)
        .bind(encode_smallint(membership.track_number())?)
        .bind(i32::try_from(membership.position()).map_err(|_| TrackRepositoryError::InvalidData)?)
        .execute(&mut *tx)
        .await
        .map_err(repository_error)?;

        tx.commit().await.map_err(repository_error)?;

        Ok(())
    }

    async fn clear_album_track(&self, track_id: &TrackId) -> Result<(), TrackRepositoryError> {
        sqlx::query("DELETE FROM album_tracks WHERE track_id = $1")
            .bind(track_id.as_uuid())
            .execute(&self.pool)
            .await
            .map(|_| ())
            .map_err(repository_error)
    }

    async fn find_album(&self, album_id: &AlbumId) -> Result<Option<Album>, TrackRepositoryError> {
        let row = sqlx::query_as::<_, AlbumRow>(&format!(
            "SELECT {ALBUM_COLUMNS} FROM albums WHERE id = $1"
        ))
        .bind(album_id.as_uuid())
        .fetch_optional(&self.pool)
        .await
        .map_err(repository_error)?;

        row.map(decode_album).transpose()
    }

    async fn find_album_for_track(
        &self,
        track_id: &TrackId,
    ) -> Result<Option<Album>, TrackRepositoryError> {
        let row = sqlx::query_as::<_, AlbumRow>(&format!(
            r#"
            SELECT {ALBUM_COLUMNS}
            FROM albums AS a
            JOIN album_tracks AS mt ON mt.album_id = a.id
            WHERE mt.track_id = $1
            LIMIT 1
            "#
        ))
        .bind(track_id.as_uuid())
        .fetch_optional(&self.pool)
        .await
        .map_err(repository_error)?;

        row.map(decode_album).transpose()
    }

    async fn find_album_summary(
        &self,
        album_id: &AlbumId,
        user: Option<Uuid>,
    ) -> Result<Option<AlbumSummary>, TrackRepositoryError> {
        let row = sqlx::query_as::<_, AlbumBrowseRow>(&format!(
            r#"
            SELECT {ALBUM_BROWSE_COLUMNS}
            {ALBUM_BROWSE_FROM}
            WHERE a.id = $1
            GROUP BY a.id, ar.id
            "#
        ))
        .bind(album_id.as_uuid())
        .fetch_optional(&self.pool)
        .await
        .map_err(repository_error)?;

        let Some(summary) = row.map(decode_album_summary).transpose()? else {
            return Ok(None);
        };
        let states =
            album_states(&self.pool, user, std::slice::from_ref(album_id.as_uuid())).await?;

        Ok(Some(attach_album_state(summary, &states)))
    }

    async fn find_artist(
        &self,
        artist_id: &ArtistId,
        _user: Option<Uuid>,
    ) -> Result<Option<Artist>, TrackRepositoryError> {
        let row = sqlx::query_as::<_, ArtistRow>(&format!(
            "SELECT {ARTIST_COLUMNS} FROM artists WHERE id = $1"
        ))
        .bind(artist_id.as_uuid())
        .fetch_optional(&self.pool)
        .await
        .map_err(repository_error)?;

        row.map(decode_artist).transpose()
    }

    async fn find_artist_state(
        &self,
        artist_id: &ArtistId,
        user: Uuid,
    ) -> Result<LibraryState, TrackRepositoryError> {
        let states = artist_states(
            &self.pool,
            Some(user),
            std::slice::from_ref(artist_id.as_uuid()),
        )
        .await?;

        Ok(states.get(artist_id.as_uuid()).copied().unwrap_or_default())
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

    async fn find_album_entries(
        &self,
        album_id: &AlbumId,
        user: Option<Uuid>,
    ) -> Result<Vec<AlbumEntry>, TrackRepositoryError> {
        let rows = sqlx::query_as::<
            _,
            (
                Uuid,
                String,
                Option<i64>,
                Option<String>,
                Option<String>,
                Option<String>,
                Option<i16>,
                Option<i16>,
                i32,
            ),
        >(
            r#"
            SELECT t.id, t.title, t.duration_ms, t.musicbrainz_recording_id, t.isrc, t.genre,
                   mt.disc_number, mt.track_number, mt.position
            FROM album_tracks AS mt
            JOIN tracks AS t ON t.id = mt.track_id
            WHERE mt.album_id = $1
            ORDER BY COALESCE(mt.disc_number, 1), mt.track_number NULLS LAST,
                     mt.position, mt.track_id
            "#,
        )
        .bind(album_id.as_uuid())
        .fetch_all(&self.pool)
        .await
        .map_err(repository_error)?;

        let track_ids = rows.iter().map(|row| row.0).collect::<Vec<_>>();
        let mut credits = credits_for_tracks(&self.pool, &track_ids).await?;
        let states = track_states(&self.pool, user, &track_ids).await?;
        let mut entries = Vec::with_capacity(rows.len());

        for (
            id,
            title,
            duration_ms,
            recording_id,
            isrc,
            genre,
            disc_number,
            track_number,
            position,
        ) in rows
        {
            let track = decode_track(
                id,
                title,
                duration_ms,
                recording_id,
                isrc,
                genre,
                credits.remove(&id).unwrap_or_default(),
            )?;
            let membership = decode_membership(disc_number, track_number, position)?;
            let state = states.get(&id).copied().unwrap_or_default();

            entries.push(AlbumEntry::new(track, membership).with_state(state));
        }

        Ok(entries)
    }

    async fn find_albums_for_artist(
        &self,
        artist_id: &ArtistId,
        user: Option<Uuid>,
    ) -> Result<Vec<AlbumSummary>, TrackRepositoryError> {
        let rows = sqlx::query_as::<_, AlbumBrowseRow>(&format!(
            r#"
            SELECT {ALBUM_BROWSE_COLUMNS}
            {ALBUM_BROWSE_FROM}
            WHERE a.album_artist_id = $1
            GROUP BY a.id, ar.id
            ORDER BY a.title_normalized, a.id
            "#
        ))
        .bind(artist_id.as_uuid())
        .fetch_all(&self.pool)
        .await
        .map_err(repository_error)?;

        let mut summaries = rows
            .into_iter()
            .map(decode_album_summary)
            .collect::<Result<Vec<_>, _>>()?;
        attach_album_states(&self.pool, user, &mut summaries).await?;

        Ok(summaries)
    }

    async fn list_albums(
        &self,
        offset: u32,
        limit: u32,
        order: AlbumOrder,
        filter: AlbumListFilter,
        user: Option<Uuid>,
    ) -> Result<Vec<AlbumSummary>, TrackRepositoryError> {
        if matches!(
            order,
            AlbumOrder::Frequent
                | AlbumOrder::Recent
                | AlbumOrder::HighestRated
                | AlbumOrder::Starred
        ) {
            let Some(user) = user else {
                return Ok(Vec::new());
            };
            let ids = ordered_album_ids(&self.pool, order, user, offset, limit).await?;

            return album_summaries_by_ids(&self.pool, &ids, Some(user)).await;
        }

        let rows = sqlx::query_as::<_, AlbumBrowseRow>(&format!(
            r#"
            SELECT {ALBUM_BROWSE_COLUMNS}
            {ALBUM_BROWSE_FROM}
            WHERE (NOT $3 OR EXISTS (
                      SELECT 1 FROM album_favorites
                      WHERE album_favorites.album_id = a.id AND album_favorites.user_id = $7
                  ))
              AND ($4::TEXT IS NULL OR EXISTS (
                      SELECT 1 FROM album_tracks AS genre_tracks
                      JOIN tracks AS genre_track ON genre_track.id = genre_tracks.track_id
                      WHERE genre_tracks.album_id = a.id
                        AND lower(genre_track.genre) = lower($4)
                  ))
              AND ($5::SMALLINT IS NULL OR a.year >= $5)
              AND ($6::SMALLINT IS NULL OR a.year <= $6)
            GROUP BY a.id, ar.id
            ORDER BY {}, a.id
            LIMIT $1 OFFSET $2
            "#,
            album_order_clause(order)
        ))
        .bind(i64::from(limit))
        .bind(i64::from(offset))
        .bind(filter.is_starred())
        .bind(filter.genre())
        .bind(filter.year_from().and_then(|year| i16::try_from(year).ok()))
        .bind(filter.year_to().and_then(|year| i16::try_from(year).ok()))
        .bind(user)
        .fetch_all(&self.pool)
        .await
        .map_err(repository_error)?;

        let mut summaries = rows
            .into_iter()
            .map(decode_album_summary)
            .collect::<Result<Vec<_>, _>>()?;
        attach_album_states(&self.pool, user, &mut summaries).await?;

        Ok(summaries)
    }

    async fn list_artists(
        &self,
        offset: u32,
        limit: u32,
        user: Option<Uuid>,
    ) -> Result<Vec<ArtistSummary>, TrackRepositoryError> {
        let rows = sqlx::query_as::<_, (Uuid, String, Option<String>, Option<String>, i64)>(
            r#"
            SELECT ar.id, ar.name, ar.sort_name, ar.musicbrainz_artist_id,
                   (SELECT COUNT(*) FROM albums WHERE album_artist_id = ar.id)::BIGINT
            FROM artists AS ar
            ORDER BY ar.name_normalized, ar.id
            LIMIT $1 OFFSET $2
            "#,
        )
        .bind(i64::from(limit))
        .bind(i64::from(offset))
        .fetch_all(&self.pool)
        .await
        .map_err(repository_error)?;

        let mut summaries = rows
            .into_iter()
            .map(decode_artist_summary)
            .collect::<Result<Vec<_>, _>>()?;
        attach_artist_states(&self.pool, user, &mut summaries).await?;

        Ok(summaries)
    }

    async fn count_albums(&self) -> Result<u32, TrackRepositoryError> {
        let count = sqlx::query_scalar::<_, i64>("SELECT COUNT(*)::BIGINT FROM albums")
            .fetch_one(&self.pool)
            .await
            .map_err(repository_error)?;

        u32::try_from(count).map_err(|_| TrackRepositoryError::InvalidData)
    }

    async fn count_artists(&self) -> Result<u32, TrackRepositoryError> {
        let count = sqlx::query_scalar::<_, i64>("SELECT COUNT(*)::BIGINT FROM artists")
            .fetch_one(&self.pool)
            .await
            .map_err(repository_error)?;

        u32::try_from(count).map_err(|_| TrackRepositoryError::InvalidData)
    }

    async fn search_tracks(
        &self,
        query: &str,
        limit: u32,
        user: Option<Uuid>,
    ) -> Result<Vec<TrackHit>, TrackRepositoryError> {
        let needle = normalize_for_matching(query);
        let rows = sqlx::query_as::<_, TrackSearchRow>(&format!(
            r#"
            SELECT t.id, t.title, t.duration_ms, t.musicbrainz_recording_id, t.isrc, t.genre,
                   {ALBUM_SEARCH_COLUMNS}
            FROM tracks AS t
            LEFT JOIN album_tracks AS mt ON mt.track_id = t.id
            LEFT JOIN albums AS a ON a.id = mt.album_id
            WHERE strpos(t.title_normalized, $1) > 0
               OR EXISTS (
                   SELECT 1
                   FROM track_artist_credits AS credits
                   WHERE credits.track_id = t.id
                     AND strpos(
                         translate(
                             lower(regexp_replace(normalize(credits.name, NFC), '\s+', ' ', 'g')),
                             U&'\2019\2018\02BC\2013\2014\2011',
                             U&'\0027\0027\0027\002D\002D\002D'
                         ),
                         $1
                     ) > 0
               )
            ORDER BY EXISTS (
                         SELECT 1
                         FROM track_sources AS sources
                         WHERE sources.track_id = t.id AND sources.provider_id = $2
                     ) DESC,
                     t.title_normalized, t.id
            LIMIT $3
            "#
        ))
        .bind(&needle)
        .bind(LOCAL_PROVIDER_ID)
        .bind(i64::from(limit))
        .fetch_all(&self.pool)
        .await
        .map_err(repository_error)?;

        hits_from_rows(&self.pool, rows, user).await
    }

    async fn search_albums(
        &self,
        query: &str,
        limit: u32,
        user: Option<Uuid>,
    ) -> Result<Vec<AlbumSummary>, TrackRepositoryError> {
        let needle = normalize_for_matching(query);
        let rows = sqlx::query_as::<_, AlbumBrowseRow>(&format!(
            r#"
            SELECT {ALBUM_BROWSE_COLUMNS}
            {ALBUM_BROWSE_FROM}
            WHERE strpos(a.title_normalized, $1) > 0
               OR EXISTS (
                   SELECT 1
                   FROM artists AS album_artists
                   WHERE album_artists.id = a.album_artist_id
                     AND strpos(album_artists.name_normalized, $1) > 0
               )
            GROUP BY a.id, ar.id
            ORDER BY a.title_normalized, a.id
            LIMIT $2
            "#
        ))
        .bind(&needle)
        .bind(i64::from(limit))
        .fetch_all(&self.pool)
        .await
        .map_err(repository_error)?;

        let mut summaries = rows
            .into_iter()
            .map(decode_album_summary)
            .collect::<Result<Vec<_>, _>>()?;
        attach_album_states(&self.pool, user, &mut summaries).await?;

        Ok(summaries)
    }

    async fn search_artists(
        &self,
        query: &str,
        limit: u32,
        user: Option<Uuid>,
    ) -> Result<Vec<ArtistSummary>, TrackRepositoryError> {
        let needle = normalize_for_matching(query);
        let rows = sqlx::query_as::<_, (Uuid, String, Option<String>, Option<String>, i64)>(
            r#"
            SELECT ar.id, ar.name, ar.sort_name, ar.musicbrainz_artist_id,
                   (SELECT COUNT(*) FROM albums WHERE album_artist_id = ar.id)::BIGINT
            FROM artists AS ar
            WHERE strpos(ar.name_normalized, $1) > 0
            ORDER BY ar.name_normalized, ar.id
            LIMIT $2
            "#,
        )
        .bind(&needle)
        .bind(i64::from(limit))
        .fetch_all(&self.pool)
        .await
        .map_err(repository_error)?;

        let mut summaries = rows
            .into_iter()
            .map(decode_artist_summary)
            .collect::<Result<Vec<_>, _>>()?;
        attach_artist_states(&self.pool, user, &mut summaries).await?;

        Ok(summaries)
    }

    async fn list_genres(&self) -> Result<Vec<GenreSummary>, TrackRepositoryError> {
        let rows = sqlx::query_as::<_, (String, i64, i64)>(
            r#"
            SELECT MIN(t.genre), COUNT(*)::BIGINT, COUNT(DISTINCT mt.album_id)::BIGINT
            FROM tracks AS t
            LEFT JOIN album_tracks AS mt ON mt.track_id = t.id
            WHERE t.genre IS NOT NULL AND btrim(t.genre) <> ''
            GROUP BY lower(t.genre)
            ORDER BY lower(t.genre), MIN(t.genre)
            "#,
        )
        .fetch_all(&self.pool)
        .await
        .map_err(repository_error)?;

        rows.into_iter()
            .map(|(genre, songs, albums)| {
                GenreSummary::new(
                    genre,
                    u32::try_from(songs).map_err(|_| TrackRepositoryError::InvalidData)?,
                    u32::try_from(albums).map_err(|_| TrackRepositoryError::InvalidData)?,
                )
                .map_err(|_| TrackRepositoryError::InvalidData)
            })
            .collect()
    }

    async fn starred(&self, user: Uuid) -> Result<Starred, TrackRepositoryError> {
        let track_ids = sqlx::query_scalar::<_, Uuid>(
            "SELECT track_id FROM track_favorites WHERE user_id = $1 ORDER BY starred_at DESC, track_id",
        )
        .bind(user)
        .fetch_all(&self.pool)
        .await
        .map_err(repository_error)?;
        let album_ids = sqlx::query_scalar::<_, Uuid>(
            "SELECT album_id FROM album_favorites WHERE user_id = $1 ORDER BY starred_at DESC, album_id",
        )
        .bind(user)
        .fetch_all(&self.pool)
        .await
        .map_err(repository_error)?;
        let artist_ids = sqlx::query_scalar::<_, Uuid>(
            "SELECT artist_id FROM artist_favorites WHERE user_id = $1 ORDER BY starred_at DESC, artist_id",
        )
        .bind(user)
        .fetch_all(&self.pool)
        .await
        .map_err(repository_error)?;

        Ok(Starred::new(
            tracks_by_ids(&self.pool, &track_ids, user).await?,
            album_summaries_by_ids(&self.pool, &album_ids, Some(user)).await?,
            artist_summaries_by_ids(&self.pool, &artist_ids, Some(user)).await?,
        ))
    }

    async fn set_starred(
        &self,
        user: Uuid,
        target: StateTarget<'_>,
        starred: bool,
    ) -> Result<(), TrackRepositoryError> {
        match target {
            StateTarget::Track(track_id) => {
                set_starred_row(
                    &self.pool,
                    "track_favorites",
                    "track_id",
                    user,
                    track_id.as_uuid(),
                    starred,
                )
                .await
            }
            StateTarget::Album(album_id) => {
                set_starred_row(
                    &self.pool,
                    "album_favorites",
                    "album_id",
                    user,
                    album_id.as_uuid(),
                    starred,
                )
                .await
            }
            StateTarget::Artist(artist_id) => {
                set_starred_row(
                    &self.pool,
                    "artist_favorites",
                    "artist_id",
                    user,
                    artist_id.as_uuid(),
                    starred,
                )
                .await
            }
        }
    }

    async fn set_rating(
        &self,
        user: Uuid,
        target: StateTarget<'_>,
        rating: Option<u8>,
    ) -> Result<(), TrackRepositoryError> {
        let rating =
            crate::tracks::checked_rating(rating).map_err(|_| TrackRepositoryError::InvalidData)?;
        let rating = rating.map(i16::from);

        match target {
            StateTarget::Track(track_id) => {
                set_rating_row(
                    &self.pool,
                    "track_ratings",
                    "track_id",
                    user,
                    track_id.as_uuid(),
                    rating,
                )
                .await
            }
            StateTarget::Album(album_id) => {
                set_rating_row(
                    &self.pool,
                    "album_ratings",
                    "album_id",
                    user,
                    album_id.as_uuid(),
                    rating,
                )
                .await
            }
            StateTarget::Artist(artist_id) => {
                set_rating_row(
                    &self.pool,
                    "artist_ratings",
                    "artist_id",
                    user,
                    artist_id.as_uuid(),
                    rating,
                )
                .await
            }
        }
    }

    async fn record_play(
        &self,
        user: Uuid,
        track_id: &TrackId,
        played_at: chrono::DateTime<chrono::Utc>,
    ) -> Result<(), TrackRepositoryError> {
        sqlx::query(
            r#"
            INSERT INTO track_play_stats (user_id, track_id, play_count, last_played_at)
            VALUES ($1, $2, 1, $3)
            ON CONFLICT (user_id, track_id) DO UPDATE
            SET play_count = track_play_stats.play_count + 1,
                last_played_at = EXCLUDED.last_played_at
            "#,
        )
        .bind(user)
        .bind(track_id.as_uuid())
        .bind(played_at)
        .execute(&self.pool)
        .await
        .map(|_| ())
        .map_err(repository_error)
    }
}

type ArtistRow = (Uuid, String, Option<String>, Option<String>);
type AlbumRow = (
    Uuid,
    String,
    Option<Uuid>,
    Option<i16>,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
);
#[derive(sqlx::FromRow)]
struct AlbumBrowseRow {
    id: Uuid,
    title: String,
    album_artist_id: Option<Uuid>,
    year: Option<i16>,
    release_type: Option<String>,
    label: Option<String>,
    musicbrainz_release_group_id: Option<String>,
    musicbrainz_release_id: Option<String>,
    created_at: chrono::DateTime<chrono::Utc>,
    artist_id: Option<Uuid>,
    artist_name: Option<String>,
    artist_sort_name: Option<String>,
    artist_musicbrainz_id: Option<String>,
    track_count: i64,
    duration_ms: i64,
}
type TrackSearchRow = (
    Uuid,
    String,
    Option<i64>,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<Uuid>,
    Option<String>,
    Option<Uuid>,
    Option<i16>,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
);

const ARTIST_COLUMNS: &str = "id, name, sort_name, musicbrainz_artist_id";
const ALBUM_COLUMNS: &str = "id, title, album_artist_id, year, release_type, label, \
     musicbrainz_release_group_id, musicbrainz_release_id";
const ALBUM_SEARCH_COLUMNS: &str = "a.id, a.title, a.album_artist_id, a.year, a.release_type, \
     a.label, a.musicbrainz_release_group_id, a.musicbrainz_release_id";
const ALBUM_BROWSE_COLUMNS: &str = "a.id, a.title, a.album_artist_id, a.year, a.release_type, \
     a.label, a.musicbrainz_release_group_id, a.musicbrainz_release_id, a.created_at, \
     ar.id AS artist_id, ar.name AS artist_name, ar.sort_name AS artist_sort_name, \
     ar.musicbrainz_artist_id AS artist_musicbrainz_id, \
     COUNT(DISTINCT mt.track_id)::BIGINT AS track_count, \
     COALESCE(SUM(t.duration_ms), 0)::BIGINT AS duration_ms";
const ALBUM_BROWSE_FROM: &str = "FROM albums AS a \
     LEFT JOIN artists AS ar ON ar.id = a.album_artist_id \
     LEFT JOIN album_tracks AS mt ON mt.album_id = a.id \
     LEFT JOIN tracks AS t ON t.id = mt.track_id";

fn album_order_clause(order: AlbumOrder) -> &'static str {
    match order {
        AlbumOrder::Title => "a.title_normalized",
        AlbumOrder::Artist => "ar.name_normalized NULLS LAST, a.title_normalized",
        AlbumOrder::RecentlyAdded => "a.created_at DESC",
        AlbumOrder::Random => "random()",
        AlbumOrder::YearAscending => "a.year NULLS LAST",
        AlbumOrder::YearDescending => "a.year DESC NULLS LAST",
        AlbumOrder::Frequent
        | AlbumOrder::Recent
        | AlbumOrder::HighestRated
        | AlbumOrder::Starred => "a.title_normalized",
    }
}

fn attach_album_state(summary: AlbumSummary, states: &HashMap<Uuid, LibraryState>) -> AlbumSummary {
    let state = states
        .get(summary.album().id().as_uuid())
        .copied()
        .unwrap_or_default();

    summary.with_state(state)
}

fn attach_artist_state(
    summary: ArtistSummary,
    states: &HashMap<Uuid, LibraryState>,
) -> ArtistSummary {
    let state = states
        .get(summary.artist().id().as_uuid())
        .copied()
        .unwrap_or_default();

    summary.with_state(state)
}

async fn attach_album_states(
    pool: &PgPool,
    user: Option<Uuid>,
    summaries: &mut [AlbumSummary],
) -> Result<(), TrackRepositoryError> {
    if summaries.is_empty() {
        return Ok(());
    }

    let ids = summaries
        .iter()
        .map(|summary| *summary.album().id().as_uuid())
        .collect::<Vec<_>>();
    let states = album_states(pool, user, &ids).await?;
    for summary in summaries {
        let state = states
            .get(summary.album().id().as_uuid())
            .copied()
            .unwrap_or_default();
        *summary = summary.clone().with_state(state);
    }

    Ok(())
}

async fn attach_artist_states(
    pool: &PgPool,
    user: Option<Uuid>,
    summaries: &mut [ArtistSummary],
) -> Result<(), TrackRepositoryError> {
    if summaries.is_empty() {
        return Ok(());
    }

    let ids = summaries
        .iter()
        .map(|summary| *summary.artist().id().as_uuid())
        .collect::<Vec<_>>();
    let states = artist_states(pool, user, &ids).await?;
    for summary in summaries {
        let state = states
            .get(summary.artist().id().as_uuid())
            .copied()
            .unwrap_or_default();
        *summary = summary.clone().with_state(state);
    }

    Ok(())
}

type StateRow = (
    Uuid,
    Option<chrono::DateTime<chrono::Utc>>,
    Option<i16>,
    i64,
    Option<chrono::DateTime<chrono::Utc>>,
);

fn decode_state(
    starred_at: Option<chrono::DateTime<chrono::Utc>>,
    rating: Option<i16>,
    play_count: i64,
    last_played_at: Option<chrono::DateTime<chrono::Utc>>,
) -> Result<LibraryState, TrackRepositoryError> {
    let rating = rating
        .map(u8::try_from)
        .transpose()
        .map_err(|_| TrackRepositoryError::InvalidData)?;
    let play_count =
        u32::try_from(play_count.max(0)).map_err(|_| TrackRepositoryError::InvalidData)?;

    LibraryState::new(starred_at, rating, play_count, last_played_at)
        .map_err(|_| TrackRepositoryError::InvalidData)
}

async fn track_states(
    pool: &PgPool,
    user: Option<Uuid>,
    track_ids: &[Uuid],
) -> Result<HashMap<Uuid, LibraryState>, TrackRepositoryError> {
    let Some(user) = user else {
        return Ok(HashMap::new());
    };
    if track_ids.is_empty() {
        return Ok(HashMap::new());
    }

    let rows = sqlx::query_as::<_, StateRow>(
        r#"
        SELECT t.id, fav.starred_at, rating.rating,
               COALESCE(plays.play_count, 0)::BIGINT AS play_count, plays.last_played_at
        FROM tracks AS t
        LEFT JOIN track_favorites AS fav ON fav.track_id = t.id AND fav.user_id = $1
        LEFT JOIN track_ratings AS rating ON rating.track_id = t.id AND rating.user_id = $1
        LEFT JOIN (
            SELECT track_id, SUM(play_count)::BIGINT AS play_count,
                   MAX(last_played_at) AS last_played_at
            FROM track_play_stats
            WHERE user_id = $1 AND track_id = ANY($2)
            GROUP BY track_id
        ) AS plays ON plays.track_id = t.id
        WHERE t.id = ANY($2)
        "#,
    )
    .bind(user)
    .bind(track_ids)
    .fetch_all(pool)
    .await
    .map_err(repository_error)?;

    rows.into_iter()
        .map(|(id, starred_at, rating, play_count, last_played_at)| {
            decode_state(starred_at, rating, play_count, last_played_at).map(|state| (id, state))
        })
        .collect()
}

async fn album_states(
    pool: &PgPool,
    user: Option<Uuid>,
    album_ids: &[Uuid],
) -> Result<HashMap<Uuid, LibraryState>, TrackRepositoryError> {
    let Some(user) = user else {
        return Ok(HashMap::new());
    };
    if album_ids.is_empty() {
        return Ok(HashMap::new());
    }

    let rows = sqlx::query_as::<_, StateRow>(
        r#"
        SELECT a.id, fav.starred_at, rating.rating,
               COALESCE(plays.play_count, 0)::BIGINT AS play_count, plays.last_played_at
        FROM albums AS a
        LEFT JOIN album_favorites AS fav ON fav.album_id = a.id AND fav.user_id = $1
        LEFT JOIN album_ratings AS rating ON rating.album_id = a.id AND rating.user_id = $1
        LEFT JOIN (
            SELECT mt.album_id, SUM(stats.play_count)::BIGINT AS play_count,
                   MAX(stats.last_played_at) AS last_played_at
            FROM track_play_stats AS stats
            JOIN album_tracks AS mt ON mt.track_id = stats.track_id
            WHERE stats.user_id = $1 AND mt.album_id = ANY($2)
            GROUP BY mt.album_id
        ) AS plays ON plays.album_id = a.id
        WHERE a.id = ANY($2)
        "#,
    )
    .bind(user)
    .bind(album_ids)
    .fetch_all(pool)
    .await
    .map_err(repository_error)?;

    rows.into_iter()
        .map(|(id, starred_at, rating, play_count, last_played_at)| {
            decode_state(starred_at, rating, play_count, last_played_at).map(|state| (id, state))
        })
        .collect()
}

async fn artist_states(
    pool: &PgPool,
    user: Option<Uuid>,
    artist_ids: &[Uuid],
) -> Result<HashMap<Uuid, LibraryState>, TrackRepositoryError> {
    let Some(user) = user else {
        return Ok(HashMap::new());
    };
    if artist_ids.is_empty() {
        return Ok(HashMap::new());
    }

    let rows = sqlx::query_as::<_, StateRow>(
        r#"
        SELECT ar.id, fav.starred_at, rating.rating,
               COALESCE(plays.play_count, 0)::BIGINT AS play_count, plays.last_played_at
        FROM artists AS ar
        LEFT JOIN artist_favorites AS fav ON fav.artist_id = ar.id AND fav.user_id = $1
        LEFT JOIN artist_ratings AS rating ON rating.artist_id = ar.id AND rating.user_id = $1
        LEFT JOIN (
            SELECT albums.album_artist_id AS artist_id,
                   SUM(stats.play_count)::BIGINT AS play_count,
                   MAX(stats.last_played_at) AS last_played_at
            FROM track_play_stats AS stats
            JOIN album_tracks AS mt ON mt.track_id = stats.track_id
            JOIN albums ON albums.id = mt.album_id
            WHERE stats.user_id = $1 AND albums.album_artist_id = ANY($2)
            GROUP BY albums.album_artist_id
        ) AS plays ON plays.artist_id = ar.id
        WHERE ar.id = ANY($2)
        "#,
    )
    .bind(user)
    .bind(artist_ids)
    .fetch_all(pool)
    .await
    .map_err(repository_error)?;

    rows.into_iter()
        .map(|(id, starred_at, rating, play_count, last_played_at)| {
            decode_state(starred_at, rating, play_count, last_played_at).map(|state| (id, state))
        })
        .collect()
}

async fn ordered_album_ids(
    pool: &PgPool,
    order: AlbumOrder,
    user: Uuid,
    offset: u32,
    limit: u32,
) -> Result<Vec<Uuid>, TrackRepositoryError> {
    let statement = match order {
        AlbumOrder::Starred => {
            "SELECT album_id FROM album_favorites WHERE user_id = $1              ORDER BY starred_at DESC, album_id LIMIT $2 OFFSET $3"
        }
        AlbumOrder::HighestRated => {
            "SELECT album_id FROM album_ratings WHERE user_id = $1              ORDER BY rating DESC, album_id LIMIT $2 OFFSET $3"
        }
        AlbumOrder::Recent => {
            r#"
            SELECT mt.album_id
            FROM track_play_stats AS stats
            JOIN album_tracks AS mt ON mt.track_id = stats.track_id
            WHERE stats.user_id = $1
            GROUP BY mt.album_id
            HAVING MAX(stats.last_played_at) IS NOT NULL
            ORDER BY MAX(stats.last_played_at) DESC, mt.album_id
            LIMIT $2 OFFSET $3
            "#
        }
        _ => {
            r#"
            SELECT mt.album_id
            FROM track_play_stats AS stats
            JOIN album_tracks AS mt ON mt.track_id = stats.track_id
            WHERE stats.user_id = $1
            GROUP BY mt.album_id
            HAVING SUM(stats.play_count) > 0
            ORDER BY SUM(stats.play_count) DESC, mt.album_id
            LIMIT $2 OFFSET $3
            "#
        }
    };

    sqlx::query_scalar::<_, Uuid>(statement)
        .bind(user)
        .bind(i64::from(limit))
        .bind(i64::from(offset))
        .fetch_all(pool)
        .await
        .map_err(repository_error)
}

async fn album_summaries_by_ids(
    pool: &PgPool,
    album_ids: &[Uuid],
    user: Option<Uuid>,
) -> Result<Vec<AlbumSummary>, TrackRepositoryError> {
    if album_ids.is_empty() {
        return Ok(Vec::new());
    }

    let rows = sqlx::query_as::<_, AlbumBrowseRow>(&format!(
        r#"
        SELECT {ALBUM_BROWSE_COLUMNS}
        {ALBUM_BROWSE_FROM}
        WHERE a.id = ANY($1)
        GROUP BY a.id, ar.id
        "#
    ))
    .bind(album_ids)
    .fetch_all(pool)
    .await
    .map_err(repository_error)?;

    let states = album_states(pool, user, album_ids).await?;
    let mut by_id = rows
        .into_iter()
        .map(decode_album_summary)
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .map(|summary| {
            (
                *summary.album().id().as_uuid(),
                attach_album_state(summary, &states),
            )
        })
        .collect::<HashMap<_, _>>();

    Ok(album_ids.iter().filter_map(|id| by_id.remove(id)).collect())
}

async fn artist_summaries_by_ids(
    pool: &PgPool,
    artist_ids: &[Uuid],
    user: Option<Uuid>,
) -> Result<Vec<ArtistSummary>, TrackRepositoryError> {
    if artist_ids.is_empty() {
        return Ok(Vec::new());
    }

    let rows = sqlx::query_as::<_, (Uuid, String, Option<String>, Option<String>, i64)>(
        r#"
        SELECT ar.id, ar.name, ar.sort_name, ar.musicbrainz_artist_id,
               (SELECT COUNT(*) FROM albums WHERE album_artist_id = ar.id)::BIGINT
        FROM artists AS ar
        WHERE ar.id = ANY($1)
        "#,
    )
    .bind(artist_ids)
    .fetch_all(pool)
    .await
    .map_err(repository_error)?;

    let states = artist_states(pool, user, artist_ids).await?;
    let mut by_id = rows
        .into_iter()
        .map(decode_artist_summary)
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .map(|summary| {
            (
                *summary.artist().id().as_uuid(),
                attach_artist_state(summary, &states),
            )
        })
        .collect::<HashMap<_, _>>();

    Ok(artist_ids
        .iter()
        .filter_map(|id| by_id.remove(id))
        .collect())
}

async fn hits_from_rows(
    pool: &PgPool,
    rows: Vec<TrackSearchRow>,
    user: Option<Uuid>,
) -> Result<Vec<TrackHit>, TrackRepositoryError> {
    let track_ids = rows.iter().map(|row| row.0).collect::<Vec<_>>();
    let mut credits = credits_for_tracks(pool, &track_ids).await?;
    let mut sources = sources_for_tracks(pool, &track_ids).await?;
    let states = track_states(pool, user, &track_ids).await?;
    let mut hits = Vec::with_capacity(rows.len());

    for row in rows {
        let (
            id,
            title,
            duration_ms,
            recording_id,
            isrc,
            genre,
            album_id,
            album_title,
            album_artist_id,
            year,
            release_type,
            label,
            group_id,
            release_id,
        ) = row;

        let track = decode_track(
            id,
            title,
            duration_ms,
            recording_id,
            isrc,
            genre,
            credits.remove(&id).unwrap_or_default(),
        )?;
        let album = match (album_id, album_title) {
            (Some(album_id), Some(album_title)) => Some(
                Album::new(
                    AlbumId::from_uuid(album_id),
                    album_title,
                    album_artist_id.map(ArtistId::from_uuid),
                    year.map(i32::from),
                    release_type,
                    label,
                    group_id,
                    release_id,
                )
                .map_err(|_| TrackRepositoryError::InvalidData)?,
            ),
            _ => None,
        };
        let state = states.get(&id).copied().unwrap_or_default();

        hits.push(
            TrackHit::new(track, album, sources.remove(&id).unwrap_or_default()).with_state(state),
        );
    }

    Ok(hits)
}

async fn tracks_by_ids(
    pool: &PgPool,
    track_ids: &[Uuid],
    user: Uuid,
) -> Result<Vec<TrackHit>, TrackRepositoryError> {
    if track_ids.is_empty() {
        return Ok(Vec::new());
    }

    let rows = sqlx::query_as::<_, TrackSearchRow>(&format!(
        r#"
        SELECT t.id, t.title, t.duration_ms, t.musicbrainz_recording_id, t.isrc, t.genre,
               {ALBUM_SEARCH_COLUMNS}
        FROM tracks AS t
        LEFT JOIN album_tracks AS mt ON mt.track_id = t.id
        LEFT JOIN albums AS a ON a.id = mt.album_id
        WHERE t.id = ANY($1)
        "#
    ))
    .bind(track_ids)
    .fetch_all(pool)
    .await
    .map_err(repository_error)?;

    let hits = hits_from_rows(pool, rows, Some(user)).await?;
    let mut by_id = hits
        .into_iter()
        .map(|hit| (*hit.track().id().as_uuid(), hit))
        .collect::<HashMap<_, _>>();

    Ok(track_ids.iter().filter_map(|id| by_id.remove(id)).collect())
}

async fn set_starred_row(
    pool: &PgPool,
    table: &'static str,
    column: &'static str,
    user: Uuid,
    id: &Uuid,
    starred: bool,
) -> Result<(), TrackRepositoryError> {
    let statement = if starred {
        format!("INSERT INTO {table} (user_id, {column}) VALUES ($1, $2) ON CONFLICT DO NOTHING")
    } else {
        format!("DELETE FROM {table} WHERE user_id = $1 AND {column} = $2")
    };

    sqlx::query(&statement)
        .bind(user)
        .bind(id)
        .execute(pool)
        .await
        .map(|_| ())
        .map_err(repository_error)
}

async fn set_rating_row(
    pool: &PgPool,
    table: &'static str,
    column: &'static str,
    user: Uuid,
    id: &Uuid,
    rating: Option<i16>,
) -> Result<(), TrackRepositoryError> {
    match rating {
        Some(rating) => {
            let statement = format!(
                "INSERT INTO {table} (user_id, {column}, rating) VALUES ($1, $2, $3)                  ON CONFLICT (user_id, {column}) DO UPDATE                  SET rating = EXCLUDED.rating, updated_at = CURRENT_TIMESTAMP"
            );
            sqlx::query(&statement)
                .bind(user)
                .bind(id)
                .bind(rating)
                .execute(pool)
                .await
                .map(|_| ())
                .map_err(repository_error)
        }
        None => {
            let statement = format!("DELETE FROM {table} WHERE user_id = $1 AND {column} = $2");
            sqlx::query(&statement)
                .bind(user)
                .bind(id)
                .execute(pool)
                .await
                .map(|_| ())
                .map_err(repository_error)
        }
    }
}

async fn resolve_artist(
    pool: &PgPool,
    credit: &ArtistCredit,
) -> Result<Artist, TrackRepositoryError> {
    for _ in 0..2 {
        if let Some(artist) = find_or_attach_artist(pool, credit).await? {
            return Ok(artist);
        }
        if let Some(artist) = insert_artist(pool, credit).await? {
            return Ok(artist);
        }
    }

    Err(TrackRepositoryError::InvalidData)
}

async fn find_or_attach_artist(
    pool: &PgPool,
    credit: &ArtistCredit,
) -> Result<Option<Artist>, TrackRepositoryError> {
    if let Some(identifier) = credit.musicbrainz_artist_id()
        && let Some(artist) = find_artist_by_identifier(pool, identifier).await?
    {
        return Ok(Some(artist));
    }

    let Some(existing) = find_artist_by_name(pool, credit.name()).await? else {
        return Ok(None);
    };

    let Some(identifier) = credit.musicbrainz_artist_id() else {
        return Ok(Some(existing));
    };

    let row = sqlx::query_as::<_, ArtistRow>(&format!(
        r#"
        UPDATE artists
        SET musicbrainz_artist_id = $1, updated_at = CURRENT_TIMESTAMP
        WHERE id = $2 AND musicbrainz_artist_id IS NULL
        RETURNING {ARTIST_COLUMNS}
        "#
    ))
    .bind(identifier)
    .bind(existing.id().as_uuid())
    .fetch_optional(pool)
    .await
    .map_err(repository_error)?;

    match row {
        Some(row) => Ok(Some(decode_artist(row)?)),
        None => find_artist_by_identifier(pool, identifier).await,
    }
}

async fn find_artist_by_identifier(
    pool: &PgPool,
    identifier: &str,
) -> Result<Option<Artist>, TrackRepositoryError> {
    let row = sqlx::query_as::<_, ArtistRow>(&format!(
        "SELECT {ARTIST_COLUMNS} FROM artists WHERE musicbrainz_artist_id = $1"
    ))
    .bind(identifier)
    .fetch_optional(pool)
    .await
    .map_err(repository_error)?;

    row.map(decode_artist).transpose()
}

async fn find_artist_by_name(
    pool: &PgPool,
    name: &str,
) -> Result<Option<Artist>, TrackRepositoryError> {
    let row = sqlx::query_as::<_, ArtistRow>(&format!(
        r#"
        SELECT {ARTIST_COLUMNS}
        FROM artists
        WHERE musicbrainz_artist_id IS NULL
          AND name_normalized = lower(regexp_replace(normalize($1, NFC), '\s+', ' ', 'g'))
        "#
    ))
    .bind(name)
    .fetch_optional(pool)
    .await
    .map_err(repository_error)?;

    row.map(decode_artist).transpose()
}

async fn insert_artist(
    pool: &PgPool,
    credit: &ArtistCredit,
) -> Result<Option<Artist>, TrackRepositoryError> {
    let row = sqlx::query_as::<_, ArtistRow>(&format!(
        r#"
        INSERT INTO artists (id, name, musicbrainz_artist_id)
        VALUES ($1, $2, $3)
        ON CONFLICT DO NOTHING
        RETURNING {ARTIST_COLUMNS}
        "#
    ))
    .bind(ArtistId::new().as_uuid())
    .bind(credit.name())
    .bind(credit.musicbrainz_artist_id())
    .fetch_optional(pool)
    .await
    .map_err(repository_error)?;

    row.map(decode_artist).transpose()
}

async fn resolve_album(
    pool: &PgPool,
    metadata: &AlbumMetadata,
) -> Result<Album, TrackRepositoryError> {
    let album_artist_id = match metadata.album_artist() {
        Some(credit) => Some(resolve_artist(pool, credit).await?.id().clone()),
        None => None,
    };

    for _ in 0..2 {
        if let Some(album) = find_or_attach_album(pool, metadata, album_artist_id.as_ref()).await? {
            return Ok(album);
        }
        if let Some(album) = insert_album(pool, metadata, album_artist_id.as_ref()).await? {
            return Ok(album);
        }
    }

    Err(TrackRepositoryError::InvalidData)
}

async fn find_or_attach_album(
    pool: &PgPool,
    metadata: &AlbumMetadata,
    album_artist_id: Option<&ArtistId>,
) -> Result<Option<Album>, TrackRepositoryError> {
    if let Some(identifier) = metadata.musicbrainz_release_group_id()
        && let Some(album) = find_album_by_release_group(pool, identifier).await?
    {
        return Ok(Some(album));
    }

    if let Some(identifier) = metadata.musicbrainz_release_id()
        && let Some(album) = find_album_by_release(pool, identifier).await?
    {
        return Ok(Some(album));
    }

    let Some(existing) = find_album_by_text(pool, metadata.title(), album_artist_id).await? else {
        return Ok(None);
    };

    let existing_has_identifier = existing.musicbrainz_release_group_id().is_some()
        || existing.musicbrainz_release_id().is_some();
    let incoming_has_identifier = metadata.musicbrainz_release_group_id().is_some()
        || metadata.musicbrainz_release_id().is_some();

    if existing_has_identifier && incoming_has_identifier {
        return Ok(None);
    }
    if !incoming_has_identifier {
        return Ok(Some(existing));
    }

    let row = sqlx::query_as::<_, AlbumRow>(&format!(
        r#"
        UPDATE albums
        SET musicbrainz_release_group_id = COALESCE($1, musicbrainz_release_group_id),
            musicbrainz_release_id = COALESCE($2, musicbrainz_release_id),
            year = COALESCE(year, $3),
            release_type = COALESCE(release_type, $4),
            label = COALESCE(label, $5),
            updated_at = CURRENT_TIMESTAMP
        WHERE id = $6
        RETURNING {ALBUM_COLUMNS}
        "#
    ))
    .bind(metadata.musicbrainz_release_group_id())
    .bind(metadata.musicbrainz_release_id())
    .bind(metadata.year())
    .bind(metadata.release_type())
    .bind(metadata.label())
    .bind(existing.id().as_uuid())
    .fetch_optional(pool)
    .await
    .map_err(repository_error)?;

    match row {
        Some(row) => Ok(Some(decode_album(row)?)),
        None => Ok(None),
    }
}

async fn find_album_by_release_group(
    pool: &PgPool,
    identifier: &str,
) -> Result<Option<Album>, TrackRepositoryError> {
    let row = sqlx::query_as::<_, AlbumRow>(&format!(
        "SELECT {ALBUM_COLUMNS} FROM albums WHERE musicbrainz_release_group_id = $1"
    ))
    .bind(identifier)
    .fetch_optional(pool)
    .await
    .map_err(repository_error)?;

    row.map(decode_album).transpose()
}

async fn find_album_by_release(
    pool: &PgPool,
    identifier: &str,
) -> Result<Option<Album>, TrackRepositoryError> {
    let row = sqlx::query_as::<_, AlbumRow>(&format!(
        "SELECT {ALBUM_COLUMNS} FROM albums WHERE musicbrainz_release_id = $1"
    ))
    .bind(identifier)
    .fetch_optional(pool)
    .await
    .map_err(repository_error)?;

    row.map(decode_album).transpose()
}

async fn find_album_by_text(
    pool: &PgPool,
    title: &str,
    album_artist_id: Option<&ArtistId>,
) -> Result<Option<Album>, TrackRepositoryError> {
    let row = sqlx::query_as::<_, AlbumRow>(&format!(
        r#"
        SELECT {ALBUM_COLUMNS}
        FROM albums
        WHERE album_artist_id IS NOT DISTINCT FROM $1
          AND title_normalized = lower(regexp_replace(normalize($2, NFC), '\s+', ' ', 'g'))
        ORDER BY (musicbrainz_release_group_id IS NULL), created_at
        LIMIT 1
        "#
    ))
    .bind(album_artist_id.map(|id| *id.as_uuid()))
    .bind(title)
    .fetch_optional(pool)
    .await
    .map_err(repository_error)?;

    row.map(decode_album).transpose()
}

async fn insert_album(
    pool: &PgPool,
    metadata: &AlbumMetadata,
    album_artist_id: Option<&ArtistId>,
) -> Result<Option<Album>, TrackRepositoryError> {
    let row = sqlx::query_as::<_, AlbumRow>(&format!(
        r#"
        INSERT INTO albums (
            id, title, album_artist_id, year, release_type, label,
            musicbrainz_release_group_id, musicbrainz_release_id
        )
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
        ON CONFLICT DO NOTHING
        RETURNING {ALBUM_COLUMNS}
        "#
    ))
    .bind(AlbumId::new().as_uuid())
    .bind(metadata.title())
    .bind(album_artist_id.map(|id| *id.as_uuid()))
    .bind(metadata.year())
    .bind(metadata.release_type())
    .bind(metadata.label())
    .bind(metadata.musicbrainz_release_group_id())
    .bind(metadata.musicbrainz_release_id())
    .fetch_optional(pool)
    .await
    .map_err(repository_error)?;

    row.map(decode_album).transpose()
}

fn decode_artist(row: ArtistRow) -> Result<Artist, TrackRepositoryError> {
    let (id, name, sort_name, identifier) = row;

    Artist::new(ArtistId::from_uuid(id), name, sort_name, identifier)
        .map_err(|_| TrackRepositoryError::InvalidData)
}

fn decode_album(row: AlbumRow) -> Result<Album, TrackRepositoryError> {
    let (id, title, album_artist_id, year, release_type, label, group_id, release_id) = row;

    Album::new(
        AlbumId::from_uuid(id),
        title,
        album_artist_id.map(ArtistId::from_uuid),
        year.map(i32::from),
        release_type,
        label,
        group_id,
        release_id,
    )
    .map_err(|_| TrackRepositoryError::InvalidData)
}

fn decode_album_summary(row: AlbumBrowseRow) -> Result<AlbumSummary, TrackRepositoryError> {
    let AlbumBrowseRow {
        id,
        title,
        album_artist_id,
        year,
        release_type,
        label,
        musicbrainz_release_group_id: group_id,
        musicbrainz_release_id: release_id,
        created_at,
        artist_id,
        artist_name,
        artist_sort_name,
        artist_musicbrainz_id,
        track_count,
        duration_ms,
    } = row;

    let album = Album::new(
        AlbumId::from_uuid(id),
        title,
        album_artist_id.map(ArtistId::from_uuid),
        year.map(i32::from),
        release_type,
        label,
        group_id,
        release_id,
    )
    .map_err(|_| TrackRepositoryError::InvalidData)?;

    let album_artist = match (artist_id, artist_name) {
        (Some(id), Some(name)) => Some(
            Artist::new(
                ArtistId::from_uuid(id),
                name,
                artist_sort_name,
                artist_musicbrainz_id,
            )
            .map_err(|_| TrackRepositoryError::InvalidData)?,
        ),
        _ => None,
    };
    let track_count = u32::try_from(track_count).map_err(|_| TrackRepositoryError::InvalidData)?;

    Ok(AlbumSummary::new(
        album,
        album_artist,
        track_count,
        duration_ms.max(0) as u64,
        created_at,
    ))
}

fn decode_artist_summary(
    row: (Uuid, String, Option<String>, Option<String>, i64),
) -> Result<ArtistSummary, TrackRepositoryError> {
    let (id, name, sort_name, identifier, album_count) = row;
    let artist = Artist::new(ArtistId::from_uuid(id), name, sort_name, identifier)
        .map_err(|_| TrackRepositoryError::InvalidData)?;
    let album_count = u32::try_from(album_count).map_err(|_| TrackRepositoryError::InvalidData)?;

    Ok(ArtistSummary::new(artist, album_count))
}

fn decode_track(
    id: Uuid,
    title: String,
    duration_ms: Option<i64>,
    recording_id: Option<String>,
    isrc: Option<String>,
    genre: Option<String>,
    artists: Vec<ArtistCredit>,
) -> Result<Track, TrackRepositoryError> {
    let duration_ms = duration_ms
        .map(u64::try_from)
        .transpose()
        .map_err(|_| TrackRepositoryError::InvalidData)?;
    let metadata = TrackMetadata::new(title, artists, duration_ms)
        .map_err(|_| TrackRepositoryError::InvalidData)?
        .with_recording_id(recording_id)
        .map_err(|_| TrackRepositoryError::InvalidData)?
        .with_isrc(isrc)
        .map_err(|_| TrackRepositoryError::InvalidData)?
        .with_genre(genre)
        .map_err(|_| TrackRepositoryError::InvalidData)?;

    Ok(Track::new(TrackId::from_uuid(id), metadata))
}

fn decode_membership(
    disc_number: Option<i16>,
    track_number: Option<i16>,
    position: i32,
) -> Result<AlbumTrack, TrackRepositoryError> {
    let position = u32::try_from(position).map_err(|_| TrackRepositoryError::InvalidData)?;

    AlbumTrack::new(
        decode_smallint(disc_number)?,
        decode_smallint(track_number)?,
        position,
    )
    .map_err(|_| TrackRepositoryError::InvalidData)
}

async fn credits_for_tracks(
    pool: &PgPool,
    track_ids: &[Uuid],
) -> Result<HashMap<Uuid, Vec<ArtistCredit>>, TrackRepositoryError> {
    if track_ids.is_empty() {
        return Ok(HashMap::new());
    }

    let rows = sqlx::query_as::<_, (Uuid, String, Option<String>)>(
        r#"
        SELECT credits.track_id, credits.name, artists.musicbrainz_artist_id
        FROM track_artist_credits AS credits
        LEFT JOIN artists ON artists.id = credits.artist_id
        WHERE credits.track_id = ANY($1)
        ORDER BY credits.track_id, credits.position
        "#,
    )
    .bind(track_ids)
    .fetch_all(pool)
    .await
    .map_err(repository_error)?;

    let mut by_track: HashMap<Uuid, Vec<ArtistCredit>> = HashMap::new();
    for (track_id, name, identifier) in rows {
        let credit =
            ArtistCredit::new(name, identifier).map_err(|_| TrackRepositoryError::InvalidData)?;

        by_track.entry(track_id).or_default().push(credit);
    }

    Ok(by_track)
}

async fn sources_for_tracks(
    pool: &PgPool,
    track_ids: &[Uuid],
) -> Result<HashMap<Uuid, Vec<SourceIdentity>>, TrackRepositoryError> {
    if track_ids.is_empty() {
        return Ok(HashMap::new());
    }

    let rows = sqlx::query_as::<_, (Uuid, String, String, String)>(
        r#"
        SELECT track_id, provider_id, source_scope, external_id
        FROM track_sources
        WHERE track_id = ANY($1)
        ORDER BY created_at, id
        "#,
    )
    .bind(track_ids)
    .fetch_all(pool)
    .await
    .map_err(repository_error)?;

    let mut by_track: HashMap<Uuid, Vec<SourceIdentity>> = HashMap::new();
    for (track_id, provider_id, source_scope, external_id) in rows {
        let source = decode_source(provider_id, source_scope, external_id)?;

        by_track.entry(track_id).or_default().push(source);
    }

    Ok(by_track)
}

fn encode_smallint(value: Option<u16>) -> Result<Option<i16>, TrackRepositoryError> {
    value
        .map(i16::try_from)
        .transpose()
        .map_err(|_| TrackRepositoryError::InvalidData)
}

fn decode_smallint(value: Option<i16>) -> Result<Option<u16>, TrackRepositoryError> {
    value
        .map(u16::try_from)
        .transpose()
        .map_err(|_| TrackRepositoryError::InvalidData)
}

fn decode_source(
    provider_id: String,
    source_scope: String,
    external_id: String,
) -> Result<SourceIdentity, TrackRepositoryError> {
    let provider_id =
        ProviderId::new(provider_id).map_err(|_| TrackRepositoryError::InvalidData)?;

    let source_scope = match source_scope.as_str() {
        "global" => SourceScope::Global,
        value if value.starts_with("named:") && value.len() > "named:".len() => {
            SourceScope::Named(value["named:".len()..].to_owned())
        }
        _ => return Err(TrackRepositoryError::InvalidData),
    };

    SourceIdentity::new(provider_id, source_scope, external_id)
        .map_err(|_| TrackRepositoryError::InvalidData)
}

async fn set_source_file_facts_in(
    connection: &mut PgConnection,
    source: &SourceIdentity,
    facts: &SourceFileFacts,
) -> Result<(), TrackRepositoryError> {
    sqlx::query(
        r#"UPDATE track_sources
        SET path = $1, size_bytes = $2, modified_at = $3, missing_at = $4,
            content_type = $5, suffix = $6, acoustid_fingerprint = $7
        WHERE provider_id = $8 AND source_scope = $9 AND external_id = $10
        "#,
    )
    .bind(facts.path.as_deref())
    .bind(facts.size_bytes as i64)
    .bind(facts.modified_at)
    .bind(facts.missing_at)
    .bind(facts.content_type.as_deref())
    .bind(facts.suffix.as_deref())
    .bind(facts.acoustid_fingerprint.as_deref())
    .bind(source.provider_id().as_str())
    .bind(encode_source_scope(source.source_scope()))
    .bind(source.external_id())
    .execute(&mut *connection)
    .await
    .map(|_| ())
    .map_err(repository_error)
}

async fn get_or_create_in(
    connection: &mut PgConnection,
    source: &SourceIdentity,
    metadata: &TrackMetadata,
    credits: &[(String, ArtistId)],
) -> Result<TrackId, TrackRepositoryError> {
    let track_id = match resolve_identified_track(connection, metadata).await? {
        Some(identified) => {
            let previous = find_track_id_by_source_for_update(connection, source).await?;
            attach_source(connection, &identified, source).await?;

            if let Some(previous) = previous
                && previous != identified
            {
                delete_track_if_source_less(connection, &previous).await?;
            }

            identified
        }
        None => match find_track_id_by_source_for_update(connection, source).await? {
            Some(current) => current,
            None => create_with_source(connection, source, metadata).await?,
        },
    };

    update_track_metadata(connection, &track_id, metadata).await?;
    replace_artist_credits(connection, &track_id, credits).await?;

    Ok(track_id)
}

async fn resolve_identified_track(
    connection: &mut PgConnection,
    metadata: &TrackMetadata,
) -> Result<Option<TrackId>, TrackRepositoryError> {
    if let Some(track_id) =
        find_track_id_by_recording_id(connection, metadata.musicbrainz_recording_id()).await?
    {
        return Ok(Some(track_id));
    }

    find_track_id_by_isrc(connection, metadata.isrc()).await
}

async fn find_track_id_by_recording_id(
    connection: &mut PgConnection,
    recording_id: Option<&str>,
) -> Result<Option<TrackId>, TrackRepositoryError> {
    let Some(recording_id) = recording_id else {
        return Ok(None);
    };

    sqlx::query_scalar::<_, Uuid>("SELECT id FROM tracks WHERE musicbrainz_recording_id = $1")
        .bind(recording_id)
        .fetch_optional(&mut *connection)
        .await
        .map(|row| row.map(TrackId::from_uuid))
        .map_err(repository_error)
}

async fn find_track_id_by_isrc(
    connection: &mut PgConnection,
    isrc: Option<&str>,
) -> Result<Option<TrackId>, TrackRepositoryError> {
    let Some(isrc) = isrc else {
        return Ok(None);
    };

    let rows =
        sqlx::query_scalar::<_, Uuid>("SELECT id FROM tracks WHERE isrc = $1 ORDER BY id LIMIT 2")
            .bind(isrc)
            .fetch_all(&mut *connection)
            .await
            .map_err(repository_error)?;

    Ok(match rows.as_slice() {
        [only] => Some(TrackId::from_uuid(*only)),
        _ => None,
    })
}

async fn create_with_source(
    connection: &mut PgConnection,
    source: &SourceIdentity,
    metadata: &TrackMetadata,
) -> Result<TrackId, TrackRepositoryError> {
    let candidate = TrackId::new();

    if !insert_candidate_track(connection, &candidate, metadata).await? {
        let winner = find_track_id_by_recording_id(connection, metadata.musicbrainz_recording_id())
            .await?
            .ok_or(TrackRepositoryError::Unavailable)?;

        attach_source(connection, &winner, source).await?;

        return Ok(winner);
    }

    if try_insert_source(connection, &candidate, source).await? {
        return Ok(candidate);
    }

    resolve_concurrent_winner(connection, &candidate, source).await
}

async fn attach_source(
    connection: &mut PgConnection,
    track_id: &TrackId,
    source: &SourceIdentity,
) -> Result<(), TrackRepositoryError> {
    sqlx::query(
        r#"
        INSERT INTO track_sources (
            id,
            track_id,
            provider_id,
            source_scope,
            external_id
        )
        VALUES ($1, $2, $3, $4, $5)
        ON CONFLICT (provider_id, source_scope, external_id)
        DO UPDATE SET track_id = EXCLUDED.track_id
        "#,
    )
    .bind(Uuid::now_v7())
    .bind(track_id.as_uuid())
    .bind(source.provider_id().as_str())
    .bind(encode_source_scope(source.source_scope()))
    .bind(source.external_id())
    .execute(&mut *connection)
    .await
    .map(|_| ())
    .map_err(repository_error)
}

async fn delete_track_if_source_less(
    connection: &mut PgConnection,
    track_id: &TrackId,
) -> Result<(), TrackRepositoryError> {
    sqlx::query(
        r#"
        DELETE FROM tracks
        WHERE id = $1
          AND NOT EXISTS (
              SELECT 1 FROM track_sources WHERE track_sources.track_id = tracks.id
          )
        "#,
    )
    .bind(track_id.as_uuid())
    .execute(&mut *connection)
    .await
    .map(|_| ())
    .map_err(repository_error)
}

async fn find_track_id_by_source_for_update(
    connection: &mut PgConnection,
    source: &SourceIdentity,
) -> Result<Option<TrackId>, TrackRepositoryError> {
    sqlx::query_scalar::<_, Uuid>(
        r#"
        SELECT track_id
        FROM track_sources
        WHERE provider_id = $1
          AND source_scope = $2
          AND external_id = $3
        FOR UPDATE
        "#,
    )
    .bind(source.provider_id().as_str())
    .bind(encode_source_scope(source.source_scope()))
    .bind(source.external_id())
    .fetch_optional(&mut *connection)
    .await
    .map(|track_id| track_id.map(TrackId::from_uuid))
    .map_err(repository_error)
}

async fn insert_candidate_track(
    connection: &mut PgConnection,
    track_id: &TrackId,
    metadata: &TrackMetadata,
) -> Result<bool, TrackRepositoryError> {
    sqlx::query_scalar::<_, Uuid>(
        r#"
        INSERT INTO tracks (id, title, duration_ms, musicbrainz_recording_id, isrc, genre)
        VALUES ($1, $2, $3, $4, $5, $6)
        ON CONFLICT (musicbrainz_recording_id) WHERE musicbrainz_recording_id IS NOT NULL
        DO NOTHING
        RETURNING id
        "#,
    )
    .bind(track_id.as_uuid())
    .bind(metadata.title())
    .bind(encode_duration_ms(metadata.duration_ms())?)
    .bind(metadata.musicbrainz_recording_id())
    .bind(metadata.isrc())
    .bind(metadata.genre())
    .fetch_optional(&mut *connection)
    .await
    .map(|row| row.is_some())
    .map_err(repository_error)
}

async fn try_insert_source(
    connection: &mut PgConnection,
    track_id: &TrackId,
    source: &SourceIdentity,
) -> Result<bool, TrackRepositoryError> {
    sqlx::query_scalar::<_, Uuid>(
        r#"
        INSERT INTO track_sources (
            id,
            track_id,
            provider_id,
            source_scope,
            external_id
        )
        VALUES ($1, $2, $3, $4, $5)
        ON CONFLICT (provider_id, source_scope, external_id)
        DO NOTHING
        RETURNING track_id
        "#,
    )
    .bind(Uuid::now_v7())
    .bind(track_id.as_uuid())
    .bind(source.provider_id().as_str())
    .bind(encode_source_scope(source.source_scope()))
    .bind(source.external_id())
    .fetch_optional(&mut *connection)
    .await
    .map(|track_id| track_id.is_some())
    .map_err(repository_error)
}

async fn resolve_concurrent_winner(
    connection: &mut PgConnection,
    candidate_track_id: &TrackId,
    source: &SourceIdentity,
) -> Result<TrackId, TrackRepositoryError> {
    let result = sqlx::query(
        r#"
        DELETE FROM tracks
        WHERE id = $1
        "#,
    )
    .bind(candidate_track_id.as_uuid())
    .execute(&mut *connection)
    .await
    .map_err(repository_error)?;

    if result.rows_affected() != 1 {
        return Err(TrackRepositoryError::InvalidData);
    }

    find_track_id_by_source_for_update(connection, source)
        .await?
        .ok_or(TrackRepositoryError::InvalidData)
}

async fn update_track_metadata(
    connection: &mut PgConnection,
    track_id: &TrackId,
    metadata: &TrackMetadata,
) -> Result<(), TrackRepositoryError> {
    let result = sqlx::query(
        r#"
        UPDATE tracks
        SET title = $1,
            duration_ms = $2,
            musicbrainz_recording_id = COALESCE($3, musicbrainz_recording_id),
            isrc = COALESCE($4, isrc),
            genre = COALESCE($5, genre),
            updated_at = CURRENT_TIMESTAMP
        WHERE id = $6
        "#,
    )
    .bind(metadata.title())
    .bind(encode_duration_ms(metadata.duration_ms())?)
    .bind(metadata.musicbrainz_recording_id())
    .bind(metadata.isrc())
    .bind(metadata.genre())
    .bind(track_id.as_uuid())
    .execute(&mut *connection)
    .await
    .map_err(repository_error)?;

    if result.rows_affected() != 1 {
        return Err(TrackRepositoryError::InvalidData);
    }

    Ok(())
}

async fn replace_artist_credits(
    connection: &mut PgConnection,
    track_id: &TrackId,
    credits: &[(String, ArtistId)],
) -> Result<(), TrackRepositoryError> {
    sqlx::query(
        r#"
        DELETE FROM track_artist_credits
        WHERE track_id = $1
        "#,
    )
    .bind(track_id.as_uuid())
    .execute(&mut *connection)
    .await
    .map_err(repository_error)?;

    for (position, (name, artist_id)) in credits.iter().enumerate() {
        let position = i32::try_from(position).map_err(|_| TrackRepositoryError::InvalidData)?;

        sqlx::query(
            r#"
            INSERT INTO track_artist_credits (track_id, position, name, artist_id)
            VALUES ($1, $2, $3, $4)
            "#,
        )
        .bind(track_id.as_uuid())
        .bind(position)
        .bind(name)
        .bind(artist_id.as_uuid())
        .execute(&mut *connection)
        .await
        .map_err(repository_error)?;
    }

    Ok(())
}

async fn resolve_credits(
    pool: &PgPool,
    credits: &[ArtistCredit],
) -> Result<Vec<(String, ArtistId)>, TrackRepositoryError> {
    let mut resolved = Vec::with_capacity(credits.len());

    for credit in credits {
        let artist = resolve_artist(pool, credit).await?;
        resolved.push((credit.name().to_owned(), artist.id().clone()));
    }

    Ok(resolved)
}

fn encode_duration_ms(duration_ms: Option<u64>) -> Result<Option<i64>, TrackRepositoryError> {
    duration_ms
        .map(i64::try_from)
        .transpose()
        .map_err(|_| TrackRepositoryError::InvalidData)
}

fn repository_error(error: sqlx::Error) -> TrackRepositoryError {
    if let sqlx::Error::Database(database) = &error
        && (database.is_unique_violation()
            || database.is_check_violation()
            || database.is_foreign_key_violation())
    {
        return TrackRepositoryError::InvalidData;
    }

    TrackRepositoryError::Unavailable
}

fn encode_source_scope(source_scope: &SourceScope) -> String {
    match source_scope {
        SourceScope::Global => "global".to_owned(),
        SourceScope::Named(name) => format!("named:{}", name),
    }
}
