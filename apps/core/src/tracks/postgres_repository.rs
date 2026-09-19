use async_trait::async_trait;
use sqlx::{PgConnection, PgPool};
use uuid::Uuid;

use crate::tracks::{
    Album, AlbumId, AlbumMetadata, AlbumTrack, Artist, ArtistCredit, ArtistId, ArtworkRef,
    ProviderId, SourceFileFacts, SourceIdentity, SourceScope, Track, TrackId, TrackMetadata,
    TrackRepository, TrackRepositoryError,
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

            insert_candidate_track(&mut tx, &candidate_track_id, &source, &metadata).await?;

            if try_insert_source(&mut tx, &candidate_track_id, &source).await? {
                candidate_track_id
            } else {
                resolve_concurrent_winner(&mut tx, &candidate_track_id, &source).await?
            }
        };

        let album = update_track_metadata(&mut tx, &track_id, &source, &metadata).await?;
        replace_artist_credits(&mut tx, &track_id, &metadata).await?;

        tx.commit()
            .await
            .map_err(|_| TrackRepositoryError::Unavailable)?;

        let metadata = metadata
            .with_album(album)
            .map_err(|_| TrackRepositoryError::InvalidData)?;

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
        let row: Option<(String, Option<i64>, Option<String>)> = sqlx::query_as(
            r#"SELECT title, duration_ms, album
            FROM tracks
            WHERE id = $1
            "#,
        )
        .bind(track_id.as_uuid())
        .fetch_optional(&self.pool)
        .await
        .map_err(|_| TrackRepositoryError::Unavailable)?;

        let Some((title, duration_ms, album)) = row else {
            return Ok(None);
        };

        let artists: Vec<String> = sqlx::query_scalar(
            r#"SELECT name
            FROM track_artist_credits
            WHERE track_id = $1
            ORDER BY position
            "#,
        )
        .bind(track_id.as_uuid())
        .fetch_all(&self.pool)
        .await
        .map_err(|_| TrackRepositoryError::Unavailable)?;

        let duration_ms = duration_ms
            .map(u64::try_from)
            .transpose()
            .map_err(|_| TrackRepositoryError::InvalidData)?;
        let metadata = TrackMetadata::new(title, artists, duration_ms)
            .map_err(|_| TrackRepositoryError::InvalidData)?
            .with_album(album)
            .map_err(|_| TrackRepositoryError::InvalidData)?;

        Ok(Some(Track::new(
            TrackId::from_uuid(*track_id.as_uuid()),
            metadata,
        )))
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
                content_type = $5, suffix = $6
            WHERE provider_id = $7 AND source_scope = $8 AND external_id = $9
            "#,
        )
        .bind(facts.path.as_deref())
        .bind(facts.size_bytes as i64)
        .bind(facts.modified_at)
        .bind(facts.missing_at)
        .bind(facts.content_type.as_deref())
        .bind(facts.suffix.as_deref())
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
            ),
        >(
            r#"SELECT path, size_bytes, modified_at, missing_at, content_type, suffix
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
            |(path, size_bytes, modified_at, missing_at, content_type, suffix)| SourceFileFacts {
                path,
                size_bytes: size_bytes.unwrap_or_default().max(0) as u64,
                modified_at,
                missing_at,
                content_type,
                suffix,
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
            ),
        >(
            r#"SELECT provider_id, source_scope, external_id,
                      path, size_bytes, modified_at, missing_at, content_type, suffix
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

    async fn find_album_tracks(
        &self,
        album_id: &AlbumId,
    ) -> Result<Vec<(TrackId, AlbumTrack)>, TrackRepositoryError> {
        let rows = sqlx::query_as::<_, (Uuid, Option<i16>, Option<i16>, i32)>(
            r#"
            SELECT track_id, disc_number, track_number, position
            FROM album_tracks
            WHERE album_id = $1
            ORDER BY position, track_id
            "#,
        )
        .bind(album_id.as_uuid())
        .fetch_all(&self.pool)
        .await
        .map_err(|_| TrackRepositoryError::Unavailable)?;

        rows.into_iter()
            .map(|(track_id, disc_number, track_number, position)| {
                let disc_number = decode_smallint(disc_number)?;
                let track_number = decode_smallint(track_number)?;
                let position =
                    u32::try_from(position).map_err(|_| TrackRepositoryError::InvalidData)?;
                let membership = AlbumTrack::new(disc_number, track_number, position)
                    .map_err(|_| TrackRepositoryError::InvalidData)?;

                Ok((TrackId::from_uuid(track_id), membership))
            })
            .collect()
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

const ARTIST_COLUMNS: &str = "id, name, sort_name, musicbrainz_artist_id";
const ALBUM_COLUMNS: &str = "id, title, album_artist_id, year, release_type, label, \
     musicbrainz_release_group_id, musicbrainz_release_id";

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
    source: &SourceIdentity,
    metadata: &TrackMetadata,
) -> Result<(), TrackRepositoryError> {
    sqlx::query(
        r#"
        INSERT INTO tracks (id, title, duration_ms, album, album_source)
        VALUES ($1, $2, $3, $4, $5)
        "#,
    )
    .bind(track_id.as_uuid())
    .bind(metadata.title())
    .bind(encode_duration_ms(metadata.duration_ms())?)
    .bind(metadata.album())
    .bind(metadata.album().map(|_| source.provider_id().as_str()))
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

/// merges display metadata and returns the album that won provider precedence.
async fn update_track_metadata(
    connection: &mut PgConnection,
    track_id: &TrackId,
    source: &SourceIdentity,
    metadata: &TrackMetadata,
) -> Result<Option<String>, TrackRepositoryError> {
    let (existing_album, existing_source): (Option<String>, Option<String>) = sqlx::query_as(
        r#"
        SELECT album, album_source
        FROM tracks
        WHERE id = $1
        FOR UPDATE
        "#,
    )
    .bind(track_id.as_uuid())
    .fetch_one(&mut *connection)
    .await
    .map_err(|_| TrackRepositoryError::Unavailable)?;

    let (album, album_source) = match metadata.album() {
        None => (existing_album, existing_source),
        Some(incoming) => {
            let existing_rank = existing_source
                .as_deref()
                .and_then(|value| ProviderId::new(value.to_owned()).ok())
                .map_or(u8::MAX, |provider| provider.precedence());

            // a higher-precedence provider replaces the album; within one
            // provider the fresher observation wins.
            if existing_album.is_some() && existing_rank < source.provider_id().precedence() {
                (existing_album, existing_source)
            } else {
                (
                    Some(incoming.to_owned()),
                    Some(source.provider_id().as_str().to_owned()),
                )
            }
        }
    };

    let result = sqlx::query(
        r#"
        UPDATE tracks
        SET title = $1,
            duration_ms = $2,
            album = $3,
            album_source = $4,
            updated_at = CURRENT_TIMESTAMP
        WHERE id = $5
        "#,
    )
    .bind(metadata.title())
    .bind(encode_duration_ms(metadata.duration_ms())?)
    .bind(album.as_deref())
    .bind(album_source.as_deref())
    .bind(track_id.as_uuid())
    .execute(&mut *connection)
    .await
    .map_err(|_| TrackRepositoryError::Unavailable)?;

    if result.rows_affected() != 1 {
        return Err(TrackRepositoryError::InvalidData);
    }

    Ok(album)
}

async fn replace_artist_credits(
    connection: &mut PgConnection,
    track_id: &TrackId,
    metadata: &TrackMetadata,
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

    for (position, artist) in metadata.artists().iter().enumerate() {
        let position = i32::try_from(position).map_err(|_| TrackRepositoryError::InvalidData)?;

        sqlx::query(
            r#"
            INSERT INTO track_artist_credits (track_id, position, name)
            VALUES ($1, $2, $3)
            "#,
        )
        .bind(track_id.as_uuid())
        .bind(position)
        .bind(artist)
        .execute(&mut *connection)
        .await
        .map_err(|_| TrackRepositoryError::Unavailable)?;
    }

    Ok(())
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
