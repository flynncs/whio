use std::{collections::HashMap, sync::Arc};

use sqlx::{PgConnection, PgPool};

use async_trait::async_trait;
use uuid::Uuid;

use crate::features::music_library::{
    availability::ProviderRegistry,
    domain::{
        Album, AlbumEntry, AlbumId, AlbumListFilter, AlbumOrder, AlbumPage, AlbumSummary,
        AlbumTrack, Artist, ArtistCredit, ArtistId, ArtistListFilter, ArtistSort, ArtistSummary,
        FullTextSelector, GenreSummary, LOCAL_PROVIDER_ID, LibraryState, RandomSeed, Range,
        Resource, RoleSelector, RoleSet, SearchTerms, SortDirection, SourceIdentity,
        SourceRepresentationFacts, Track, TrackHit, TrackId, TrackListFilter, TrackListScope,
        TrackOrder, TrackPage, TrackView, YOUTUBE_MUSIC_PROVIDER_ID,
    },
    library::{LibraryRegistrationError, LibraryRegistry},
    storage::contracts::TrackRepositoryError,
};

use crate::features::music_library::storage::contracts::LibraryRead;

use super::PgStore;

pub(super) async fn dedup_candidate_ids(
    connection: &mut PgConnection,
    keys: &[String],
    limit: u32,
    library_provider_ids: &[String],
) -> Result<Vec<Uuid>, TrackRepositoryError> {
    if keys.is_empty() || library_provider_ids.is_empty() {
        return Ok(Vec::new());
    }

    sqlx::query_scalar::<_, Uuid>(&format!(
        r#"
        SELECT matched.id
        FROM tracks AS matched
        WHERE EXISTS (
            SELECT 1
            FROM unnest($1::TEXT[]) AS key(value)
            WHERE matched.title_normalized = {key}
               OR matched.title_normalized % {key}
        )
          AND EXISTS (
            SELECT 1
            FROM track_sources AS source
            WHERE source.track_id = matched.id
              AND source.provider_id = ANY($2::TEXT[])
        )
        ORDER BY matched.title_normalized, matched.id
        LIMIT $3
        "#,
        key = folded("key.value"),
    ))
    .bind(keys)
    .bind(library_provider_ids)
    .bind(i64::from(limit))
    .fetch_all(&mut *connection)
    .await
    .map_err(repository_error)
}

use super::PostgresLibraryStore;
use super::decode::{
    album_states, albums_for_tracks, artist_count_expressions, attach_album_state,
    attach_album_states, attach_artist_states, credits_for_tracks, decode_album,
    decode_album_summary, decode_artist, decode_artist_summary, decode_byte, decode_integer,
    decode_membership, decode_source, decode_track, genres_for_tracks, hits_from_rows,
    repository_error, sources_for_tracks, track_states,
};
use super::schema::{
    ALBUM_BROWSE_COLUMNS, ALBUM_BROWSE_FROM, ALBUM_COLUMNS, ARTIST_COLUMNS, AlbumBrowseRow,
    AlbumEntryRow, AlbumRow, ArtistRow, CREDIT_NORMALIZED, TRACK_COLUMNS, TRACK_ENGAGED,
    TRACK_OWNED, TrackPageRow, TrackRecord, TrackSearchRow, album_scope, artist_membership,
    artist_scope, folded, primary_album_order, search_folded,
};
use crate::features::music_library::domain::{
    album_quality, normalize_for_matching, track_quality,
};

