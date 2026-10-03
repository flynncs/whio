use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sqlx::{PgConnection, PgPool};
use uuid::Uuid;

use crate::features::metadata::{
    domain::{
        AttemptError, Attribution, CandidateItem, FactSource, FieldId, FieldValue, FieldWrite,
        ImageFact, MatchBasis, MetadataEntityKind, MetricFact, StoredAttempt, StoredFact,
        SubjectFacts, SubjectKey, bytes_state,
    },
    licences::licence_terms,
    storage::contracts::{CommitOutcome, MetadataRepositoryError, MetadataStore},
};
use crate::features::music_library::EnrichmentGuard;
use crate::features::music_library::domain::EnrichmentSubject;

const FACT_COLUMNS: &str = "field, source_provider_id, source_external_id, \
     value_kind, value_text, value_count, value_json, unit, population, period, as_of, \
     licence_id, licence_revision, attribution_text, attribution_url, source_url, match_basis, \
     operation, terms_basis, uncertainty, image_role, image_width, image_height, \
     image_content_type, rank, score, stored_bytes, fetched_at";

const ATTEMPT_COLUMNS: &str =
    "field, source_provider_id, fetched_at, attempted_at, last_error, last_error_class";

pub struct PostgresMetadataStore {
    pool: PgPool,
    guard: Arc<dyn EnrichmentGuard>,
    max_stored_bytes: u64,
}

impl PostgresMetadataStore {
    pub fn new(pool: PgPool, guard: Arc<dyn EnrichmentGuard>, max_stored_bytes: u64) -> Self {
        Self {
            pool,
            guard,
            max_stored_bytes,
        }
    }
}

#[derive(sqlx::FromRow)]
struct FactRow {
    field: String,
    source_provider_id: String,
    source_external_id: String,
    value_kind: String,
    value_text: Option<String>,
    value_count: Option<i64>,
    value_json: Option<String>,
    unit: Option<String>,
    population: Option<String>,
    period: Option<String>,
    as_of: Option<DateTime<Utc>>,
    licence_id: String,
    licence_revision: i32,
    attribution_text: Option<String>,
    attribution_url: Option<String>,
    source_url: Option<String>,
    match_basis: String,
    operation: String,
    terms_basis: Option<String>,
    uncertainty: bool,
    image_role: Option<String>,
    image_width: Option<i32>,
    image_height: Option<i32>,
    image_content_type: Option<String>,
    rank: Option<i32>,
    score: Option<f64>,
    stored_bytes: i64,
    fetched_at: DateTime<Utc>,
}

#[derive(sqlx::FromRow)]
struct AttemptRow {
    field: String,
    source_provider_id: String,
    fetched_at: Option<DateTime<Utc>>,
    attempted_at: DateTime<Utc>,
    last_error: Option<String>,
    last_error_class: Option<String>,
}

struct FactKey<'a> {
    entity_id: Uuid,
    field: FieldId,
    source_provider_id: &'a str,
    source_external_id: &'a str,
    identity_revision: &'a str,
    language: &'a str,
    adapter_version: &'a str,
}

struct AttemptKey<'a> {
    entity_id: Uuid,
    field: FieldId,
    source_provider_id: &'a str,
    identity_revision: &'a str,
    language: &'a str,
}

struct EncodedFact {
    value_kind: &'static str,
    value_text: Option<String>,
    value_count: Option<i64>,
    value_json: Option<String>,
    unit: Option<String>,
    population: Option<String>,
    period: Option<String>,
    as_of: Option<DateTime<Utc>>,
    licence_id: String,
    licence_revision: i32,
    attribution_text: Option<String>,
    attribution_url: Option<String>,
    source_url: Option<String>,
    operation: String,
    terms_basis: Option<String>,
    uncertainty: bool,
    image_role: Option<String>,
    image_width: Option<i32>,
    image_height: Option<i32>,
    image_content_type: Option<String>,
    rank: Option<i32>,
    score: Option<f64>,
    stored_bytes: i64,
}

