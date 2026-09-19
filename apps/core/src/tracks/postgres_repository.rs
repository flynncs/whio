use std::collections::HashMap;

use async_trait::async_trait;
use sqlx::{PgConnection, PgPool};
use uuid::Uuid;

use crate::tracks::{
    Album, AlbumEntry, AlbumId, AlbumMetadata, AlbumSummary, AlbumTrack, Artist, ArtistCredit,
    ArtistId, ArtistSummary, ArtworkRef, ProviderId, SourceFileFacts, SourceIdentity, SourceScope,
    Track, TrackId, TrackMetadata, TrackRepository, TrackRepositoryError,
};

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

        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|_| TrackRepositoryError::Unavailable)?;

        let track_id = find_track_id_by_source_for_update(&mut tx, &source).await?;

        let track_id = if let Some(track_id) = track_id {
            track_id
        } else {
            let candidate_track_id = TrackId::new();

            insert_candidate_track(&mut tx, &candidate_track_id, &metadata).await?;

            if try_insert_source(&mut tx, &candidate_track_id, &source).await? {
                candidate_track_id
            } else {
                resolve_concurrent_winner(&mut tx, &candidate_track_id, &source).await?
            }
        };

        update_track_metadata(&mut tx, &track_id, &metadata).await?;
        replace_artist_credits(&mut tx, &track_id, &credits).await?;

        tx.commit()
            .await
            .map_err(|_| TrackRepositoryError::Unavailable)?;

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
        .map_err(|_| TrackRepositoryError::Unavailable)?;

        rows.into_iter()
            .map(|(provider_id, source_scope, external_id)| {
                decode_source(provider_id, source_scope, external_id)
            })
            .collect()
    }

    async fn find_track(&self, track_id: &TrackId) -> Result<Option<Track>, TrackRepositoryError> {
        let row: Option<(String, Option<i64>, Option<String>, Option<String>)> = sqlx::query_as(
            r#"SELECT title, duration_ms, musicbrainz_recording_id, isrc
            FROM tracks
            WHERE id = $1
            "#,
        )
        .bind(track_id.as_uuid())
        .fetch_optional(&self.pool)
        .await
        .map_err(|_| TrackRepositoryError::Unavailable)?;

        let Some((title, duration_ms, recording_id, isrc)) = row else {
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
        .map_err(|_| TrackRepositoryError::Unavailable)?;

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
            artists,
        )?))
    }

    async fn set_source_artwork(
        &self,
        source: &SourceIdentity,
        artwork: &ArtworkRef,
    ) -> Result<(), TrackRepositoryError> {
        sqlx::query(
            r#"UPDATE track_sources
            SET artwork_kind = $1, artwork_ref = $2
            WHERE provider_id = $3 AND source_scope = $4 AND external_id = $5
            "#,
        )
        .bind(artwork.kind())
        .bind(artwork.reference())
        .bind(source.provider_id().as_str())
        .bind(encode_source_scope(source.source_scope()))
        .bind(source.external_id())
        .execute(&self.pool)
        .await
        .map(|_| ())
        .map_err(|_| TrackRepositoryError::Unavailable)
    }

    async fn find_artwork(
        &self,
        track_id: &TrackId,
    ) -> Result<Option<(SourceIdentity, ArtworkRef)>, TrackRepositoryError> {
        let rows = sqlx::query_as::<_, (String, String, String, String, String)>(
            r#"SELECT provider_id, source_scope, external_id, artwork_kind, artwork_ref
            FROM track_sources
            WHERE track_id = $1
              AND artwork_kind IS NOT NULL
              AND artwork_ref IS NOT NULL
            ORDER BY created_at, id
            "#,
        )
        .bind(track_id.as_uuid())
        .fetch_all(&self.pool)
        .await
        .map_err(|_| TrackRepositoryError::Unavailable)?;

        let mut best: Option<(SourceIdentity, ArtworkRef)> = None;
        for (provider_id, source_scope, external_id, kind, reference) in rows {
            let source = decode_source(provider_id, source_scope, external_id)?;
            let artwork = ArtworkRef::from_stored(&kind, reference)
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
        .execute(&self.pool)
        .await
        .map(|_| ())
        .map_err(|_| TrackRepositoryError::Unavailable)
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
        .map_err(|_| TrackRepositoryError::Unavailable)?;

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
        .map_err(|_| TrackRepositoryError::Unavailable)?;

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
        .map_err(|_| TrackRepositoryError::Unavailable)
    }

    async fn set_album_track(
        &self,
        album_id: &AlbumId,
        track_id: &TrackId,
        membership: &AlbumTrack,
    ) -> Result<(), TrackRepositoryError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|_| TrackRepositoryError::Unavailable)?;

        sqlx::query("DELETE FROM album_tracks WHERE track_id = $1 AND album_id <> $2")
            .bind(track_id.as_uuid())
            .bind(album_id.as_uuid())
            .execute(&mut *tx)
            .await
            .map_err(|_| TrackRepositoryError::Unavailable)?;

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
        .map_err(|_| TrackRepositoryError::Unavailable)?;

        tx.commit()
            .await
            .map_err(|_| TrackRepositoryError::Unavailable)?;

        Ok(())
    }

    async fn clear_album_track(&self, track_id: &TrackId) -> Result<(), TrackRepositoryError> {
        sqlx::query("DELETE FROM album_tracks WHERE track_id = $1")
            .bind(track_id.as_uuid())
            .execute(&self.pool)
            .await
            .map(|_| ())
            .map_err(|_| TrackRepositoryError::Unavailable)
    }

    async fn find_album(&self, album_id: &AlbumId) -> Result<Option<Album>, TrackRepositoryError> {
        let row = sqlx::query_as::<_, AlbumRow>(&format!(
            "SELECT {ALBUM_COLUMNS} FROM albums WHERE id = $1"
        ))
        .bind(album_id.as_uuid())
        .fetch_optional(&self.pool)
        .await
        .map_err(|_| TrackRepositoryError::Unavailable)?;

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
        .map_err(|_| TrackRepositoryError::Unavailable)?;

        row.map(decode_album).transpose()
    }

    async fn find_artist(
        &self,
        artist_id: &ArtistId,
    ) -> Result<Option<Artist>, TrackRepositoryError> {
        let row = sqlx::query_as::<_, ArtistRow>(&format!(
            "SELECT {ARTIST_COLUMNS} FROM artists WHERE id = $1"
        ))
        .bind(artist_id.as_uuid())
        .fetch_optional(&self.pool)
        .await
        .map_err(|_| TrackRepositoryError::Unavailable)?;

        row.map(decode_artist).transpose()
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
        .map_err(|_| TrackRepositoryError::Unavailable)?;

        rows.into_iter()
            .map(|(provider_id, source_scope, external_id)| {
                decode_source(provider_id, source_scope, external_id)
            })
            .collect()
    }

    async fn find_album_entries(
        &self,
        album_id: &AlbumId,
    ) -> Result<Vec<AlbumEntry>, TrackRepositoryError> {
        let rows = sqlx::query_as::<
            _,
            (
                Uuid,
                String,
                Option<i64>,
                Option<String>,
                Option<String>,
                Option<i16>,
                Option<i16>,
                i32,
            ),
        >(
            r#"
            SELECT t.id, t.title, t.duration_ms, t.musicbrainz_recording_id, t.isrc,
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
        .map_err(|_| TrackRepositoryError::Unavailable)?;

        let track_ids = rows.iter().map(|row| row.0).collect::<Vec<_>>();
        let mut credits = credits_for_tracks(&self.pool, &track_ids).await?;
        let mut entries = Vec::with_capacity(rows.len());

        for (id, title, duration_ms, recording_id, isrc, disc_number, track_number, position) in
            rows
        {
            let track = decode_track(
                id,
                title,
                duration_ms,
                recording_id,
                isrc,
                credits.remove(&id).unwrap_or_default(),
            )?;
            let membership = decode_membership(disc_number, track_number, position)?;

            entries.push(AlbumEntry::new(track, membership));
        }

        Ok(entries)
    }

    async fn find_albums_for_artist(
        &self,
        artist_id: &ArtistId,
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
        .map_err(|_| TrackRepositoryError::Unavailable)?;

        rows.into_iter().map(decode_album_summary).collect()
    }

    async fn list_albums(
        &self,
        offset: u32,
        limit: u32,
    ) -> Result<Vec<AlbumSummary>, TrackRepositoryError> {
        let rows = sqlx::query_as::<_, AlbumBrowseRow>(&format!(
            r#"
            SELECT {ALBUM_BROWSE_COLUMNS}
            {ALBUM_BROWSE_FROM}
            GROUP BY a.id, ar.id
            ORDER BY a.title_normalized, a.id
            LIMIT $1 OFFSET $2
            "#
        ))
        .bind(i64::from(limit))
        .bind(i64::from(offset))
        .fetch_all(&self.pool)
        .await
        .map_err(|_| TrackRepositoryError::Unavailable)?;

        rows.into_iter().map(decode_album_summary).collect()
    }

    async fn list_artists(
        &self,
        offset: u32,
        limit: u32,
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
        .map_err(|_| TrackRepositoryError::Unavailable)?;

        rows.into_iter().map(decode_artist_summary).collect()
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
type AlbumBrowseRow = (
    Uuid,
    String,
    Option<Uuid>,
    Option<i16>,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<Uuid>,
    Option<String>,
    Option<String>,
    Option<String>,
    i64,
    i64,
);

const ARTIST_COLUMNS: &str = "id, name, sort_name, musicbrainz_artist_id";
const ALBUM_COLUMNS: &str = "id, title, album_artist_id, year, release_type, label, \
     musicbrainz_release_group_id, musicbrainz_release_id";
const ALBUM_BROWSE_COLUMNS: &str = "a.id, a.title, a.album_artist_id, a.year, a.release_type, \
     a.label, a.musicbrainz_release_group_id, a.musicbrainz_release_id, \
     ar.id, ar.name, ar.sort_name, ar.musicbrainz_artist_id, \
     COUNT(DISTINCT mt.track_id)::BIGINT, COALESCE(SUM(t.duration_ms), 0)::BIGINT";
const ALBUM_BROWSE_FROM: &str = "FROM albums AS a \
     LEFT JOIN artists AS ar ON ar.id = a.album_artist_id \
     LEFT JOIN album_tracks AS mt ON mt.album_id = a.id \
     LEFT JOIN tracks AS t ON t.id = mt.track_id";

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
    .map_err(|_| TrackRepositoryError::Unavailable)?;

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
    .map_err(|_| TrackRepositoryError::Unavailable)?;

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
    .map_err(|_| TrackRepositoryError::Unavailable)?;

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
    .map_err(|_| TrackRepositoryError::Unavailable)?;

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
    .map_err(|_| TrackRepositoryError::Unavailable)?;

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
    .map_err(|_| TrackRepositoryError::Unavailable)?;

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
    .map_err(|_| TrackRepositoryError::Unavailable)?;

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
    .map_err(|_| TrackRepositoryError::Unavailable)?;

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
    .map_err(|_| TrackRepositoryError::Unavailable)?;

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
    let (
        id,
        title,
        album_artist_id,
        year,
        release_type,
        label,
        group_id,
        release_id,
        artist_id,
        artist_name,
        sort_name,
        artist_identifier,
        track_count,
        duration_ms,
    ) = row;

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
            Artist::new(ArtistId::from_uuid(id), name, sort_name, artist_identifier)
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
    .map_err(|_| TrackRepositoryError::Unavailable)?;

    let mut by_track: HashMap<Uuid, Vec<ArtistCredit>> = HashMap::new();
    for (track_id, name, identifier) in rows {
        let credit =
            ArtistCredit::new(name, identifier).map_err(|_| TrackRepositoryError::InvalidData)?;

        by_track.entry(track_id).or_default().push(credit);
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
    .map_err(|_| TrackRepositoryError::Unavailable)
}

async fn insert_candidate_track(
    connection: &mut PgConnection,
    track_id: &TrackId,
    metadata: &TrackMetadata,
) -> Result<(), TrackRepositoryError> {
    sqlx::query(
        r#"
        INSERT INTO tracks (id, title, duration_ms, musicbrainz_recording_id, isrc)
        VALUES ($1, $2, $3, $4, $5)
        "#,
    )
    .bind(track_id.as_uuid())
    .bind(metadata.title())
    .bind(encode_duration_ms(metadata.duration_ms())?)
    .bind(metadata.musicbrainz_recording_id())
    .bind(metadata.isrc())
    .execute(&mut *connection)
    .await
    .map(|_| ())
    .map_err(|_| TrackRepositoryError::Unavailable)
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
    .map_err(|_| TrackRepositoryError::Unavailable)
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
    .map_err(|_| TrackRepositoryError::Unavailable)?;

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
            updated_at = CURRENT_TIMESTAMP
        WHERE id = $5
        "#,
    )
    .bind(metadata.title())
    .bind(encode_duration_ms(metadata.duration_ms())?)
    .bind(metadata.musicbrainz_recording_id())
    .bind(metadata.isrc())
    .bind(track_id.as_uuid())
    .execute(&mut *connection)
    .await
    .map_err(|_| TrackRepositoryError::Unavailable)?;

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
    .map_err(|_| TrackRepositoryError::Unavailable)?;

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
        .map_err(|_| TrackRepositoryError::Unavailable)?;
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

fn encode_source_scope(source_scope: &SourceScope) -> String {
    match source_scope {
        SourceScope::Global => "global".to_owned(),
        SourceScope::Named(name) => format!("named:{}", name),
    }
}