#[async_trait]
impl LibraryRead for PgStore {
    async fn find_track(&self, track_id: &TrackId) -> Result<Option<Track>, TrackRepositoryError> {
        let row = sqlx::query_as::<_, TrackRecord>(&format!(
            "SELECT {TRACK_COLUMNS} FROM tracks AS t WHERE t.id = $1"
        ))
        .bind(track_id.as_uuid())
        .fetch_optional(&self.pool)
        .await
        .map_err(repository_error)?;

        let Some(record) = row else {
            return Ok(None);
        };

        let ids = [*track_id.as_uuid()];
        let mut credits = credits_for_tracks(&self.pool, &ids).await?;
        let mut genres = genres_for_tracks(&self.pool, &ids).await?;

        Ok(Some(decode_track(
            record,
            genres.remove(&ids[0]).unwrap_or_default(),
            credits.remove(&ids[0]).unwrap_or_default(),
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
            ORDER BY {order}
            LIMIT 1
            "#,
            order = primary_album_order("a", 2),
        ))
        .bind(track_id.as_uuid())
        .bind(self.library_provider_ids())
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
        let library_ids = self.library_provider_ids();
        let rows = sqlx::query_as::<_, AlbumEntryRow>(&format!(
            r#"
            SELECT {TRACK_COLUMNS},
                   mt.disc_number, mt.track_number, mt.position, mt.disc_total, mt.track_total
            FROM album_tracks AS mt
            JOIN tracks AS t ON t.id = mt.track_id
            WHERE mt.album_id = $1
            ORDER BY COALESCE(mt.disc_number, 1), mt.track_number NULLS LAST,
                     mt.position, mt.track_id
            "#
        ))
        .bind(album_id.as_uuid())
        .fetch_all(&self.pool)
        .await
        .map_err(repository_error)?;

        let track_ids = rows.iter().map(|row| row.track.id).collect::<Vec<_>>();
        let mut credits = credits_for_tracks(&self.pool, &track_ids).await?;
        let mut genres = genres_for_tracks(&self.pool, &track_ids).await?;
        let mut sources = sources_for_tracks(&self.pool, &track_ids).await?;
        let states = track_states(&self.pool, user, &track_ids).await?;
        let mut representations =
            selected_representations_for_tracks(&self.pool, &library_ids, &track_ids).await?;
        let mut entries = Vec::with_capacity(rows.len());

        for row in rows {
            let id = row.track.id;
            let track = decode_track(
                row.track,
                genres.remove(&id).unwrap_or_default(),
                credits.remove(&id).unwrap_or_default(),
            )?;
            let membership = decode_membership(
                row.disc_number,
                row.track_number,
                row.position,
                row.disc_total,
                row.track_total,
            )?;
            let state = states.get(&id).copied().unwrap_or_default();
            let sources = sources.remove(&id).unwrap_or_default();

            entries.push(
                AlbumEntry::new(track, membership)
                    .with_state(state)
                    .with_sources(sources)
                    .with_representation(representations.remove(&id)),
            );
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

    async fn page_artist_albums(
        &self,
        artist_id: &ArtistId,
        offset: u32,
        limit: u32,
        order: AlbumOrder,
        filter: &AlbumListFilter,
        user: Option<Uuid>,
    ) -> Result<AlbumPage, TrackRepositoryError> {
        let library_ids = self.library_provider_ids();
        let direction = filter.resolved_direction(order);
        let seed = filter
            .resolved_seed(order)
            .map_err(|_| TrackRepositoryError::InvalidData)?;
        let seed = match (order, seed) {
            (AlbumOrder::Random, Some(seed)) => Some(seed),
            (AlbumOrder::Random, None) => Some(RandomSeed::for_request(
                user,
                Resource::Album,
                chrono::Utc::now().date_naive(),
            )),
            _ => None,
        };
        let mut predicates = TrackPredicates::new(6);
        album_selector_clauses(&self.pool, filter, &mut predicates).await?;
        predicates.push("a.album_artist_id = $3".to_owned());
        let selectors = predicates.where_clause();
        let order_clause = album_order_clause(order, direction, seed, &mut predicates);
        let scope =
            if filter.scope() == crate::features::music_library::domain::AlbumListScope::Engaged {
                "TRUE".to_owned()
            } else {
                album_filter_scope(filter, 1, 2)
            };
        let statement = format!(
            r#"
            SELECT {ALBUM_BROWSE_COLUMNS}
            {ALBUM_BROWSE_FROM}
            WHERE {scope}
              {selectors}
            GROUP BY a.id, ar.id
            ORDER BY {order_clause}
            LIMIT $4 OFFSET $5
            "#
        );
        let mut query = sqlx::query_as::<_, AlbumBrowseRow>(&statement)
            .bind(user)
            .bind(&library_ids)
            .bind(artist_id.as_uuid())
            .bind(i64::from(limit))
            .bind(i64::from(offset));
        for value in predicates.values() {
            query = value.bind(query);
        }
        let rows = query
            .fetch_all(&self.pool)
            .await
            .map_err(repository_error)?;
        let mut albums = rows
            .into_iter()
            .map(decode_album_summary)
            .collect::<Result<Vec<_>, _>>()?;
        attach_album_states(&self.pool, user, &mut albums).await?;

        let mut count_predicates = TrackPredicates::new(4);
        album_selector_clauses(&self.pool, filter, &mut count_predicates).await?;
        count_predicates.push("a.album_artist_id = $3".to_owned());
        let count_scope = scope;
        let count_statement = format!(
            "SELECT COUNT(*)::BIGINT FROM albums AS a WHERE {count_scope} {}",
            count_predicates.where_clause()
        );
        let mut count_query = sqlx::query_as::<_, (i64,)>(&count_statement)
            .bind(user)
            .bind(&library_ids)
            .bind(artist_id.as_uuid());
        for value in count_predicates.values() {
            count_query = value.bind(count_query);
        }
        let (total,) = count_query
            .fetch_one(&self.pool)
            .await
            .map_err(repository_error)?;
        Ok(AlbumPage {
            albums,
            total: u32::try_from(total).map_err(|_| TrackRepositoryError::InvalidData)?,
        })
    }

    async fn list_albums(
        &self,
        offset: u32,
        limit: u32,
        order: AlbumOrder,
        filter: AlbumListFilter,
        user: Option<Uuid>,
    ) -> Result<Vec<AlbumSummary>, TrackRepositoryError> {
        let library_ids = self.library_provider_ids();
        let direction = filter.resolved_direction(order);
        let seed = match filter.resolved_seed(order) {
            Ok(seed) => seed,
            Err(_) => return Err(TrackRepositoryError::InvalidData),
        };
        let seed = match (order, seed) {
            (AlbumOrder::Random, Some(seed)) => Some(seed),
            (AlbumOrder::Random, None) => Some(RandomSeed::for_request(
                user,
                Resource::Album,
                chrono::Utc::now().date_naive(),
            )),
            _ => None,
        };
        let mut predicates = TrackPredicates::new(5);
        album_selector_clauses(&self.pool, &filter, &mut predicates).await?;
        let order_clause = album_order_clause(order, direction, seed, &mut predicates);
        let scope = album_filter_scope(&filter, 1, 2);
        let selectors = predicates.where_clause();
        let statement = format!(
            r#"
            SELECT {ALBUM_BROWSE_COLUMNS}
            {ALBUM_BROWSE_FROM}
            WHERE {scope}
              {selectors}
            GROUP BY a.id, ar.id
            ORDER BY {order_clause}
            LIMIT $3 OFFSET $4
            "#
        );
        let mut query = sqlx::query_as::<_, AlbumBrowseRow>(&statement)
            .bind(user)
            .bind(&library_ids)
            .bind(i64::from(limit))
            .bind(i64::from(offset));

        for value in predicates.values() {
            query = value.bind(query);
        }

        let rows = query
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
        sort: ArtistSort,
        filter: &ArtistListFilter,
        user: Option<Uuid>,
    ) -> Result<Vec<ArtistSummary>, TrackRepositoryError> {
        let library_ids = self.library_provider_ids();
        let direction = filter.resolved_direction(sort);
        let mut predicates = TrackPredicates::new(5);
        let selectors = artist_selector_clauses(&self.pool, filter, &mut predicates).await?;
        let order_clause = artist_order_clause(sort, direction, &selectors);
        let scope = artist_list_scope(filter);
        let where_clause = predicates.where_clause();
        let statement = format!(
            r#"
            SELECT ar.id, ar.name, ar.sort_name, ar.musicbrainz_artist_id,
                   {album_count} AS album_count, {song_count} AS song_count
            FROM artists AS ar
            WHERE {scope}
              {where_clause}
            ORDER BY {order_clause}
            LIMIT $3 OFFSET $4
            "#,
            album_count = selectors.album_count,
            song_count = selectors.song_count,
        );
        let mut query =
            sqlx::query_as::<_, (Uuid, String, Option<String>, Option<String>, i64, i64)>(
                &statement,
            )
            .bind(user)
            .bind(&library_ids)
            .bind(i64::from(limit))
            .bind(i64::from(offset));

        for value in predicates.values() {
            query = value.bind(query);
        }

        let rows = query
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

    async fn count_albums(
        &self,
        user: Option<Uuid>,
        filter: AlbumListFilter,
    ) -> Result<u32, TrackRepositoryError> {
        let library_ids = self.library_provider_ids();
        let mut predicates = TrackPredicates::new(3);
        album_selector_clauses(&self.pool, &filter, &mut predicates).await?;
        let scope = album_filter_scope(&filter, 1, 2);
        let selectors = predicates.where_clause();
        let statement =
            format!("SELECT COUNT(*)::BIGINT FROM albums AS a WHERE {scope} {selectors}");
        let mut query = sqlx::query_as::<_, (i64,)>(&statement)
            .bind(user)
            .bind(&library_ids);

        for value in predicates.values() {
            query = value.bind(query);
        }

        let (count,) = query
            .fetch_one(&self.pool)
            .await
            .map_err(repository_error)?;

        u32::try_from(count).map_err(|_| TrackRepositoryError::InvalidData)
    }

    async fn count_artists(
        &self,
        user: Option<Uuid>,
        filter: &ArtistListFilter,
    ) -> Result<u32, TrackRepositoryError> {
        let library_ids = self.library_provider_ids();
        let filter = if filter.name().is_some() {
            filter.clone()
        } else {
            filter.clone().with_provider_identity(false)
        };
        let mut predicates = TrackPredicates::new(3);
        artist_selector_clauses(&self.pool, &filter, &mut predicates).await?;
        let scope = artist_list_scope(&filter);
        let where_clause = predicates.where_clause();
        let statement =
            format!("SELECT COUNT(*)::BIGINT FROM artists AS ar WHERE {scope} {where_clause}");
        let mut query = sqlx::query_as::<_, (i64,)>(&statement)
            .bind(user)
            .bind(&library_ids);

        for value in predicates.values() {
            query = value.bind(query);
        }

        let (count,) = query
            .fetch_one(&self.pool)
            .await
            .map_err(repository_error)?;

        u32::try_from(count).map_err(|_| TrackRepositoryError::InvalidData)
    }

    async fn page_tracks(
        &self,
        offset: u32,
        limit: u32,
        order: TrackOrder,
        filter: &TrackListFilter,
        user: Option<Uuid>,
    ) -> Result<TrackPage, TrackRepositoryError> {
        self.track_page(TrackPageRequest {
            offset,
            limit,
            order,
            filter,
            user,
            album: None,
        })
        .await
    }

    async fn page_album_tracks(
        &self,
        album_id: &AlbumId,
        offset: u32,
        limit: u32,
        order: TrackOrder,
        filter: &TrackListFilter,
        user: Option<Uuid>,
    ) -> Result<TrackPage, TrackRepositoryError> {
        self.track_page(TrackPageRequest {
            offset,
            limit,
            order,
            filter,
            user,
            album: Some(album_id),
        })
        .await
    }

    async fn track_views(
        &self,
        track_ids: &[TrackId],
        user: Option<Uuid>,
    ) -> Result<Vec<TrackView>, TrackRepositoryError> {
        if track_ids.is_empty() {
            return Ok(Vec::new());
        }

        let library_ids = self.library_provider_ids();
        let mut ids = track_ids
            .iter()
            .map(|track_id| *track_id.as_uuid())
            .collect::<Vec<_>>();
        ids.sort_unstable();
        ids.dedup();
        let rows = sqlx::query_as::<_, TrackRecord>(&format!(
            "SELECT {TRACK_COLUMNS} FROM tracks AS t WHERE t.id = ANY($1)"
        ))
        .bind(&ids)
        .fetch_all(&self.pool)
        .await
        .map_err(repository_error)?;

        let credits = credits_for_tracks(&self.pool, &ids).await?;
        let genres = genres_for_tracks(&self.pool, &ids).await?;
        let sources = sources_for_tracks(&self.pool, &ids).await?;
        let states = track_states(&self.pool, user, &ids).await?;
        let albums = albums_for_tracks(&self.pool, &ids, None, &library_ids).await?;
        let representations =
            selected_representations_for_tracks(&self.pool, &library_ids, &ids).await?;
        let views = assemble_track_views(
            rows,
            credits,
            genres,
            sources,
            states,
            albums,
            representations,
        )?;
        let by_id = views
            .into_iter()
            .map(|view| (*view.track().id().as_uuid(), view))
            .collect::<HashMap<_, _>>();

        Ok(track_ids
            .iter()
            .filter_map(|track_id| by_id.get(track_id.as_uuid()).cloned())
            .collect())
    }

    async fn tracks_credited_to(
        &self,
        artist_ids: &[ArtistId],
    ) -> Result<Vec<TrackId>, TrackRepositoryError> {
        if artist_ids.is_empty() {
            return Ok(Vec::new());
        }

        let ids = artist_ids
            .iter()
            .map(|artist_id| *artist_id.as_uuid())
            .collect::<Vec<_>>();
        let rows = sqlx::query_scalar::<_, Uuid>(
            r#"
            SELECT DISTINCT credits.track_id
            FROM track_artist_credits AS credits
            WHERE credits.artist_id = ANY($1)
            ORDER BY credits.track_id
            "#,
        )
        .bind(&ids)
        .fetch_all(&self.pool)
        .await
        .map_err(repository_error)?;

        Ok(rows.into_iter().map(TrackId::from_uuid).collect())
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

        let title = SearchMatch::new("t.title_normalized", "$1");
        let credit = SearchMatch::new(CREDIT_NORMALIZED, "$1");
        let rows = sqlx::query_as::<_, TrackSearchRow>(&format!(
            r#"
            SELECT {TRACK_COLUMNS}
            FROM tracks AS t
            WHERE {title_matches}
               OR EXISTS (
                   SELECT 1
                   FROM track_artist_credits AS credits
                   WHERE credits.track_id = t.id
                     AND {credit_matches}
               )
            ORDER BY
                {title_rank},
                EXISTS (
                    SELECT 1
                    FROM track_sources AS sources
                    WHERE sources.track_id = t.id AND sources.provider_id = $2
                ) DESC,
                t.title_normalized, t.id
            LIMIT $3
            "#,
            title_matches = title.any(),
            credit_matches = credit.any(),
            title_rank = title.rank(),
        ))
        .bind(query.trim())
        .bind(LOCAL_PROVIDER_ID)
        .bind(i64::from(limit))
        .fetch_all(&self.pool)
        .await
        .map_err(repository_error)?;

        let mut hits = hits_from_rows(&self.pool, rows, user, &self.library_provider_ids()).await?;
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

        let title = SearchMatch::new("a.title_normalized", "$1");
        let artist = SearchMatch::new("album_artists.name_normalized", "$1");
        let rows = sqlx::query_as::<_, AlbumBrowseRow>(&format!(
            r#"
            SELECT {ALBUM_BROWSE_COLUMNS}
            {ALBUM_BROWSE_FROM}
            WHERE {title_matches}
               OR EXISTS (
                   SELECT 1
                   FROM artists AS album_artists
                   WHERE album_artists.id = a.album_artist_id
                     AND {artist_matches}
               )
            GROUP BY a.id, ar.id
            ORDER BY
                {title_rank},
                a.title_normalized, a.id
            LIMIT $2
            "#,
            title_matches = title.any(),
            artist_matches = artist.any(),
            title_rank = title.rank(),
        ))
        .bind(query.trim())
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
        filter: &ArtistListFilter,
    ) -> Result<Vec<ArtistSummary>, TrackRepositoryError> {
        let terms = SearchTerms::new(query);

        if terms.is_empty() {
            return Ok(Vec::new());
        }

        let name = SearchMatch::new("ar.name_normalized", "$1");
        let ranked = SearchMatch::new("picked.name_normalized", "$1");
        let mut predicates = TrackPredicates::new(3);
        let selectors = artist_membership_selectors(filter, &mut predicates);
        let membership = predicates.where_clause();
        let statement = format!(
            r#"
            SELECT picked.id, picked.name, picked.sort_name, picked.musicbrainz_artist_id,
                   picked.album_count, picked.song_count
            FROM (
                SELECT DISTINCT ON (ar.name_normalized)
                       ar.id, ar.name, ar.sort_name, ar.musicbrainz_artist_id,
                       ar.name_normalized,
                       {album_count} AS album_count,
                       {song_count} AS song_count
                FROM artists AS ar
                WHERE {name_matches}
                  {membership}
                ORDER BY ar.name_normalized,
                         (ar.musicbrainz_artist_id IS NULL),
                         album_count DESC,
                         ar.id
            ) AS picked
            ORDER BY
                {ranked_rank},
                picked.name_normalized, picked.id
            LIMIT $2
            "#,
            album_count = selectors.album_count,
            song_count = selectors.song_count,
            name_matches = name.any(),
            ranked_rank = ranked.rank(),
        );
        let mut query =
            sqlx::query_as::<_, (Uuid, String, Option<String>, Option<String>, i64, i64)>(
                &statement,
            )
            .bind(query.trim())
            .bind(i64::from(limit));

        for value in predicates.values() {
            query = value.bind(query);
        }

        let rows = query
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

    async fn find_artist_by_exact_name(
        &self,
        name: &str,
    ) -> Result<Option<ArtistId>, TrackRepositoryError> {
        let ids = sqlx::query_scalar::<_, Uuid>(&format!(
            r#"
            SELECT id
            FROM artists
            WHERE name_normalized = {key}
            LIMIT 2
            "#,
            key = folded("$1"),
        ))
        .bind(name)
        .fetch_all(&self.pool)
        .await
        .map_err(repository_error)?;

        match ids.as_slice() {
            [only] => Ok(Some(ArtistId::from_uuid(*only))),
            _ => Ok(None),
        }
    }

    async fn candidate_tracks(
        &self,
        titles: &[String],
        limit: u32,
    ) -> Result<Vec<TrackHit>, TrackRepositoryError> {
        if titles.is_empty() {
            return Ok(Vec::new());
        }

        let rows = sqlx::query_as::<_, TrackSearchRow>(&format!(
            r#"
            SELECT {TRACK_COLUMNS}
            FROM tracks AS t
            WHERE t.title_normalized = ANY(
                SELECT {key} FROM unnest($1::TEXT[]) AS raw(value)
            )
            ORDER BY t.title_normalized, t.id
            LIMIT $2
            "#,
            key = folded("raw.value"),
        ))
        .bind(titles)
        .bind(i64::from(limit))
        .fetch_all(&self.pool)
        .await
        .map_err(repository_error)?;

        hits_from_rows(&self.pool, rows, None, &self.library_provider_ids()).await
    }

    async fn dedup_candidates(
        &self,
        keys: &[String],
        limit: u32,
    ) -> Result<Vec<TrackHit>, TrackRepositoryError> {
        if keys.is_empty() {
            return Ok(Vec::new());
        }

        let mut connection = self.pool.acquire().await.map_err(repository_error)?;
        let candidate_ids =
            dedup_candidate_ids(&mut connection, keys, limit, &self.library_provider_ids()).await?;
        if candidate_ids.is_empty() {
            return Ok(Vec::new());
        }

        let rows = sqlx::query_as::<_, TrackSearchRow>(&format!(
            r#"
            SELECT {TRACK_COLUMNS}
            FROM tracks AS t
            WHERE t.id = ANY($1)
            ORDER BY t.title_normalized, t.id
            "#,
        ))
        .bind(candidate_ids)
        .fetch_all(&mut *connection)
        .await
        .map_err(repository_error)?;
        drop(connection);

        hits_from_rows(&self.pool, rows, None, &self.library_provider_ids()).await
    }

    async fn list_genres(&self) -> Result<Vec<GenreSummary>, TrackRepositoryError> {
        let rows = sqlx::query_as::<_, (String, i64, i64)>(
            r#"
            SELECT MIN(genre.name), COUNT(DISTINCT genre.track_id)::BIGINT,
                   COUNT(DISTINCT mt.album_id)::BIGINT
            FROM track_genres AS genre
            LEFT JOIN album_tracks AS mt ON mt.track_id = genre.track_id
            GROUP BY genre.name_normalized
            ORDER BY genre.name_normalized, MIN(genre.name)
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

struct TrackPageRequest<'a> {
    offset: u32,
    limit: u32,
    order: TrackOrder,
    filter: &'a TrackListFilter,
    user: Option<Uuid>,
    album: Option<&'a AlbumId>,
}

impl PgStore {
    async fn track_page(
        &self,
        request: TrackPageRequest<'_>,
    ) -> Result<TrackPage, TrackRepositoryError> {
        let TrackPageRequest {
            offset,
            limit,
            order,
            filter,
            user,
            album,
        } = request;
        let library_ids = self.library_provider_ids();
        let direction = filter.resolved_direction(order);
        let seed = match filter.resolved_seed(order) {
            Ok(seed) => seed,
            Err(_) => return Err(TrackRepositoryError::InvalidData),
        };
        let seed = match (order, seed) {
            (TrackOrder::Random, Some(seed)) => Some(seed),
            (TrackOrder::Random, None) => Some(RandomSeed::for_request(
                user,
                Resource::Track,
                chrono::Utc::now().date_naive(),
            )),
            _ => None,
        };
        let mut predicates = TrackPredicates::new(5);
        let album_placeholder = album.map(|album| predicates.uuids(vec![*album.as_uuid()]));
        let membership = album_placeholder.as_deref();
        track_selector_clauses(&self.pool, filter, membership, &mut predicates).await?;
        let order_clause = track_order_clause(order, direction, seed, membership, &mut predicates);
        let scope = match album_placeholder.as_deref() {
            Some(album) => format!(
                "EXISTS (SELECT 1 FROM album_tracks AS mt \
                 WHERE mt.track_id = t.id AND mt.album_id = ANY({album}))"
            ),
            None => match filter.scope() {
                TrackListScope::Engaged
                    if filter.provider_discovered() && filter.title().is_some() =>
                {
                    format!(
                        "({TRACK_ENGAGED} OR EXISTS (SELECT 1 FROM track_sources AS discovered_track \
                     WHERE discovered_track.track_id = t.id \
                       AND discovered_track.provider_id <> ALL($2::TEXT[])))"
                    )
                }
                TrackListScope::Engaged => TRACK_ENGAGED.to_owned(),
                TrackListScope::Owned => TRACK_OWNED.to_owned(),
            },
        };
        let selectors = predicates.where_clause();
        let statement = format!(
            r#"
            SELECT page.id, page.title, page.duration_ms, page.musicbrainz_recording_id,
                   page.isrc, page.recording_date, page.recording_date_precision,
                   page.original_date, page.original_date_precision, totals.n AS total
            FROM (
                SELECT COUNT(*)::BIGINT AS n
                FROM tracks AS t
                WHERE {scope}
                  {selectors}
            ) AS totals
            LEFT JOIN LATERAL (
                SELECT {TRACK_COLUMNS}
                FROM tracks AS t
                WHERE {scope}
                  {selectors}
                ORDER BY {order_clause}
                LIMIT $3 OFFSET $4
            ) AS page ON TRUE
            "#
        );
        let mut query = sqlx::query_as::<_, TrackPageRow>(&statement)
            .bind(user)
            .bind(&library_ids)
            .bind(i64::from(limit))
            .bind(i64::from(offset));

        for value in predicates.values() {
            query = value.bind(query);
        }

        let rows = query
            .fetch_all(&self.pool)
            .await
            .map_err(repository_error)?;

        let Some(first) = rows.first() else {
            return Err(TrackRepositoryError::InvalidData);
        };
        let total = u32::try_from(first.total).map_err(|_| TrackRepositoryError::InvalidData)?;

        if rows.len() == 1 && first.id.is_none() {
            return Ok(TrackPage::new(Vec::new(), total));
        }

        let ids = rows.iter().filter_map(|row| row.id).collect::<Vec<_>>();
        let credits = credits_for_tracks(&self.pool, &ids).await?;
        let genres = genres_for_tracks(&self.pool, &ids).await?;
        let sources = sources_for_tracks(&self.pool, &ids).await?;
        let states = track_states(&self.pool, user, &ids).await?;
        let albums = albums_for_tracks(&self.pool, &ids, album, &library_ids).await?;
        let representations =
            selected_representations_for_tracks(&self.pool, &library_ids, &ids).await?;
        let records = rows
            .into_iter()
            .map(|row| {
                let (Some(id), Some(title)) = (row.id, row.title) else {
                    return Err(TrackRepositoryError::InvalidData);
                };

                Ok(TrackRecord {
                    id,
                    title,
                    duration_ms: row.duration_ms,
                    musicbrainz_recording_id: row.musicbrainz_recording_id,
                    isrc: row.isrc,
                    recording_date: row.recording_date,
                    recording_date_precision: row.recording_date_precision,
                    original_date: row.original_date,
                    original_date_precision: row.original_date_precision,
                })
            })
            .collect::<Result<Vec<_>, TrackRepositoryError>>()?;
        let items = assemble_track_views(
            records,
            credits,
            genres,
            sources,
            states,
            albums,
            representations,
        )?;

        Ok(TrackPage::new(items, total))
    }
}

fn assemble_track_views(
    rows: Vec<TrackRecord>,
    mut credits: HashMap<Uuid, Vec<ArtistCredit>>,
    mut genres: HashMap<Uuid, Vec<String>>,
    mut sources: HashMap<Uuid, Vec<SourceIdentity>>,
    states: HashMap<Uuid, LibraryState>,
    mut albums: HashMap<Uuid, (Album, AlbumTrack, Option<String>)>,
    mut representations: HashMap<Uuid, SourceRepresentationFacts>,
) -> Result<Vec<TrackView>, TrackRepositoryError> {
    let mut views = Vec::with_capacity(rows.len());

    for row in rows {
        let id = row.id;
        let track = decode_track(
            row,
            genres.remove(&id).unwrap_or_default(),
            credits.remove(&id).unwrap_or_default(),
        )?;
        let (album, membership, album_artist) = match albums.remove(&id) {
            Some((album, membership, album_artist)) => {
                (Some(album), Some(membership), album_artist)
            }
            None => (None, None, None),
        };
        let state = states.get(&id).copied().unwrap_or_default();
        let sources = sources.remove(&id).unwrap_or_default();
        let representation = representations.remove(&id);

        views.push(
            TrackView::new(track, album, membership, album_artist, state, sources)
                .with_representation(representation),
        );
    }

    Ok(views)
}

#[derive(sqlx::FromRow)]
struct SelectedRepresentationRow {
    track_id: Uuid,
    provider_id: String,
    source_scope: String,
    external_id: String,
    content_type: Option<String>,
    suffix: Option<String>,
    container: Option<String>,
    codec: Option<String>,
    size_bytes: Option<i64>,
    bitrate_kbps: Option<i32>,
    sample_rate_hz: Option<i32>,
    bit_depth: Option<i16>,
    channels: Option<i16>,
}

async fn selected_representations_for_tracks(
    pool: &PgPool,
    library_ids: &[String],
    track_ids: &[Uuid],
) -> Result<HashMap<Uuid, SourceRepresentationFacts>, TrackRepositoryError> {
    if track_ids.is_empty() {
        return Ok(HashMap::new());
    }

    let rows = sqlx::query_as::<_, SelectedRepresentationRow>(&format!(
        r#"
        SELECT DISTINCT ON (rep.track_id)
               rep.track_id, rep.provider_id, rep.source_scope, rep.external_id,
               rep.content_type, rep.suffix, rep.container, rep.codec,
               rep.size_bytes, rep.bitrate_kbps, rep.sample_rate_hz,
               rep.bit_depth, rep.channels
        FROM track_sources AS rep
        WHERE rep.track_id = ANY($1)
          AND rep.provider_id = ANY($2::TEXT[])
        ORDER BY rep.track_id, {key}
        "#,
        key = source_selection_key("rep"),
    ))
    .bind(track_ids)
    .bind(library_ids)
    .fetch_all(pool)
    .await
    .map_err(repository_error)?;

    rows.into_iter()
        .map(|row| {
            let size_bytes = row
                .size_bytes
                .map(u64::try_from)
                .transpose()
                .map_err(|_| TrackRepositoryError::InvalidData)?;
            let facts = SourceRepresentationFacts {
                source: decode_source(row.provider_id, row.source_scope, row.external_id)?,
                content_type: row.content_type,
                suffix: row.suffix,
                container: row.container,
                codec: row.codec,
                size_bytes,
                bitrate_kbps: decode_integer(row.bitrate_kbps)?,
                sample_rate_hz: decode_integer(row.sample_rate_hz)?,
                bit_depth: decode_byte(row.bit_depth)?,
                channels: decode_byte(row.channels)?,
            };

            Ok((row.track_id, facts))
        })
        .collect()
}

struct SearchMatch {
    exact: String,
    prefix: String,
    words: String,
    contains: String,
}

impl SearchMatch {
    fn new(column: &str, query: &str) -> Self {
        let column = search_folded(column);
        let key = search_folded(query);
        let pattern =
            format!("replace(replace(replace({key}, '\\', '\\\\'), '%', '\\%'), '_', '\\_')");
        let query_words = format!("string_to_array({key}, ' ')");

        Self {
            exact: format!("{column} = {key}"),
            prefix: format!("{column} LIKE {pattern} || '%'"),
            words: format!("string_to_array({column}, ' ') @> {query_words}"),
            contains: format!(
                "array_length({query_words}, 1) > 1 AND {column} LIKE '%' || {pattern} || '%'"
            ),
        }
    }

    fn any(&self) -> String {
        format!(
            "({} OR {} OR {} OR {})",
            self.exact, self.prefix, self.words, self.contains
        )
    }

    fn rank(&self) -> String {
        format!(
            "CASE WHEN {} THEN 0 WHEN {} THEN 1 WHEN {} THEN 2 WHEN {} THEN 3 ELSE 4 END",
            self.exact, self.prefix, self.words, self.contains
        )
    }
}

fn full_text_clause(
    selector: &FullTextSelector,
    predicates: &mut TrackPredicates,
    fields: impl Fn(&str) -> Vec<String>,
) -> String {
    selector
        .terms()
        .iter()
        .map(|term| {
            let pattern = predicates.text(format!("%{}%", escape_like(term)));
            format!("({})", fields(&pattern).join(" OR "))
        })
        .collect::<Vec<_>>()
        .join(" AND ")
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

fn source_precedence(alias: &str) -> String {
    format!(
        "(CASE {alias}.provider_id WHEN '{LOCAL_PROVIDER_ID}' THEN 0 \
         WHEN '{YOUTUBE_MUSIC_PROVIDER_ID}' THEN 1 ELSE 255 END)"
    )
}

fn album_membership_restriction(album: Option<&str>) -> String {
    album.map_or_else(String::new, |album| {
        format!(" AND member.album_id = ANY({album})")
    })
}

fn primary_membership(album: Option<&str>, fact: &str) -> String {
    let restriction = if album.is_some() {
        album_membership_restriction(album)
    } else {
        " AND NOT EXISTS (SELECT 1 FROM album_tracks AS nearer \
         JOIN albums AS nearer_album ON nearer_album.id = nearer.album_id \
         WHERE nearer.track_id = t.id \
           AND (nearer_album.title_normalized, nearer_album.id) \
               < (member_album.title_normalized, member_album.id))"
            .to_owned()
    };

    format!(
        "EXISTS (SELECT 1 FROM album_tracks AS member \
         JOIN albums AS member_album ON member_album.id = member.album_id \
         WHERE member.track_id = t.id AND ({fact}){restriction})"
    )
}

fn primary_membership_row(album: Option<&str>, column: &str) -> String {
    format!(
        "(SELECT {column} FROM album_tracks AS member \
         JOIN albums AS member_album ON member_album.id = member.album_id \
         WHERE member.track_id = t.id{} \
         ORDER BY member_album.title_normalized, member_album.id, member.position \
         LIMIT 1)",
        album_membership_restriction(album)
    )
}

fn source_selection_key(alias: &str) -> String {
    format!(
        "({}, {alias}.external_id, {alias}.source_scope)",
        source_precedence(alias)
    )
}

fn selected_representation(alias: &str) -> String {
    format!(
        "{alias}.track_id = t.id AND {alias}.provider_id = ANY($2::TEXT[]) \
         AND NOT EXISTS (SELECT 1 FROM track_sources AS nearer \
                         WHERE nearer.track_id = t.id \
                           AND nearer.provider_id = ANY($2::TEXT[]) \
                           AND {nearer} < {selected})",
        nearer = source_selection_key("nearer"),
        selected = source_selection_key(alias),
    )
}

fn selected_representation_scalar(column: &str) -> String {
    format!(
        "(SELECT {column} FROM track_sources AS rep WHERE {} LIMIT 1)",
        selected_representation("rep")
    )
}

fn role_clause(
    role: RoleSelector,
    artists: Option<&str>,
    album: Option<&str>,
    predicates: &mut TrackPredicates,
) -> String {
    let artist_term = artists.map_or_else(String::new, |placeholder| {
        format!(" AND sel_role.artist_id = ANY({placeholder})")
    });

    match role.stored_role() {
        Some(stored) => {
            let placeholder = predicates.text(stored.as_str().to_owned());

            format!(
                "EXISTS (SELECT 1 FROM track_artist_credits AS sel_role \
                 WHERE sel_role.track_id = t.id AND sel_role.role = {placeholder}{artist_term})"
            )
        }
        None if role == RoleSelector::Artist => format!(
            "EXISTS (SELECT 1 FROM track_artist_credits AS sel_role \
             WHERE sel_role.track_id = t.id AND sel_role.role IS NULL{artist_term})"
        ),
        None => {
            let album_artist_term = artists.map_or_else(
                || " AND sel_album_artist_album.album_artist_id IS NOT NULL".to_owned(),
                |placeholder| {
                    format!(" AND sel_album_artist_album.album_artist_id = ANY({placeholder})")
                },
            );
            let album_membership = album.map_or_else(String::new, |placeholder| {
                format!(" AND sel_album_artist_member.album_id = ANY({placeholder})")
            });

            format!(
                "EXISTS (SELECT 1 FROM album_tracks AS sel_album_artist_member \
                 JOIN albums AS sel_album_artist_album \
                   ON sel_album_artist_album.id = sel_album_artist_member.album_id \
                 WHERE sel_album_artist_member.track_id = t.id{album_artist_term}{album_membership})"
            )
        }
    }
}

fn library_selector_error(error: LibraryRegistrationError) -> TrackRepositoryError {
    match error {
        LibraryRegistrationError::UnknownLibrary { id } => {
            TrackRepositoryError::UnknownLibraryId { id }
        }
        LibraryRegistrationError::Store(_)
        | LibraryRegistrationError::Disabled
        | LibraryRegistrationError::UnknownLabel { .. }
        | LibraryRegistrationError::UnknownNumericId { .. } => TrackRepositoryError::InvalidData,
    }
}

async fn track_selector_clauses(
    pool: &PgPool,
    filter: &TrackListFilter,
    album: Option<&str>,
    predicates: &mut TrackPredicates,
) -> Result<(), TrackRepositoryError> {
    if let Some(title) = filter.title() {
        let clause = full_text_clause(title, predicates, |pattern| {
            vec![
                format!("{} LIKE {pattern}", search_folded("t.title_normalized")),
                format!(
                    "EXISTS (SELECT 1 FROM album_tracks AS text_member \
                    JOIN albums AS text_album ON text_album.id = text_member.album_id \
                    WHERE text_member.track_id = t.id AND {} LIKE {pattern})",
                    search_folded("text_album.title_normalized")
                ),
                format!(
                    "EXISTS (SELECT 1 FROM track_artist_credits AS text_credit \
                    JOIN artists AS text_artist ON text_artist.id = text_credit.artist_id \
                    WHERE text_credit.track_id = t.id AND {} LIKE {pattern})",
                    search_folded("text_artist.name_normalized")
                ),
                format!(
                    "EXISTS (SELECT 1 FROM album_tracks AS text_member \
                    JOIN albums AS text_album ON text_album.id = text_member.album_id \
                    JOIN artists AS text_album_artist ON text_album_artist.id = text_album.album_artist_id \
                    WHERE text_member.track_id = t.id AND {} LIKE {pattern})",
                    search_folded("text_album_artist.name_normalized")
                ),
            ]
        });
        predicates.push(clause);
    }

    if let Some(album_id) = filter.album_id() {
        let ids = album_id.iter().map(|id| *id.as_uuid()).collect::<Vec<_>>();
        let placeholder = predicates.uuids(ids);

        predicates.push(format!(
            "EXISTS (SELECT 1 FROM album_tracks AS sel_album \
             WHERE sel_album.track_id = t.id AND sel_album.album_id = ANY({placeholder}))"
        ));
    }

    let artists = filter.artists_id().map(|artists_id| {
        let ids = artists_id
            .iter()
            .map(|id| *id.as_uuid())
            .collect::<Vec<_>>();

        predicates.uuids(ids)
    });

    if let Some(album_artist_id) = filter.album_artist_id() {
        let ids = album_artist_id
            .iter()
            .map(|id| *id.as_uuid())
            .collect::<Vec<_>>();
        let placeholder = predicates.uuids(ids);

        predicates.push(primary_membership(
            album,
            &format!("member_album.album_artist_id = ANY({placeholder})"),
        ));
    }

    if let Some(roles) = filter.role() {
        let mut terms = Vec::new();

        for role in roles.iter() {
            terms.push(role_clause(*role, artists.as_deref(), album, predicates));
        }

        predicates.push(format!("({})", terms.join(" OR ")));
    } else if let Some(placeholder) = artists.as_deref() {
        predicates.push(format!(
            "EXISTS (SELECT 1 FROM track_artist_credits AS sel_credit \
             WHERE sel_credit.track_id = t.id AND sel_credit.artist_id = ANY({placeholder}))"
        ));
    }

    if let Some(genre) = filter.genre() {
        let names = genre
            .iter()
            .map(|name| name.as_str().to_owned())
            .collect::<Vec<_>>();
        let placeholder = predicates.texts(names);

        predicates.push(format!(
            "EXISTS (SELECT 1 FROM track_genres AS sel_genre \
             WHERE sel_genre.track_id = t.id AND sel_genre.name_normalized = ANY({placeholder}))"
        ));
    }

    if let Some(year) = filter.year() {
        let range = predicates.i32_range("member_album.year", year);

        predicates.push(primary_membership(album, &range));
    }

    if let Some(compilation) = filter.compilation() {
        let placeholder = predicates.boolean(compilation);

        predicates.push(primary_membership(
            album,
            &format!("member_album.compilation = {placeholder}"),
        ));
    }

    if let Some(starred) = filter.starred() {
        let clause = "EXISTS (SELECT 1 FROM track_favorites AS sel_favorite \
                      WHERE sel_favorite.track_id = t.id AND sel_favorite.user_id = $1)";

        predicates.push(if starred {
            clause.to_owned()
        } else {
            format!("NOT {clause}")
        });
    }

    if let Some(has_rating) = filter.has_rating() {
        let clause = "EXISTS (SELECT 1 FROM track_ratings AS sel_rating \
                      WHERE sel_rating.track_id = t.id AND sel_rating.user_id = $1)";

        predicates.push(if has_rating {
            clause.to_owned()
        } else {
            format!("NOT {clause}")
        });
    }

    if let Some(rating) = filter.rating() {
        let range = predicates.u8_range("sel_rating.rating", rating);

        predicates.push(format!(
            "EXISTS (SELECT 1 FROM track_ratings AS sel_rating \
             WHERE sel_rating.track_id = t.id AND sel_rating.user_id = $1 AND {range})"
        ));
    }

    if let Some(play_count) = filter.play_count() {
        let clause = predicates.u32_range(
            "COALESCE((SELECT sel_plays.play_count FROM track_play_stats AS sel_plays \
              WHERE sel_plays.track_id = t.id AND sel_plays.user_id = $1), 0)",
            play_count,
        );

        predicates.push(clause);
    }

    if let Some(recently_played) = filter.recently_played() {
        let clause = "EXISTS (SELECT 1 FROM track_play_stats AS sel_played \
                      WHERE sel_played.track_id = t.id AND sel_played.user_id = $1 \
                        AND sel_played.last_played_at IS NOT NULL)";

        predicates.push(if recently_played {
            clause.to_owned()
        } else {
            format!("NOT {clause}")
        });
    }

    if let Some(date_added) = filter.date_added() {
        let clause = predicates.time_range(
            "(SELECT MIN(sel_added.created_at) FROM track_sources AS sel_added \
              WHERE sel_added.track_id = t.id AND sel_added.provider_id = ANY($2::TEXT[]))",
            date_added,
        );

        predicates.push(clause);
    }

    if let Some(missing) = filter.missing() {
        let clause = "EXISTS (SELECT 1 FROM track_sources AS sel_missing \
                      WHERE sel_missing.track_id = t.id \
                        AND sel_missing.provider_id = ANY($2::TEXT[]) \
                        AND sel_missing.missing_at IS NOT NULL)";

        predicates.push(if missing {
            clause.to_owned()
        } else {
            format!("NOT {clause}")
        });
    }

    if let Some(path) = filter.path() {
        let pattern = predicates.text(format!("%{}%", escape_like(path.as_str())));

        predicates.push(format!(
            "EXISTS (SELECT 1 FROM track_sources AS sel_path \
             WHERE sel_path.track_id = t.id AND sel_path.provider_id = ANY($2::TEXT[]) \
               AND sel_path.path IS NOT NULL AND {} LIKE {pattern})",
            folded("sel_path.path")
        ));
    }

    if let Some(duration) = filter.duration() {
        let clause = predicates.u64_range("t.duration_ms", duration);

        predicates.push(clause);
    }

    for (selector, column) in [
        (filter.track_number(), "member.track_number"),
        (filter.disc_number(), "member.disc_number"),
        (filter.disc_total(), "member.disc_total"),
        (filter.track_total(), "member.track_total"),
    ] {
        if let Some(range) = selector {
            let range = predicates.u16_range(column, range);

            predicates.push(primary_membership(album, &range));
        }
    }

    for (selector, column) in [
        (filter.bitrate(), "rep.bitrate_kbps"),
        (filter.bitdepth(), "rep.bit_depth"),
        (filter.samplerate(), "rep.sample_rate_hz"),
        (filter.size(), "rep.size_bytes"),
    ] {
        if let Some(range) = selector {
            let range = predicates.i64_range(column, range);

            predicates.push(format!(
                "EXISTS (SELECT 1 FROM track_sources AS rep WHERE {} AND {range})",
                selected_representation("rep")
            ));
        }
    }

    if let Some(channels) = filter.channels() {
        let range = predicates.u16_range("rep.channels", channels);

        predicates.push(format!(
            "EXISTS (SELECT 1 FROM track_sources AS rep WHERE {} AND {range})",
            selected_representation("rep")
        ));
    }

    if let Some(codec) = filter.codec() {
        let placeholder = predicates.text(codec.as_str().to_owned());

        predicates.push(format!(
            "EXISTS (SELECT 1 FROM track_sources AS rep WHERE {} AND {} = {placeholder})",
            selected_representation("rep"),
            folded("rep.codec")
        ));
    }

    if let Some(filetype) = filter.filetype() {
        let placeholder = predicates.text(filetype.as_str().to_owned());

        predicates.push(format!(
            "EXISTS (SELECT 1 FROM track_sources AS rep WHERE {} AND {} = {placeholder})",
            selected_representation("rep"),
            folded("rep.container")
        ));
    }

    if let Some(library_id) = filter.library_id() {
        let ids = library_id.iter().cloned().collect::<Vec<_>>();
        let registry = LibraryRegistry::new(Arc::new(PostgresLibraryStore::new(pool.clone())));
        let scopes = registry
            .scopes_for_ids(&ids)
            .await
            .map_err(library_selector_error)?;
        let placeholder = predicates.texts(scopes);

        predicates.push(format!(
            "EXISTS (SELECT 1 FROM track_sources AS sel_library \
             WHERE sel_library.track_id = t.id \
               AND sel_library.source_scope = ANY({placeholder}))"
        ));
    }

    Ok(())
}

#[derive(Debug)]
enum TrackBind {
    Text(String),
    Texts(Vec<String>),
    Uuids(Vec<Uuid>),
    Boolean(bool),
    Integer(i64),
    Small(i32),
    Timestamp(chrono::DateTime<chrono::Utc>),
}

impl TrackBind {
    fn bind<'q, R>(
        self,
        query: sqlx::query::QueryAs<'q, sqlx::postgres::Postgres, R, sqlx::postgres::PgArguments>,
    ) -> sqlx::query::QueryAs<'q, sqlx::postgres::Postgres, R, sqlx::postgres::PgArguments> {
        match self {
            Self::Text(value) => query.bind(value),
            Self::Texts(value) => query.bind(value),
            Self::Uuids(value) => query.bind(value),
            Self::Boolean(value) => query.bind(value),
            Self::Integer(value) => query.bind(value),
            Self::Small(value) => query.bind(value),
            Self::Timestamp(value) => query.bind(value),
        }
    }
}

struct TrackPredicates {
    clauses: Vec<String>,
    binds: Vec<TrackBind>,
    next: u16,
}

impl TrackPredicates {
    fn new(first: u16) -> Self {
        Self {
            clauses: Vec::new(),
            binds: Vec::new(),
            next: first,
        }
    }

    fn push(&mut self, clause: String) {
        self.clauses.push(clause);
    }

    fn where_clause(&self) -> String {
        if self.clauses.is_empty() {
            String::new()
        } else {
            format!("AND {}", self.clauses.join("\n                  AND "))
        }
    }

    fn values(self) -> Vec<TrackBind> {
        self.binds
    }

    fn add(&mut self, value: TrackBind, cast: &str) -> String {
        let placeholder = format!("${}::{cast}", self.next);

        self.next = self
            .next
            .checked_add(1)
            .expect("PostgreSQL bind index exceeds u16 range");
        self.binds.push(value);

        placeholder
    }

    fn text(&mut self, value: String) -> String {
        self.add(TrackBind::Text(value), "TEXT")
    }

    fn texts(&mut self, value: Vec<String>) -> String {
        self.add(TrackBind::Texts(value), "TEXT[]")
    }

    fn uuids(&mut self, value: Vec<Uuid>) -> String {
        self.add(TrackBind::Uuids(value), "UUID[]")
    }

    fn boolean(&mut self, value: bool) -> String {
        self.add(TrackBind::Boolean(value), "BOOLEAN")
    }

    fn integer(&mut self, value: i64) -> String {
        self.add(TrackBind::Integer(value), "BIGINT")
    }

    fn small(&mut self, value: i32) -> String {
        self.add(TrackBind::Small(value), "INTEGER")
    }

    fn timestamp(&mut self, value: chrono::DateTime<chrono::Utc>) -> String {
        self.add(TrackBind::Timestamp(value), "TIMESTAMPTZ")
    }

    fn i64_range(&mut self, fact: &str, range: Range<i64>) -> String {
        let mut terms = Vec::new();

        if let Some(from) = range.from() {
            terms.push(format!("{fact} >= {}", self.integer(from)));
        }
        if let Some(to) = range.to() {
            terms.push(format!("{fact} <= {}", self.integer(to)));
        }

        terms.join(" AND ")
    }

    fn i32_range(&mut self, fact: &str, range: Range<i32>) -> String {
        let mut terms = Vec::new();

        if let Some(from) = range.from() {
            terms.push(format!("{fact} >= {}", self.small(from)));
        }
        if let Some(to) = range.to() {
            terms.push(format!("{fact} <= {}", self.small(to)));
        }

        terms.join(" AND ")
    }

    fn u8_range(&mut self, fact: &str, range: Range<u8>) -> String {
        let mut terms = Vec::new();

        if let Some(from) = range.from() {
            terms.push(format!("{fact} >= {}", self.small(i32::from(from))));
        }
        if let Some(to) = range.to() {
            terms.push(format!("{fact} <= {}", self.small(i32::from(to))));
        }

        terms.join(" AND ")
    }

    fn u16_range(&mut self, fact: &str, range: Range<u16>) -> String {
        let mut terms = Vec::new();

        if let Some(from) = range.from() {
            terms.push(format!("{fact} >= {}", self.small(i32::from(from))));
        }
        if let Some(to) = range.to() {
            terms.push(format!("{fact} <= {}", self.small(i32::from(to))));
        }

        terms.join(" AND ")
    }

    fn u32_range(&mut self, fact: &str, range: Range<u32>) -> String {
        let mut terms = Vec::new();

        if let Some(from) = range.from() {
            terms.push(format!("{fact} >= {}", self.integer(i64::from(from))));
        }
        if let Some(to) = range.to() {
            terms.push(format!("{fact} <= {}", self.integer(i64::from(to))));
        }

        terms.join(" AND ")
    }

    fn u64_range(&mut self, fact: &str, range: Range<u64>) -> String {
        let mut terms = Vec::new();

        if let Some(from) = range.from() {
            terms.push(format!(
                "{fact} >= {}",
                self.integer(i64::try_from(from).unwrap_or(i64::MAX))
            ));
        }
        if let Some(to) = range.to() {
            terms.push(format!(
                "{fact} <= {}",
                self.integer(i64::try_from(to).unwrap_or(i64::MAX))
            ));
        }

        terms.join(" AND ")
    }

    fn time_range(&mut self, fact: &str, range: Range<chrono::DateTime<chrono::Utc>>) -> String {
        let mut terms = Vec::new();

        if let Some(from) = range.from() {
            terms.push(format!("{fact} >= {}", self.timestamp(from)));
        }
        if let Some(to) = range.to() {
            terms.push(format!("{fact} <= {}", self.timestamp(to)));
        }

        terms.join(" AND ")
    }
}

fn track_order_clause(
    order: TrackOrder,
    direction: SortDirection,
    seed: Option<RandomSeed>,
    album: Option<&str>,
    predicates: &mut TrackPredicates,
) -> String {
    let dir = direction.sql();

    match order {
        TrackOrder::Title => format!("t.title_normalized {dir}, t.id"),
        TrackOrder::Id => format!("t.id {dir}"),
        TrackOrder::Album => format!(
            "{} {dir} NULLS LAST, {} {dir} NULLS LAST, {} {dir} NULLS LAST, \
             {} {dir} NULLS LAST, t.id",
            primary_membership_row(album, "member_album.title_normalized"),
            primary_membership_row(album, "member.disc_number"),
            primary_membership_row(album, "member.track_number"),
            primary_membership_row(album, "member.position"),
        ),
        TrackOrder::Track => format!(
            "{} {dir} NULLS LAST, {} {dir} NULLS LAST, {} {dir} NULLS LAST, t.id",
            primary_membership_row(album, "member.disc_number"),
            primary_membership_row(album, "member.track_number"),
            primary_membership_row(album, "member.position"),
        ),
        TrackOrder::Artist => format!(
            "(SELECT artist.name_normalized FROM track_artist_credits AS credited \
              LEFT JOIN artists AS artist ON artist.id = credited.artist_id \
              WHERE credited.track_id = t.id ORDER BY credited.position LIMIT 1) \
             {dir} NULLS LAST, t.id"
        ),
        TrackOrder::AlbumArtist => format!(
            "(SELECT album_artist.name_normalized FROM album_tracks AS member \
              JOIN albums AS member_album ON member_album.id = member.album_id \
              JOIN artists AS album_artist ON album_artist.id = member_album.album_artist_id \
              WHERE member.track_id = t.id{} \
              ORDER BY member_album.title_normalized, member_album.id, member.position \
              LIMIT 1) {dir} NULLS LAST, t.id",
            album_membership_restriction(album)
        ),
        TrackOrder::Duration => format!("t.duration_ms {dir} NULLS LAST, t.id"),
        TrackOrder::Genre => format!(
            "(SELECT genre.name_normalized FROM track_genres AS genre \
              WHERE genre.track_id = t.id ORDER BY genre.position LIMIT 1) \
             {dir} NULLS LAST, t.id"
        ),
        TrackOrder::Year | TrackOrder::ReleaseYear => format!(
            "{} {dir} NULLS LAST, t.id",
            primary_membership_row(album, "member_album.year")
        ),
        TrackOrder::DateAdded => format!(
            "(SELECT MIN(added.created_at) FROM track_sources AS added \
              WHERE added.track_id = t.id AND added.provider_id = ANY($2::TEXT[])) \
             {dir} NULLS LAST, t.id"
        ),
        TrackOrder::StarredAt => format!(
            "(SELECT favorite.starred_at FROM track_favorites AS favorite \
              WHERE favorite.track_id = t.id AND favorite.user_id = $1) \
             {dir} NULLS LAST, t.id"
        ),
        TrackOrder::Rating => format!(
            "(SELECT rating.rating FROM track_ratings AS rating \
              WHERE rating.track_id = t.id AND rating.user_id = $1) \
             {dir} NULLS LAST, t.id"
        ),
        TrackOrder::PlayCount => format!(
            "COALESCE((SELECT plays.play_count FROM track_play_stats AS plays \
              WHERE plays.track_id = t.id AND plays.user_id = $1), 0) {dir}, t.id"
        ),
        TrackOrder::PlayDate => format!(
            "(SELECT plays.last_played_at FROM track_play_stats AS plays \
              WHERE plays.track_id = t.id AND plays.user_id = $1) \
             {dir} NULLS LAST, t.id"
        ),
        TrackOrder::Channels => format!(
            "{} {dir} NULLS LAST, t.id",
            selected_representation_scalar("rep.channels")
        ),
        TrackOrder::Random => {
            let seed = seed.map_or(0, |seed| seed.get() as i64);
            let placeholder = predicates.integer(seed);

            format!("hashtextextended(t.id::text, {placeholder}) {dir}, t.id")
        }
        TrackOrder::RecentlyAdded => format!("t.created_at {dir}, t.id"),
    }
}

fn album_play_count(user_bind: u8) -> String {
    format!(
        "COALESCE((SELECT SUM(plays.play_count) FROM track_play_stats AS plays \
          JOIN album_tracks AS plays_member ON plays_member.track_id = plays.track_id \
          WHERE plays_member.album_id = a.id AND plays.user_id = ${user_bind}), 0)"
    )
}

fn album_role_clause(
    role: RoleSelector,
    artists: Option<&str>,
    predicates: &mut TrackPredicates,
) -> String {
    let artist_term = artists.map_or_else(String::new, |placeholder| {
        format!(" AND sel_role.artist_id = ANY({placeholder})")
    });

    match role.stored_role() {
        Some(stored) => {
            let placeholder = predicates.text(stored.as_str().to_owned());

            format!(
                "EXISTS (SELECT 1 FROM album_tracks AS sel_role_member \
                 JOIN track_artist_credits AS sel_role \
                   ON sel_role.track_id = sel_role_member.track_id \
                 WHERE sel_role_member.album_id = a.id \
                   AND sel_role.role = {placeholder}{artist_term})"
            )
        }
        None if role == RoleSelector::Artist => format!(
            "EXISTS (SELECT 1 FROM album_tracks AS sel_role_member \
             JOIN track_artist_credits AS sel_role \
               ON sel_role.track_id = sel_role_member.track_id \
             WHERE sel_role_member.album_id = a.id AND sel_role.role IS NULL{artist_term})"
        ),
        None => artists.map_or_else(
            || "a.album_artist_id IS NOT NULL".to_owned(),
            |placeholder| format!("a.album_artist_id = ANY({placeholder})"),
        ),
    }
}

async fn album_selector_clauses(
    pool: &PgPool,
    filter: &AlbumListFilter,
    predicates: &mut TrackPredicates,
) -> Result<(), TrackRepositoryError> {
    if let Some(name) = filter.name() {
        let clause = full_text_clause(name, predicates, |pattern| {
            vec![
                format!("{} LIKE {pattern}", search_folded("a.title_normalized")),
                format!(
                    "EXISTS (SELECT 1 FROM artists AS text_album_artist \
                    WHERE text_album_artist.id = a.album_artist_id AND {} LIKE {pattern})",
                    search_folded("text_album_artist.name_normalized")
                ),
                format!(
                    "EXISTS (SELECT 1 FROM album_tracks AS text_member \
                    JOIN track_artist_credits AS text_credit ON text_credit.track_id = text_member.track_id \
                    JOIN artists AS text_artist ON text_artist.id = text_credit.artist_id \
                    WHERE text_member.album_id = a.id AND {} LIKE {pattern})",
                    search_folded("text_artist.name_normalized")
                ),
            ]
        });
        predicates.push(clause);
    }

    if let Some(artist_id) = filter.artist_id() {
        let ids = artist_id.iter().map(|id| *id.as_uuid()).collect::<Vec<_>>();
        let placeholder = predicates.uuids(ids);

        predicates.push(format!(
            "(a.album_artist_id = ANY({placeholder}) OR EXISTS (
                SELECT 1 FROM album_tracks AS sel_album_credit_member
                JOIN track_artist_credits AS sel_album_credit
                  ON sel_album_credit.track_id = sel_album_credit_member.track_id
                WHERE sel_album_credit_member.album_id = a.id
                  AND sel_album_credit.artist_id = ANY({placeholder})))"
        ));
    }

    if let Some(album_artist_id) = filter.album_artist_id() {
        let ids = album_artist_id
            .iter()
            .map(|id| *id.as_uuid())
            .collect::<Vec<_>>();
        let placeholder = predicates.uuids(ids);

        predicates.push(format!("a.album_artist_id = ANY({placeholder})"));
    }

    let artists = filter.artists_id().map(|artists_id| {
        let ids = artists_id
            .iter()
            .map(|id| *id.as_uuid())
            .collect::<Vec<_>>();

        predicates.uuids(ids)
    });

    if let Some(roles) = filter.role() {
        let mut terms = Vec::new();

        for role in roles.iter() {
            terms.push(album_role_clause(*role, artists.as_deref(), predicates));
        }

        predicates.push(format!("({})", terms.join(" OR ")));
    } else if let Some(placeholder) = artists.as_deref() {
        predicates.push(format!(
            "EXISTS (SELECT 1 FROM album_tracks AS sel_credit_member \
             JOIN track_artist_credits AS sel_credit \
               ON sel_credit.track_id = sel_credit_member.track_id \
             WHERE sel_credit_member.album_id = a.id \
               AND sel_credit.artist_id = ANY({placeholder}))"
        ));
    }

    if let Some(genre) = filter.genre() {
        let names = genre
            .iter()
            .map(|name| name.as_str().to_owned())
            .collect::<Vec<_>>();
        let placeholder = predicates.texts(names);

        predicates.push(format!(
            "EXISTS (SELECT 1 FROM album_tracks AS sel_genre_member \
             JOIN track_genres AS sel_genre ON sel_genre.track_id = sel_genre_member.track_id \
             WHERE sel_genre_member.album_id = a.id \
               AND sel_genre.name_normalized = ANY({placeholder}))"
        ));
    }

    if let Some(year) = filter.year() {
        let range = predicates.i32_range("a.year", year);

        predicates.push(range);
    }

    if let Some(compilation) = filter.compilation() {
        let placeholder = predicates.boolean(compilation);

        predicates.push(format!("a.compilation = {placeholder}"));
    }

    if let Some(starred) = filter.starred() {
        let clause = "EXISTS (SELECT 1 FROM album_favorites AS sel_favorite \
                      WHERE sel_favorite.album_id = a.id AND sel_favorite.user_id = $1)";

        predicates.push(if starred {
            clause.to_owned()
        } else {
            format!("NOT {clause}")
        });
    }

    if let Some(has_rating) = filter.has_rating() {
        let clause = "EXISTS (SELECT 1 FROM album_ratings AS sel_rating \
                      WHERE sel_rating.album_id = a.id AND sel_rating.user_id = $1)";

        predicates.push(if has_rating {
            clause.to_owned()
        } else {
            format!("NOT {clause}")
        });
    }

    if let Some(rating) = filter.rating() {
        let range = predicates.u8_range("sel_rating.rating", rating);

        predicates.push(format!(
            "EXISTS (SELECT 1 FROM album_ratings AS sel_rating \
             WHERE sel_rating.album_id = a.id AND sel_rating.user_id = $1 AND {range})"
        ));
    }

    if let Some(play_count) = filter.play_count() {
        let clause = predicates.u32_range(&album_play_count(1), play_count);

        predicates.push(clause);
    }

    if let Some(recently_played) = filter.recently_played() {
        let clause = "EXISTS (SELECT 1 FROM track_play_stats AS sel_played \
                      JOIN album_tracks AS sel_played_member \
                        ON sel_played_member.track_id = sel_played.track_id \
                      WHERE sel_played_member.album_id = a.id AND sel_played.user_id = $1 \
                        AND sel_played.last_played_at IS NOT NULL)";

        predicates.push(if recently_played {
            clause.to_owned()
        } else {
            format!("NOT {clause}")
        });
    }

    if let Some(date_added) = filter.date_added() {
        let clause = predicates.time_range(
            "(SELECT MIN(sel_added.created_at) FROM album_sources AS sel_added \
              WHERE sel_added.album_id = a.id AND sel_added.provider_id = ANY($2::TEXT[]))",
            date_added,
        );

        predicates.push(clause);
    }

    if let Some(missing) = filter.missing() {
        let clause = "EXISTS (SELECT 1 FROM album_tracks AS sel_missing_member \
                      JOIN track_sources AS sel_missing \
                        ON sel_missing.track_id = sel_missing_member.track_id \
                      WHERE sel_missing_member.album_id = a.id \
                        AND sel_missing.provider_id = ANY($2::TEXT[]) \
                        AND sel_missing.missing_at IS NOT NULL)";

        predicates.push(if missing {
            clause.to_owned()
        } else {
            format!("NOT {clause}")
        });
    }

    if let Some(library_id) = filter.library_id() {
        let ids = library_id.iter().cloned().collect::<Vec<_>>();
        let registry = LibraryRegistry::new(Arc::new(PostgresLibraryStore::new(pool.clone())));
        let scopes = registry
            .scopes_for_ids(&ids)
            .await
            .map_err(library_selector_error)?;
        let placeholder = predicates.texts(scopes);

        predicates.push(format!(
            "EXISTS (SELECT 1 FROM album_sources AS sel_library \
             WHERE sel_library.album_id = a.id \
               AND sel_library.source_scope = ANY({placeholder}))"
        ));
    }

    Ok(())
}

fn album_order_clause(
    order: AlbumOrder,
    direction: SortDirection,
    seed: Option<RandomSeed>,
    predicates: &mut TrackPredicates,
) -> String {
    let dir = direction.sql();

    match order {
        AlbumOrder::Name => format!("a.title_normalized {dir}, a.id"),
        AlbumOrder::AlbumArtist => format!("ar.name_normalized {dir} NULLS LAST, a.id"),
        AlbumOrder::Artist => format!(
            "(SELECT MIN(credited_artist.name_normalized) FROM album_tracks AS sort_member \
              JOIN track_artist_credits AS sort_credit \
                ON sort_credit.track_id = sort_member.track_id \
              JOIN artists AS credited_artist ON credited_artist.id = sort_credit.artist_id \
              WHERE sort_member.album_id = a.id) {dir} NULLS LAST, a.id"
        ),
        AlbumOrder::Duration => format!("COALESCE(SUM(t.duration_ms), 0) {dir}, a.id"),
        AlbumOrder::SongCount => format!("COUNT(DISTINCT mt.track_id) {dir}, a.id"),
        AlbumOrder::MaxYear => format!("a.year {dir} NULLS LAST, a.id"),
        AlbumOrder::DateAdded => format!(
            "(SELECT MIN(added.created_at) FROM album_sources AS added \
              WHERE added.album_id = a.id AND added.provider_id = ANY($2::TEXT[])) \
             {dir} NULLS LAST, a.id"
        ),
        AlbumOrder::StarredAt => format!(
            "(SELECT favorite.starred_at FROM album_favorites AS favorite \
              WHERE favorite.album_id = a.id AND favorite.user_id = $1) \
             {dir} NULLS LAST, a.id"
        ),
        AlbumOrder::Rating => format!(
            "(SELECT rating.rating FROM album_ratings AS rating \
              WHERE rating.album_id = a.id AND rating.user_id = $1) \
             {dir} NULLS LAST, a.id"
        ),
        AlbumOrder::PlayCount => format!("{} {dir}, a.id", album_play_count(1)),
        AlbumOrder::PlayDate => format!(
            "(SELECT MAX(plays.last_played_at) FROM track_play_stats AS plays \
              JOIN album_tracks AS plays_member ON plays_member.track_id = plays.track_id \
              WHERE plays_member.album_id = a.id AND plays.user_id = $1) \
             {dir} NULLS LAST, a.id"
        ),
        AlbumOrder::Random => {
            let seed = seed.map_or(0, |seed| seed.get() as i64);
            let placeholder = predicates.integer(seed);

            format!("hashtextextended(a.id::text, {placeholder}) {dir}, a.id")
        }
        AlbumOrder::RecentlyAdded => format!("a.created_at {dir}, a.id"),
    }
}

struct ArtistSelectors {
    membership: String,
    album_count: String,
    song_count: String,
}

fn album_filter_scope(filter: &AlbumListFilter, user_bind: u8, libraries_bind: u8) -> String {
    match (filter.scope(), filter.name().is_some()) {
        (crate::features::music_library::domain::AlbumListScope::EngagedOrDiscovered, false) => {
            album_scope(
                crate::features::music_library::domain::AlbumListScope::Engaged,
                user_bind,
                libraries_bind,
            )
        }
        (scope, _) => album_scope(scope, user_bind, libraries_bind),
    }
}

fn artist_list_scope(filter: &ArtistListFilter) -> String {
    let engaged = artist_scope(1, 2);
    if filter.name().is_none() || !filter.provider_identity() {
        return engaged;
    }

    format!(
        "({engaged} OR EXISTS (SELECT 1 FROM artist_sources AS discovered_artist \
         WHERE discovered_artist.artist_id = ar.id \
           AND discovered_artist.provider_id <> ALL($2::TEXT[])) \
         OR EXISTS (SELECT 1 FROM track_artist_credits AS discovered_credit \
         JOIN track_sources AS discovered_track \
           ON discovered_track.track_id = discovered_credit.track_id \
         WHERE discovered_credit.artist_id = ar.id \
           AND discovered_track.provider_id <> ALL($2::TEXT[])))"
    )
}

fn artist_role_membership(role: Option<&RoleSet>, predicates: &mut TrackPredicates) -> String {
    let mut stored_roles = Vec::new();
    let mut primary = role.is_none();
    let mut album_artist = role.is_none();

    for selector in role.into_iter().flat_map(|roles| roles.iter()) {
        if let Some(stored) = selector.stored_role() {
            stored_roles.push(stored.as_str().to_owned());
        } else if *selector == RoleSelector::Artist {
            primary = true;
        } else {
            album_artist = true;
        }
    }

    let credit_condition = if role.is_none() {
        Some(String::new())
    } else if stored_roles.is_empty() {
        if primary {
            Some(" AND membership_credit.role IS NULL".to_owned())
        } else {
            None
        }
    } else if primary {
        let placeholder = predicates.texts(stored_roles);

        Some(format!(
            " AND (membership_credit.role = ANY({placeholder}) \
             OR membership_credit.role IS NULL)"
        ))
    } else {
        let placeholder = predicates.texts(stored_roles);

        Some(format!(" AND membership_credit.role = ANY({placeholder})"))
    };

    artist_membership(credit_condition.as_deref(), album_artist)
}

fn artist_membership_selectors(
    filter: &ArtistListFilter,
    predicates: &mut TrackPredicates,
) -> ArtistSelectors {
    let membership = artist_role_membership(filter.role(), predicates);
    let (album_count, song_count) = artist_count_expressions(&membership);

    if filter.role().is_some() || filter.provider_identity() {
        let has_membership = format!("EXISTS (SELECT 1 FROM ({membership}) AS artist_membership)");
        if filter.provider_identity() {
            let libraries = predicates.texts(
                ProviderRegistry::builtin()
                    .library_ids()
                    .into_iter()
                    .map(str::to_owned)
                    .collect(),
            );
            predicates.push(format!(
                "({has_membership} OR EXISTS (SELECT 1 FROM artist_sources AS provider_artist \
                 WHERE provider_artist.artist_id = ar.id \
                   AND provider_artist.provider_id <> ALL({libraries})))"
            ));
        } else {
            predicates.push(has_membership);
        }
    }

    ArtistSelectors {
        membership,
        album_count,
        song_count,
    }
}

async fn artist_selector_clauses(
    pool: &PgPool,
    filter: &ArtistListFilter,
    predicates: &mut TrackPredicates,
) -> Result<ArtistSelectors, TrackRepositoryError> {
    if let Some(name) = filter.name() {
        let clause = full_text_clause(name, predicates, |pattern| {
            vec![format!(
                "{} LIKE {pattern}",
                search_folded("ar.name_normalized")
            )]
        });
        predicates.push(clause);
    }

    let selectors = artist_membership_selectors(filter, predicates);

    if let Some(starred) = filter.starred() {
        let clause = "EXISTS (SELECT 1 FROM artist_favorites AS sel_artist_favorite \
                      WHERE sel_artist_favorite.artist_id = ar.id \
                        AND sel_artist_favorite.user_id = $1)";

        predicates.push(if starred {
            clause.to_owned()
        } else {
            format!("NOT {clause}")
        });
    }

    if let Some(has_rating) = filter.has_rating() {
        let clause = "EXISTS (SELECT 1 FROM artist_ratings AS sel_artist_rating \
                      WHERE sel_artist_rating.artist_id = ar.id \
                        AND sel_artist_rating.user_id = $1)";

        predicates.push(if has_rating {
            clause.to_owned()
        } else {
            format!("NOT {clause}")
        });
    }

    if let Some(rating) = filter.rating() {
        let range = predicates.u8_range("sel_artist_rating.rating", rating);

        predicates.push(format!(
            "EXISTS (SELECT 1 FROM artist_ratings AS sel_artist_rating \
             WHERE sel_artist_rating.artist_id = ar.id \
               AND sel_artist_rating.user_id = $1 AND {range})"
        ));
    }

    if let Some(library_id) = filter.library_id() {
        let ids = library_id.iter().cloned().collect::<Vec<_>>();
        let registry = LibraryRegistry::new(Arc::new(PostgresLibraryStore::new(pool.clone())));
        let scopes = registry
            .scopes_for_ids(&ids)
            .await
            .map_err(library_selector_error)?;
        let placeholder = predicates.texts(scopes);

        predicates.push(format!(
            "(EXISTS (SELECT 1 FROM artist_sources AS sel_artist_library \
             WHERE sel_artist_library.artist_id = ar.id \
               AND sel_artist_library.source_scope = ANY({placeholder})) \
             OR EXISTS (SELECT 1 FROM track_artist_credits AS sel_artist_library_credit \
             JOIN track_sources AS sel_artist_library_source \
               ON sel_artist_library_source.track_id = sel_artist_library_credit.track_id \
             WHERE sel_artist_library_credit.artist_id = ar.id \
               AND sel_artist_library_source.source_scope = ANY({placeholder})))"
        ));
    }

    if let Some(missing) = filter.missing() {
        let clause = "EXISTS (SELECT 1 FROM track_artist_credits AS sel_missing_credit \
                      JOIN track_sources AS sel_missing \
                        ON sel_missing.track_id = sel_missing_credit.track_id \
                      WHERE sel_missing_credit.artist_id = ar.id \
                        AND sel_missing.provider_id = ANY($2::TEXT[]) \
                        AND sel_missing.missing_at IS NOT NULL)";

        predicates.push(if missing {
            clause.to_owned()
        } else {
            format!("NOT {clause}")
        });
    }

    Ok(selectors)
}

fn artist_order_clause(
    sort: ArtistSort,
    direction: SortDirection,
    selectors: &ArtistSelectors,
) -> String {
    let dir = direction.sql();

    match sort {
        ArtistSort::Name => format!("ar.name_normalized {dir}, ar.id"),
        ArtistSort::AlbumCount => format!("{} {dir}, ar.id", selectors.album_count),
        ArtistSort::SongCount => format!("{} {dir}, ar.id", selectors.song_count),
        ArtistSort::StarredAt => format!(
            "(SELECT favorite.starred_at FROM artist_favorites AS favorite \
              WHERE favorite.artist_id = ar.id AND favorite.user_id = $1) \
             {dir} NULLS LAST, ar.id"
        ),
        ArtistSort::Rating => format!(
            "(SELECT rating.rating FROM artist_ratings AS rating \
              WHERE rating.artist_id = ar.id AND rating.user_id = $1) \
             {dir} NULLS LAST, ar.id"
        ),
        ArtistSort::PlayCount => format!(
            "COALESCE((SELECT SUM(plays.play_count) \
              FROM (SELECT DISTINCT credited.track_id FROM ({membership}) AS credited \
                    WHERE credited.track_id IS NOT NULL) AS distinct_credited \
              JOIN track_play_stats AS plays \
                ON plays.track_id = distinct_credited.track_id AND plays.user_id = $1), 0) \
             {dir}, ar.id",
            membership = selectors.membership,
        ),
    }
}