#[async_trait]
impl MetadataStore for PostgresMetadataStore {
    async fn read_subject(
        &self,
        key: &SubjectKey,
    ) -> Result<SubjectFacts, MetadataRepositoryError> {
        let mut transaction = self.pool.begin().await.map_err(repository_error)?;

        let rows = sqlx::query_as::<_, FactRow>(&format!(
            "SELECT {FACT_COLUMNS} FROM metadata_facts \
             WHERE entity_kind = $1 AND entity_id = $2 AND identity_revision = $3 AND language = $4 \
               AND adapter_version = $5 \
             ORDER BY field, source_provider_id, source_external_id"
        ))
        .bind(key.entity_kind.as_str())
        .bind(key.entity_id)
        .bind(&key.identity_revision)
        .bind(&key.language)
        .bind(&key.adapter_version)
        .fetch_all(&mut *transaction)
        .await
        .map_err(repository_error)?;

        let attempts = sqlx::query_as::<_, AttemptRow>(&format!(
            "SELECT {ATTEMPT_COLUMNS} FROM metadata_attempts \
             WHERE entity_kind = $1 AND entity_id = $2 AND identity_revision = $3 AND language = $4 \
             ORDER BY field, source_provider_id"
        ))
        .bind(key.entity_kind.as_str())
        .bind(key.entity_id)
        .bind(&key.identity_revision)
        .bind(&key.language)
        .fetch_all(&mut *transaction)
        .await
        .map_err(repository_error)?;

        transaction.commit().await.map_err(repository_error)?;

        let facts = rows
            .into_iter()
            .map(decode_fact)
            .collect::<Result<Vec<_>, _>>()?;
        let attempts = attempts
            .into_iter()
            .map(decode_attempt)
            .collect::<Result<Vec<_>, _>>()?;

        Ok(SubjectFacts::new(key.clone(), facts, attempts))
    }

    async fn commit_field(
        &self,
        subject: &EnrichmentSubject,
        write: &FieldWrite,
    ) -> Result<CommitOutcome, MetadataRepositoryError> {
        let encoded = encode(write)?;
        let mut transaction = self.pool.begin().await.map_err(repository_error)?;
        let guarded = self
            .guard
            .enrichment_subject(&mut transaction, subject.artist_id())
            .await
            .map_err(|_| MetadataRepositoryError::Unavailable)?;
        let Some(guarded) = guarded else {
            return Ok(CommitOutcome::SubjectGone);
        };
        if guarded.identity_revision() != subject.identity_revision() {
            return Ok(CommitOutcome::Obsolete);
        }

        let entity_id = *subject.artist_id().as_uuid();
        let fact_key = FactKey {
            entity_id,
            field: write.field,
            source_provider_id: &write.source_provider_id,
            source_external_id: &write.source_external_id,
            identity_revision: subject.identity_revision(),
            language: &write.language,
            adapter_version: &write.adapter_version,
        };
        let attempt_key = AttemptKey {
            entity_id,
            field: write.field,
            source_provider_id: &write.source_provider_id,
            identity_revision: subject.identity_revision(),
            language: &write.language,
        };

        match encoded {
            Some(encoded) => {
                let stored = stored_total(&mut transaction).await?;
                let replaced = stored_bytes_of(&mut transaction, &fact_key).await?;
                let added = u64::try_from(encoded.stored_bytes)
                    .map_err(|_| MetadataRepositoryError::InvalidData)?;
                let delta = added.saturating_sub(replaced);

                if stored.saturating_sub(replaced).saturating_add(delta) > self.max_stored_bytes {
                    record_attempt_failure(
                        &mut transaction,
                        &attempt_key,
                        write.fetched_at,
                        &AttemptError::store_budget_exceeded(),
                    )
                    .await?;
                } else {
                    write_fact(&mut transaction, &fact_key, write, &encoded).await?;
                    record_attempt_success(&mut transaction, &attempt_key, write.fetched_at)
                        .await?;
                }
            }
            None => {
                record_attempt_success(&mut transaction, &attempt_key, write.fetched_at).await?;
            }
        }

        transaction.commit().await.map_err(repository_error)?;

        Ok(CommitOutcome::Written)
    }

    async fn record_failure(
        &self,
        subject: &EnrichmentSubject,
        field: FieldId,
        language: &str,
        source_provider_id: &str,
        error: &AttemptError,
    ) -> Result<(), MetadataRepositoryError> {
        let mut transaction = self.pool.begin().await.map_err(repository_error)?;
        let guarded = self
            .guard
            .enrichment_subject(&mut transaction, subject.artist_id())
            .await
            .map_err(|_| MetadataRepositoryError::Unavailable)?;

        if guarded.is_none() {
            return Ok(());
        }

        let key = AttemptKey {
            entity_id: *subject.artist_id().as_uuid(),
            field,
            source_provider_id,
            identity_revision: subject.identity_revision(),
            language,
        };

        record_attempt_failure(&mut transaction, &key, Utc::now(), error).await?;
        transaction.commit().await.map_err(repository_error)?;

        Ok(())
    }

