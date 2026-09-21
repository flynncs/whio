use sqlx::PgPool;

use async_trait::async_trait;
use uuid::Uuid;

use crate::tracks::{
    Album, AlbumEntry, AlbumId, AlbumListFilter, AlbumOrder, AlbumSummary, Artist, ArtistCredit,
    ArtistId, ArtistSummary, GenreSummary, LOCAL_PROVIDER_ID, SearchTerms, Track, TrackHit,
    TrackId, TrackRepositoryError,
};

use crate::tracks::LibraryRead;

use super::PgStore;
use super::decode::{
    album_states, album_summaries_by_ids, attach_album_state, attach_album_states,
    attach_artist_states, credits_for_tracks, decode_album, decode_album_summary, decode_artist,
    decode_artist_summary, decode_membership, decode_track, hits_from_rows, repository_error,
    track_states,
};
use super::schema::{
    ALBUM_BROWSE_COLUMNS, ALBUM_BROWSE_FROM, ALBUM_COLUMNS, ALBUM_SEARCH_COLUMNS, ARTIST_COLUMNS,
    AlbumBrowseRow, AlbumRow, ArtistRow, CREDIT_NORMALIZED, TrackSearchRow,
};
use crate::tracks::domain::{album_quality, normalize_for_matching, track_quality};

#[async_trait]
impl LibraryRead for PgStore {
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
        let terms = SearchTerms::new(query);

        if terms.is_empty() {
            return Ok(Vec::new());
        }

        let needle = terms.normalized().to_owned();
        let tokens = terms.tokens().to_vec();
        let prefix = format!("{}%", escape_like(&needle));
        let contains = format!("%{}%", escape_like(&needle));
        let rows = sqlx::query_as::<_, TrackSearchRow>(&format!(
            r#"
            SELECT t.id, t.title, t.duration_ms, t.musicbrainz_recording_id, t.isrc, t.genre,
                   {ALBUM_SEARCH_COLUMNS}
            FROM tracks AS t
            LEFT JOIN album_tracks AS mt ON mt.track_id = t.id
            LEFT JOIN albums AS a ON a.id = mt.album_id
            WHERE t.title_normalized = $1
               OR t.title_normalized LIKE $2
               OR string_to_array(t.title_normalized, ' ') @> $3
               OR (array_length($3, 1) > 1 AND t.title_normalized LIKE $4)
               OR EXISTS (
                   SELECT 1
                   FROM track_artist_credits AS credits
                   WHERE credits.track_id = t.id
                     AND (
                         {CREDIT_NORMALIZED} = $1
                         OR {CREDIT_NORMALIZED} LIKE $2
                         OR string_to_array({CREDIT_NORMALIZED}, ' ') @> $3
                         OR (array_length($3, 1) > 1 AND {CREDIT_NORMALIZED} LIKE $4)
                     )
               )
            ORDER BY
                CASE
                    WHEN t.title_normalized = $1 THEN 0
                    WHEN t.title_normalized LIKE $2 THEN 1
                    WHEN string_to_array(t.title_normalized, ' ') @> $3 THEN 2
                    WHEN array_length($3, 1) > 1 AND t.title_normalized LIKE $4 THEN 3
                    ELSE 4
                END,
                EXISTS (
                    SELECT 1
                    FROM track_sources AS sources
                    WHERE sources.track_id = t.id AND sources.provider_id = $5
                ) DESC,
                t.title_normalized, t.id
            LIMIT $6
            "#
        ))
        .bind(&needle)
        .bind(&prefix)
        .bind(tokens.as_slice())
        .bind(&contains)
        .bind(LOCAL_PROVIDER_ID)
        .bind(i64::from(limit))
        .fetch_all(&self.pool)
        .await
        .map_err(repository_error)?;

        let mut hits = hits_from_rows(&self.pool, rows, user).await?;
        for hit in &mut hits {
            let quality = track_quality(&terms, hit.track().title(), hit.track().artists());
            *hit = hit.clone().with_match_quality(quality);
        }