    async fn delete_subject_facts(
        &self,
        kind: MetadataEntityKind,
        ids: &[Uuid],
    ) -> Result<u64, MetadataRepositoryError> {
        if ids.is_empty() {
            return Ok(0);
        }

        let mut transaction = self.pool.begin().await.map_err(repository_error)?;

        let facts = sqlx::query(
            "DELETE FROM metadata_facts WHERE entity_kind = $1 AND entity_id = ANY($2)",
        )
        .bind(kind.as_str())
        .bind(ids)
        .execute(&mut *transaction)
        .await
        .map_err(repository_error)?
        .rows_affected();

        let attempts = sqlx::query(
            "DELETE FROM metadata_attempts WHERE entity_kind = $1 AND entity_id = ANY($2)",
        )
        .bind(kind.as_str())
        .bind(ids)
        .execute(&mut *transaction)
        .await
        .map_err(repository_error)?
        .rows_affected();

        transaction.commit().await.map_err(repository_error)?;

        Ok(facts.saturating_add(attempts))
    }
}

async fn stored_total(tx: &mut PgConnection) -> Result<u64, MetadataRepositoryError> {
    let total = sqlx::query_scalar::<_, i64>(
        "SELECT COALESCE(SUM(stored_bytes), 0)::BIGINT FROM metadata_facts",
    )
    .fetch_one(&mut *tx)
    .await
    .map_err(repository_error)?;

    u64::try_from(total).map_err(|_| MetadataRepositoryError::InvalidData)
}

async fn stored_bytes_of(
    tx: &mut PgConnection,
    key: &FactKey<'_>,
) -> Result<u64, MetadataRepositoryError> {
    let existing = sqlx::query_scalar::<_, i64>(
        "SELECT COALESCE(SUM(stored_bytes), 0)::BIGINT FROM metadata_facts \
         WHERE entity_kind = $1 AND entity_id = $2 AND field = $3 AND source_provider_id = $4 \
           AND source_external_id = $5 AND identity_revision = $6 AND language = $7 \
           AND adapter_version = $8",
    )
    .bind(MetadataEntityKind::Artist.as_str())
    .bind(key.entity_id)
    .bind(key.field.as_str())
    .bind(key.source_provider_id)
    .bind(key.source_external_id)
    .bind(key.identity_revision)
    .bind(key.language)
    .bind(key.adapter_version)
    .fetch_one(&mut *tx)
    .await
    .map_err(repository_error)?;

    u64::try_from(existing).map_err(|_| MetadataRepositoryError::InvalidData)
}

async fn write_fact(
    tx: &mut PgConnection,
    key: &FactKey<'_>,
    write: &FieldWrite,
    encoded: &EncodedFact,
) -> Result<(), MetadataRepositoryError> {
    sqlx::query(
        "INSERT INTO metadata_facts (\
             entity_kind, entity_id, field, source_provider_id, source_external_id, \
             identity_revision, language, adapter_version, value_kind, value_text, value_count, \
             value_json, unit, population, period, as_of, licence_id, licence_revision, \
             attribution_text, attribution_url, source_url, match_basis, operation, terms_basis, \
             uncertainty, image_role, image_width, image_height, image_content_type, rank, score, \
             stored_bytes, fetched_at\
         ) VALUES (\
             $1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18, \
             $19, $20, $21, $22, $23, $24, $25, $26, $27, $28, $29, $30, $31, $32, $33\
         ) ON CONFLICT (\
             entity_kind, entity_id, field, source_provider_id, source_external_id, \
             identity_revision, language, adapter_version\
         ) DO UPDATE SET \
             value_kind = EXCLUDED.value_kind, value_text = EXCLUDED.value_text, \
             value_count = EXCLUDED.value_count, value_json = EXCLUDED.value_json, \
             unit = EXCLUDED.unit, population = EXCLUDED.population, period = EXCLUDED.period, \
             as_of = EXCLUDED.as_of, licence_id = EXCLUDED.licence_id, \
             licence_revision = EXCLUDED.licence_revision, \
             attribution_text = EXCLUDED.attribution_text, \
             attribution_url = EXCLUDED.attribution_url, source_url = EXCLUDED.source_url, \
             match_basis = EXCLUDED.match_basis, operation = EXCLUDED.operation, \
             terms_basis = EXCLUDED.terms_basis, uncertainty = EXCLUDED.uncertainty, \
             image_role = EXCLUDED.image_role, image_width = EXCLUDED.image_width, \
             image_height = EXCLUDED.image_height, \
             image_content_type = EXCLUDED.image_content_type, rank = EXCLUDED.rank, \
             score = EXCLUDED.score, stored_bytes = EXCLUDED.stored_bytes, \
             fetched_at = EXCLUDED.fetched_at",
    )
    .bind(MetadataEntityKind::Artist.as_str())
    .bind(key.entity_id)
    .bind(key.field.as_str())
    .bind(key.source_provider_id)
    .bind(key.source_external_id)
    .bind(key.identity_revision)
    .bind(key.language)
    .bind(key.adapter_version)
    .bind(encoded.value_kind)
    .bind(encoded.value_text.as_deref())
    .bind(encoded.value_count)
    .bind(encoded.value_json.as_deref())
    .bind(encoded.unit.as_deref())
    .bind(encoded.population.as_deref())
    .bind(encoded.period.as_deref())
    .bind(encoded.as_of)
    .bind(&encoded.licence_id)
    .bind(encoded.licence_revision)
    .bind(encoded.attribution_text.as_deref())
    .bind(encoded.attribution_url.as_deref())
    .bind(encoded.source_url.as_deref())
    .bind(write.match_basis.as_str())
    .bind(&encoded.operation)
    .bind(encoded.terms_basis.as_deref())
    .bind(encoded.uncertainty)
    .bind(encoded.image_role.as_deref())
    .bind(encoded.image_width)
    .bind(encoded.image_height)
    .bind(encoded.image_content_type.as_deref())
    .bind(encoded.rank)
    .bind(encoded.score)
    .bind(encoded.stored_bytes)
    .bind(write.fetched_at)
    .execute(&mut *tx)
    .await
    .map_err(repository_error)?;

    Ok(())
}

async fn record_attempt_success(
    tx: &mut PgConnection,
    key: &AttemptKey<'_>,
    at: DateTime<Utc>,
) -> Result<(), MetadataRepositoryError> {
    sqlx::query(
        "INSERT INTO metadata_attempts (\
             entity_kind, entity_id, field, source_provider_id, identity_revision, language, \
             fetched_at, attempted_at, last_error, last_error_class\
         ) VALUES ($1, $2, $3, $4, $5, $6, $7, $7, NULL, NULL) \
         ON CONFLICT (\
             entity_kind, entity_id, field, source_provider_id, identity_revision, language\
         ) DO UPDATE SET fetched_at = EXCLUDED.fetched_at, attempted_at = EXCLUDED.attempted_at, \
             last_error = NULL, last_error_class = NULL",
    )
    .bind(MetadataEntityKind::Artist.as_str())
    .bind(key.entity_id)
    .bind(key.field.as_str())
    .bind(key.source_provider_id)
    .bind(key.identity_revision)
    .bind(key.language)
    .bind(at)
    .execute(&mut *tx)
    .await
    .map_err(repository_error)?;

    Ok(())
}

async fn record_attempt_failure(
    tx: &mut PgConnection,
    key: &AttemptKey<'_>,
    at: DateTime<Utc>,
    error: &AttemptError,
) -> Result<(), MetadataRepositoryError> {
    sqlx::query(
        "INSERT INTO metadata_attempts (\
             entity_kind, entity_id, field, source_provider_id, identity_revision, language, \
             fetched_at, attempted_at, last_error, last_error_class\
         ) VALUES ($1, $2, $3, $4, $5, $6, NULL, $7, $8, $9) \
         ON CONFLICT (\
             entity_kind, entity_id, field, source_provider_id, identity_revision, language\
         ) DO UPDATE SET attempted_at = EXCLUDED.attempted_at, \
             last_error = EXCLUDED.last_error, last_error_class = EXCLUDED.last_error_class",
    )
    .bind(MetadataEntityKind::Artist.as_str())
    .bind(key.entity_id)
    .bind(key.field.as_str())
    .bind(key.source_provider_id)
    .bind(key.identity_revision)
    .bind(key.language)
    .bind(at)
    .bind(&error.message)
    .bind(&error.class)
    .execute(&mut *tx)
    .await
    .map_err(repository_error)?;

    Ok(())
}