        Ok(hits)
    }

    async fn search_albums(
        &self,
        query: &str,
        limit: u32,
        user: Option<Uuid>,
    ) -> Result<Vec<AlbumSummary>, TrackRepositoryError> {
        let terms = SearchTerms::new(query);

        if terms.is_empty() {
            return Ok(Vec::new());
        }

        let needle = terms.normalized().to_owned();
        let tokens = terms.tokens().to_vec();
        let prefix = format!("{}%", escape_like(&needle));
        let contains = format!("%{}%", escape_like(&needle));
        let rows = sqlx::query_as::<_, AlbumBrowseRow>(&format!(
            r#"
            SELECT {ALBUM_BROWSE_COLUMNS}
            {ALBUM_BROWSE_FROM}
            WHERE a.title_normalized = $1
               OR a.title_normalized LIKE $2
               OR string_to_array(a.title_normalized, ' ') @> $3
               OR (array_length($3, 1) > 1 AND a.title_normalized LIKE $4)
               OR EXISTS (
                   SELECT 1
                   FROM artists AS album_artists
                   WHERE album_artists.id = a.album_artist_id
                     AND (
                         album_artists.name_normalized = $1
                         OR album_artists.name_normalized LIKE $2
                         OR string_to_array(album_artists.name_normalized, ' ') @> $3
                         OR (array_length($3, 1) > 1 AND album_artists.name_normalized LIKE $4)
                     )
               )
            GROUP BY a.id, ar.id
            ORDER BY
                CASE
                    WHEN a.title_normalized = $1 THEN 0
                    WHEN a.title_normalized LIKE $2 THEN 1
                    WHEN string_to_array(a.title_normalized, ' ') @> $3 THEN 2
                    WHEN array_length($3, 1) > 1 AND a.title_normalized LIKE $4 THEN 3
                    ELSE 4
                END,
                a.title_normalized, a.id
            LIMIT $5
            "#
        ))
        .bind(&needle)
        .bind(&prefix)
        .bind(tokens.as_slice())
        .bind(&contains)
        .bind(i64::from(limit))
        .fetch_all(&self.pool)
        .await
        .map_err(repository_error)?;

        let mut summaries = rows
            .into_iter()
            .map(decode_album_summary)
            .collect::<Result<Vec<_>, _>>()?;
        for summary in &mut summaries {
            let quality = album_quality(
                &terms,
                summary.album().title(),
                summary.album_artist().map(Artist::name),
            );
            *summary = summary.clone().with_match_quality(quality);
        }
        rank_albums(&mut summaries);
        attach_album_states(&self.pool, user, &mut summaries).await?;

        Ok(summaries)
    }

    async fn search_artists(
        &self,
        query: &str,
        limit: u32,
        user: Option<Uuid>,
    ) -> Result<Vec<ArtistSummary>, TrackRepositoryError> {
        let terms = SearchTerms::new(query);

        if terms.is_empty() {
            return Ok(Vec::new());
        }

        let needle = terms.normalized().to_owned();
        let tokens = terms.tokens().to_vec();
        let prefix = format!("{}%", escape_like(&needle));
        let contains = format!("%{}%", escape_like(&needle));
        let rows = sqlx::query_as::<_, (Uuid, String, Option<String>, Option<String>, i64)>(
            r#"
            SELECT picked.id, picked.name, picked.sort_name, picked.musicbrainz_artist_id,
                   picked.album_count
            FROM (
                SELECT DISTINCT ON (ar.name_normalized)
                       ar.id, ar.name, ar.sort_name, ar.musicbrainz_artist_id,
                       ar.name_normalized,
                       (SELECT COUNT(*) FROM albums WHERE album_artist_id = ar.id)::BIGINT
                           AS album_count
                FROM artists AS ar
                WHERE ar.name_normalized = $1
                   OR ar.name_normalized LIKE $2
                   OR string_to_array(ar.name_normalized, ' ') @> $3
                   OR (array_length($3, 1) > 1 AND ar.name_normalized LIKE $4)
                ORDER BY ar.name_normalized,
                         (ar.musicbrainz_artist_id IS NULL),
                         album_count DESC,
                         ar.id
            ) AS picked
            ORDER BY
                CASE
                    WHEN picked.name_normalized = $1 THEN 0
                    WHEN picked.name_normalized LIKE $2 THEN 1
                    WHEN string_to_array(picked.name_normalized, ' ') @> $3 THEN 2
                    WHEN array_length($3, 1) > 1 AND picked.name_normalized LIKE $4 THEN 3
                    ELSE 4
                END,
                picked.name_normalized, picked.id
            LIMIT $5
            "#,
        )
        .bind(&needle)
        .bind(&prefix)
        .bind(tokens.as_slice())
        .bind(&contains)
        .bind(i64::from(limit))
        .fetch_all(&self.pool)
        .await
        .map_err(repository_error)?;

        let mut summaries = rows
            .into_iter()
            .map(decode_artist_summary)
            .collect::<Result<Vec<_>, _>>()?;
        for summary in &mut summaries {
            let quality = terms.classify(summary.artist().name());
            *summary = summary.clone().with_match_quality(quality);
        }
        rank_artists(&mut summaries);
        attach_artist_states(&self.pool, user, &mut summaries).await?;

        Ok(summaries)
    }

    async fn candidate_tracks(
        &self,
        titles: &[String],
        limit: u32,
    ) -> Result<Vec<TrackHit>, TrackRepositoryError> {
        let keys = titles
            .iter()
            .map(|title| normalize_for_matching(title))
            .filter(|key| !key.is_empty())
            .collect::<Vec<_>>();

        if keys.is_empty() {
            return Ok(Vec::new());
        }

        let rows = sqlx::query_as::<_, TrackSearchRow>(&format!(
            r#"
            SELECT t.id, t.title, t.duration_ms, t.musicbrainz_recording_id, t.isrc, t.genre,
                   {ALBUM_SEARCH_COLUMNS}
            FROM tracks AS t
            LEFT JOIN album_tracks AS mt ON mt.track_id = t.id
            LEFT JOIN albums AS a ON a.id = mt.album_id
            WHERE t.title_normalized = ANY($1)
            ORDER BY t.title_normalized, t.id
            LIMIT $2
            "#
        ))
        .bind(keys.as_slice())
        .bind(i64::from(limit))
        .fetch_all(&self.pool)
        .await
        .map_err(repository_error)?;

        hits_from_rows(&self.pool, rows, None).await
    }

    async fn dedup_candidates(
        &self,
        keys: &[String],
        limit: u32,
    ) -> Result<Vec<TrackHit>, TrackRepositoryError> {
        let keys = keys
            .iter()
            .map(|key| normalize_for_matching(key))
            .filter(|key| !key.is_empty())
            .collect::<Vec<_>>();

        if keys.is_empty() {
            return Ok(Vec::new());
        }

        let rows = sqlx::query_as::<_, TrackSearchRow>(&format!(
            r#"
            SELECT t.id, t.title, t.duration_ms, t.musicbrainz_recording_id, t.isrc, t.genre,
                   {ALBUM_SEARCH_COLUMNS}
            FROM tracks AS t
            LEFT JOIN album_tracks AS mt ON mt.track_id = t.id
            LEFT JOIN albums AS a ON a.id = mt.album_id
            WHERE t.id IN (
                SELECT DISTINCT matched.id
                FROM tracks AS matched
                JOIN unnest($1::text[]) AS k(value)
                  ON matched.title_normalized = k.value
                  OR matched.title_normalized % k.value
                WHERE EXISTS (
                    SELECT 1 FROM track_sources AS sources
                    WHERE sources.track_id = matched.id AND sources.provider_id = $2
                )
            )
            ORDER BY t.title_normalized, t.id
            LIMIT $3
            "#
        ))
        .bind(keys.as_slice())
        .bind(LOCAL_PROVIDER_ID)
        .bind(i64::from(limit))
        .fetch_all(&self.pool)
        .await
        .map_err(repository_error)?;

        hits_from_rows(&self.pool, rows, None).await
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
}

fn escape_like(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

fn rank_albums(summaries: &mut [AlbumSummary]) {
    summaries.sort_by(|left, right| {
        left.match_quality()
            .cmp(&right.match_quality())
            .then_with(|| {
                normalize_for_matching(left.album().title())
                    .cmp(&normalize_for_matching(right.album().title()))
            })
            .then_with(|| {
                left.album()
                    .id()
                    .to_string()
                    .cmp(&right.album().id().to_string())
            })
    });
}

fn rank_artists(summaries: &mut [ArtistSummary]) {
    summaries.sort_by(|left, right| {
        left.match_quality()
            .cmp(&right.match_quality())
            .then_with(|| {
                normalize_for_matching(left.artist().name())
                    .cmp(&normalize_for_matching(right.artist().name()))
            })
            .then_with(|| {
                left.artist()
                    .id()
                    .to_string()
                    .cmp(&right.artist().id().to_string())
            })
    });
}

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