fn encode(write: &FieldWrite) -> Result<Option<EncodedFact>, MetadataRepositoryError> {
    let Some(value) = &write.value else {
        return Ok(None);
    };

    if value.kind() != write.field.kind() {
        return Err(MetadataRepositoryError::InvalidData);
    }

    let licence_id = write
        .licence_id
        .clone()
        .ok_or(MetadataRepositoryError::InvalidData)?;
    let licence_revision = write
        .licence_revision
        .ok_or(MetadataRepositoryError::InvalidData)?;
    licence_terms(&licence_id, licence_revision).ok_or(MetadataRepositoryError::InvalidData)?;
    let licence_revision =
        i32::try_from(licence_revision).map_err(|_| MetadataRepositoryError::InvalidData)?;

    let (value_text, value_count, value_json, value_bytes) = match value {
        FieldValue::Text(text) | FieldValue::Reference(text) => {
            (Some(text.clone()), None, None, text.len())
        }
        FieldValue::Count(count) => (
            None,
            Some(i64::try_from(*count).map_err(|_| MetadataRepositoryError::InvalidData)?),
            None,
            std::mem::size_of::<i64>(),
        ),
        FieldValue::Candidates(items) => {
            let json =
                serde_json::to_string(items).map_err(|_| MetadataRepositoryError::InvalidData)?;
            let bytes = json.len();

            (None, None, Some(json), bytes)
        }
    };

    let (image_role, image_width, image_height, image_content_type) = match &write.image {
        Some(image) => (
            Some(image.role.clone()),
            image
                .width
                .map(i32::try_from)
                .transpose()
                .map_err(|_| MetadataRepositoryError::InvalidData)?,
            image
                .height
                .map(i32::try_from)
                .transpose()
                .map_err(|_| MetadataRepositoryError::InvalidData)?,
            Some(image.content_type.clone()),
        ),
        None => (None, None, None, None),
    };

    let (unit, population, period, as_of) = match &write.metric {
        Some(metric) => (
            Some(metric.unit.clone()),
            metric.population.clone(),
            metric.period.clone(),
            metric.as_of,
        ),
        None => (None, None, None, None),
    };

    let rank = write
        .rank
        .map(i32::try_from)
        .transpose()
        .map_err(|_| MetadataRepositoryError::InvalidData)?;

    let mut stored_bytes = value_bytes;
    stored_bytes += write.source_provider_id.len();
    stored_bytes += write.source_external_id.len();
    stored_bytes += write.adapter_version.len();
    stored_bytes += write.language.len();
    stored_bytes += write.operation.len();
    if let Some(licence_id) = &write.licence_id {
        stored_bytes += licence_id.len();
    }
    if let Some(source_url) = &write.source_url {
        stored_bytes += source_url.len();
    }
    if let Some(terms_basis) = &write.terms_basis {
        stored_bytes += terms_basis.len();
    }
    if let Some(attribution) = &write.attribution {
        stored_bytes += attribution.text.len();
        if let Some(url) = &attribution.url {
            stored_bytes += url.len();
        }
    }
    if let Some(image) = &write.image {
        stored_bytes += image.role.len() + image.content_type.len();
    }
    if let Some(metric) = &write.metric {
        stored_bytes += metric.unit.len();
        if let Some(population) = &metric.population {
            stored_bytes += population.len();
        }
        if let Some(period) = &metric.period {
            stored_bytes += period.len();
        }
    }

    Ok(Some(EncodedFact {
        value_kind: value.kind().as_str(),
        value_text,
        value_count,
        value_json,
        unit,
        population,
        period,
        as_of,
        licence_id,
        licence_revision,
        attribution_text: write.attribution.as_ref().map(|credit| credit.text.clone()),
        attribution_url: write
            .attribution
            .as_ref()
            .and_then(|credit| credit.url.clone()),
        source_url: write.source_url.clone(),
        operation: write.operation.clone(),
        terms_basis: write.terms_basis.clone(),
        uncertainty: write.uncertainty,
        image_role,
        image_width,
        image_height,
        image_content_type,
        rank,
        score: write.score,
        stored_bytes: i64::try_from(stored_bytes)
            .map_err(|_| MetadataRepositoryError::InvalidData)?,
    }))
}

fn decode_fact(row: FactRow) -> Result<StoredFact, MetadataRepositoryError> {
    let field = FieldId::parse(&row.field).ok_or(MetadataRepositoryError::InvalidData)?;
    let value = match row.value_kind.as_str() {
        "text" => FieldValue::Text(row.value_text.ok_or(MetadataRepositoryError::InvalidData)?),
        "reference" => {
            FieldValue::Reference(row.value_text.ok_or(MetadataRepositoryError::InvalidData)?)
        }
        "count" => FieldValue::Count(
            u64::try_from(
                row.value_count
                    .ok_or(MetadataRepositoryError::InvalidData)?,
            )
            .map_err(|_| MetadataRepositoryError::InvalidData)?,
        ),
        "candidate_list" => FieldValue::Candidates(
            serde_json::from_str::<Vec<CandidateItem>>(
                &row.value_json.ok_or(MetadataRepositoryError::InvalidData)?,
            )
            .map_err(|_| MetadataRepositoryError::InvalidData)?,
        ),
        _ => return Err(MetadataRepositoryError::InvalidData),
    };
    let match_basis =
        MatchBasis::parse(&row.match_basis).ok_or(MetadataRepositoryError::InvalidData)?;
    let licence_revision =
        u32::try_from(row.licence_revision).map_err(|_| MetadataRepositoryError::InvalidData)?;
    let licence = licence_terms(&row.licence_id, licence_revision)
        .ok_or(MetadataRepositoryError::InvalidData)?;
    let image = match (row.image_role, row.image_content_type) {
        (Some(role), Some(content_type)) => Some(ImageFact {
            role,
            width: decode_optional_u32(row.image_width)?,
            height: decode_optional_u32(row.image_height)?,
            content_type,
        }),
        _ => None,
    };
    let metric = match row.unit {
        Some(unit) => Some(MetricFact {
            unit,
            population: row.population.clone(),
            period: row.period.clone(),
            as_of: row.as_of,
        }),
        None => None,
    };
    let rank = row
        .rank
        .map(u32::try_from)
        .transpose()
        .map_err(|_| MetadataRepositoryError::InvalidData)?;
    let stored_bytes =
        u64::try_from(row.stored_bytes).map_err(|_| MetadataRepositoryError::InvalidData)?;
    let bytes = bytes_state(field, &licence);

    Ok(StoredFact {
        field,
        source: FactSource {
            provider_id: row.source_provider_id,
            external_id: row.source_external_id,
            url: row.source_url.clone(),
            match_basis,
        },
        value,
        licence,
        attribution: decode_attribution(row.attribution_text, row.attribution_url)?,
        source_url: row.source_url,
        operation: row.operation,
        terms_basis: row.terms_basis,
        uncertainty: row.uncertainty,
        image,
        metric,
        rank,
        score: row.score,
        bytes,
        stored_bytes,
        fetched_at: row.fetched_at,
    })
}

fn decode_attempt(row: AttemptRow) -> Result<StoredAttempt, MetadataRepositoryError> {
    let field = FieldId::parse(&row.field).ok_or(MetadataRepositoryError::InvalidData)?;
    let error = match (row.last_error, row.last_error_class) {
        (Some(message), Some(class)) => Some(AttemptError { class, message }),
        (None, None) => None,
        _ => return Err(MetadataRepositoryError::InvalidData),
    };

    Ok(StoredAttempt {
        field,
        source_provider_id: row.source_provider_id,
        fetched_at: row.fetched_at,
        attempted_at: row.attempted_at,
        error,
    })
}

fn decode_optional_u32(value: Option<i32>) -> Result<Option<u32>, MetadataRepositoryError> {
    value
        .map(u32::try_from)
        .transpose()
        .map_err(|_| MetadataRepositoryError::InvalidData)
}

fn decode_attribution(
    text: Option<String>,
    url: Option<String>,
) -> Result<Option<Attribution>, MetadataRepositoryError> {
    match (text, url) {
        (Some(text), url) => Ok(Some(Attribution { text, url })),
        (None, None) => Ok(None),
        (None, Some(_)) => Err(MetadataRepositoryError::InvalidData),
    }
}

fn repository_error(error: sqlx::Error) -> MetadataRepositoryError {
    if let sqlx::Error::Database(database) = &error
        && (database.is_unique_violation()
            || database.is_check_violation()
            || database.is_foreign_key_violation())
    {
        return MetadataRepositoryError::InvalidData;
    }

    MetadataRepositoryError::Unavailable
}
