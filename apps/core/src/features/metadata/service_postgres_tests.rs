use std::sync::Arc;

use async_trait::async_trait;
use chrono::Utc;
use sqlx::PgPool;
use uuid::Uuid;

use crate::features::metadata::{
    capabilities::{
        CAPABILITY_TTL, CapabilityError, CapabilitySnapshot, CapabilitySource, Snapshot,
    },
    domain::{
        AttemptError, CandidateItem, FieldId, FieldValue, FieldWrite, ImageFact, MatchBasis,
        MetadataEntityKind, StoredFact, SubjectFacts, SubjectKey,
    },
    freshness::NoJitter,
    refresh::{
        CandidateSource, DocumentCandidate, DocumentError, DocumentFieldError, DocumentOutcome,
        FieldRefreshOutcome, MetadataDocuments, RefreshReport, RefreshRequest,
    },
    selection::{FieldOrder, WIKIPEDIA, YOUTUBE_MUSIC},
    service::{
        MetadataError, MetadataProfile, MetadataService, RefreshOutcome, RefreshPhase,
        RefreshReason,
    },
    storage::{CommitOutcome, MetadataRepositoryError, MetadataStore, PostgresMetadataStore},
};
use crate::features::music_library::{
    EnrichmentGuard,
    domain::{ArtistCredit, EnrichmentSubject, ProviderId, SourceIdentity, SourceScope},
    test_support::{CatalogueWrite, PostgresTrackRepository},
};
use crate::jobs::metadata_refresh::{
    METADATA_REFRESH_KIND, METADATA_REFRESH_QUEUE, MetadataRefreshV1,
};
use crate::platform::jobs::Enqueued;

const MAX_STORED_BYTES: u64 = 4 * 1024 * 1024;

struct UnsupportedDocuments;

#[async_trait]
impl MetadataDocuments for UnsupportedDocuments {
    async fn fetch(
        &self,
        _source: &SourceIdentity,
        _profile: &MetadataProfile,
        _language: &str,
        _fields: &[FieldId],
    ) -> Result<DocumentOutcome, DocumentError> {
        Err(DocumentError::Unsupported)
    }
}

struct AnsweringDocuments(DocumentOutcome);

#[async_trait]
impl MetadataDocuments for AnsweringDocuments {
    async fn fetch(
        &self,
        _source: &SourceIdentity,
        _profile: &MetadataProfile,
        _language: &str,
        _fields: &[FieldId],
    ) -> Result<DocumentOutcome, DocumentError> {
        Ok(self.0.clone())
    }
}

struct StaticCapabilities(Vec<String>);

#[async_trait]
impl CapabilitySource for StaticCapabilities {
    async fn fetch(&self) -> Result<Snapshot, CapabilityError> {
        Ok(Snapshot::new(self.0.clone()))
    }
}

struct MissingStore;

#[async_trait]
impl MetadataStore for MissingStore {
    async fn read_subject(
        &self,
        _key: &SubjectKey,
    ) -> Result<SubjectFacts, MetadataRepositoryError> {
        Err(MetadataRepositoryError::Unavailable)
    }

    async fn commit_field(
        &self,
        _subject: &EnrichmentSubject,
        _write: &FieldWrite,
    ) -> Result<CommitOutcome, MetadataRepositoryError> {
        Err(MetadataRepositoryError::Unavailable)
    }

    async fn record_failure(
        &self,
        _subject: &EnrichmentSubject,
        _field: FieldId,
        _language: &str,
        _source_provider_id: &str,
        _error: &AttemptError,
    ) -> Result<(), MetadataRepositoryError> {
        Err(MetadataRepositoryError::Unavailable)
    }

    async fn delete_subject_facts(
        &self,
        _kind: MetadataEntityKind,
        _ids: &[Uuid],
    ) -> Result<u64, MetadataRepositoryError> {
        Err(MetadataRepositoryError::Unavailable)
    }
}

fn source(provider_id: &str, external_id: &str) -> SourceIdentity {
    SourceIdentity::new(
        ProviderId::new(provider_id.to_owned()).expect("a provider id"),
        SourceScope::Global,
        external_id.to_owned(),
    )
    .expect("a source identity")
}

fn order_for(field: FieldId, providers: &[&str]) -> FieldOrder {
    FieldOrder::from_entries([(
        field,
        providers
            .iter()
            .map(|provider| (*provider).to_owned())
            .collect::<Vec<_>>(),
    )])
}

async fn artist_subject(
    pool: &PgPool,
    repository: &PostgresTrackRepository,
    musicbrainz_artist_id: &str,
) -> EnrichmentSubject {
    let credit = ArtistCredit::new(
        "Daft Punk".to_owned(),
        Some(musicbrainz_artist_id.to_owned()),
    )
    .expect("an artist credit");
    let artist = repository
        .resolve_or_create_artist(&credit)
        .await
        .expect("the artist");

    repository
        .set_artist_source(artist.id(), &source(WIKIPEDIA, musicbrainz_artist_id))
        .await
        .expect("the artist source");

    let mut transaction = pool.begin().await.expect("a transaction");
    let subject = repository
        .enrichment_subject(&mut transaction, artist.id())
        .await
        .expect("a subject")
        .expect("the artist exists");

    transaction.commit().await.expect("a commit");

    subject
}

fn store(pool: &PgPool, repository: &Arc<PostgresTrackRepository>) -> Arc<PostgresMetadataStore> {
    Arc::new(PostgresMetadataStore::new(
        pool.clone(),
        Arc::clone(repository) as Arc<dyn EnrichmentGuard>,
        MAX_STORED_BYTES,
    ))
}

fn service(
    pool: &PgPool,
    store: Arc<dyn MetadataStore>,
    order: FieldOrder,
    providers: &[&str],
) -> MetadataService {
    service_with(
        pool,
        store,
        Arc::new(UnsupportedDocuments),
        order,
        providers,
    )
}

fn service_with(
    pool: &PgPool,
    store: Arc<dyn MetadataStore>,
    documents: Arc<dyn MetadataDocuments>,
    order: FieldOrder,
    providers: &[&str],
) -> MetadataService {
    MetadataService::new(
        pool.clone(),
        store,
        documents,
        Arc::new(CapabilitySnapshot::new(
            Arc::new(StaticCapabilities(
                providers
                    .iter()
                    .map(|provider| (*provider).to_owned())
                    .collect(),
            )),
            CAPABILITY_TTL,
        )),
        order,
        "v1".to_owned(),
        Arc::new(NoJitter),
    )
}

fn at(seconds: i64) -> chrono::DateTime<Utc> {
    chrono::DateTime::from_timestamp(1_800_000_000 + seconds, 0).expect("a valid instant")
}

fn candidate(
    field: FieldId,
    provider_id: &str,
    external_id: &str,
    value: FieldValue,
) -> DocumentCandidate {
    let terms = crate::features::metadata::licences::licence_terms(provider_id, 1)
        .expect("the registry carries the licence");

    DocumentCandidate {
        field,
        source: CandidateSource {
            provider_id: provider_id.to_owned(),
            external_id: external_id.to_owned(),
            url: None,
            match_basis: MatchBasis::Identifier,
        },
        value,
        language: Some("en".to_owned()),
        licence: crate::features::metadata::licences::LicenceAssertion {
            id: terms.id,
            url: terms.url,
            attribution_required: terms.attribution_required,
            redistribution_allowed: terms.redistribution_allowed,
            cache_allowed: terms.cache_allowed,
            share_alike: terms.share_alike,
            non_commercial: terms.non_commercial,
            storage_cap_bytes: terms.storage_cap_bytes,
        },
        attribution: None,
        source_url: None,
        rank: None,
        score: None,
        image: image_member(field),
        metric: None,
    }
}

fn image_member(field: FieldId) -> Option<ImageFact> {
    matches!(field, FieldId::ImagePortrait | FieldId::ImageWide).then(|| ImageFact {
        role: "portrait".to_owned(),
        width: Some(640),
        height: Some(640),
        content_type: "image/jpeg".to_owned(),
    })
}

fn request(subject: EnrichmentSubject) -> RefreshRequest {
    RefreshRequest {
        subject,
        profile: MetadataProfile::artist_display(),
        language: "en".to_owned(),
        requested_at: at(0),
        now: at(0),
    }
}

fn outcome(report: &RefreshReport, field: FieldId) -> FieldRefreshOutcome {
    match report {
        RefreshReport::Applied { fields } => {
            fields
                .iter()
                .find(|entry| entry.field == field)
                .expect("the report names every requested field")
                .outcome
        }
        other => panic!("expected an applied report, found {other:?}"),
    }
}

fn write(field: FieldId, provider_id: &str, external_id: &str, value: FieldValue) -> FieldWrite {
    FieldWrite {
        field,
        source_provider_id: provider_id.to_owned(),
        source_external_id: external_id.to_owned(),
        adapter_version: "v1".to_owned(),
        language: "en".to_owned(),
        value: Some(value),
        licence_id: Some(provider_id.to_owned()),
        licence_revision: Some(1),
        attribution: None,
        source_url: None,
        match_basis: MatchBasis::Identifier,
        operation: "artist-display.v1".to_owned(),
        terms_basis: None,
        uncertainty: false,
        image: None,
        metric: None,
        rank: None,
        score: None,
        fetched_at: Utc::now(),
    }
}

fn fresh_summary_write(provider_id: &str, external_id: &str) -> FieldWrite {
    write(
        FieldId::BiographySummary,
        provider_id,
        external_id,
        FieldValue::Text("a summary".to_owned()),
    )
}

async fn non_terminal_jobs(pool: &PgPool, operation_id: Uuid) -> i64 {
    sqlx::query_scalar::<_, i64>(
        "SELECT count(*) FROM awa.jobs \
         WHERE kind = $1 AND args->>'operation_id' = $2 \
           AND state::text IN ('scheduled', 'available', 'running')",
    )
    .bind(METADATA_REFRESH_KIND)
    .bind(operation_id.to_string())
    .fetch_one(pool)
    .await
    .expect("the job count")
}

#[sqlx::test]
async fn a_cold_request_inserts_once_and_coalesces_on_the_second_call(pool: PgPool) {
    awa::migrations::run(&pool).await.expect("the awa schema");

    let repository = Arc::new(PostgresTrackRepository::new(pool.clone()));
    let store = store(&pool, &repository);
    let subject = artist_subject(&pool, &repository, "mbid-daft-punk").await;
    let service = service(
        &pool,
        store,
        order_for(FieldId::BiographySummary, &[WIKIPEDIA]),
        &[WIKIPEDIA],
    );
    let profile = MetadataProfile::artist_display();

    assert_eq!(
        service
            .request_refresh(&subject, &profile, "en", RefreshReason::Read)
            .await
            .expect("the first request"),
        RefreshOutcome::Inserted
    );

    let operation_id = service.payload(&subject, &profile, "en").operation_id;

    assert_eq!(non_terminal_jobs(&pool, operation_id).await, 1);

    assert_eq!(
        service
            .request_refresh(&subject, &profile, "en", RefreshReason::Read)
            .await
            .expect("the second request"),
        RefreshOutcome::Coalesced,
        "a second identical request coalesces into the non-terminal job"
    );
    assert_eq!(non_terminal_jobs(&pool, operation_id).await, 1);

    let state = service
        .refresh_state(&subject, &profile, "en")
        .await
        .expect("the refresh state");

    assert_eq!(state.phase, RefreshPhase::Pending);
    assert_eq!(state.identity_revision, subject.identity_revision());
    assert!(state.job_id.is_some());
}

#[sqlx::test]
async fn a_warm_subject_enqueues_nothing(pool: PgPool) {
    awa::migrations::run(&pool).await.expect("the awa schema");

    let repository = Arc::new(PostgresTrackRepository::new(pool.clone()));
    let store = store(&pool, &repository);
    let subject = artist_subject(&pool, &repository, "mbid-daft-punk").await;
    let store_for_write: Arc<dyn MetadataStore> = Arc::clone(&store) as Arc<dyn MetadataStore>;

    store_for_write
        .commit_field(
            &subject,
            &fresh_summary_write(
                WIKIPEDIA,
                subject.musicbrainz_artist_id().unwrap_or_default(),
            ),
        )
        .await
        .expect("a fresh fact");

    let service = service(
        &pool,
        store_for_write,
        order_for(FieldId::BiographySummary, &[WIKIPEDIA]),
        &[WIKIPEDIA],
    );
    let profile = MetadataProfile::artist_display();

    assert_eq!(
        service
            .request_refresh(&subject, &profile, "en", RefreshReason::Read)
            .await
            .expect("a warm request"),
        RefreshOutcome::Deferred,
        "a fresh subject enqueues no job"
    );

    let operation_id = service.payload(&subject, &profile, "en").operation_id;

    assert_eq!(non_terminal_jobs(&pool, operation_id).await, 0);
    assert_eq!(
        service
            .refresh_state(&subject, &profile, "en")
            .await
            .expect("the refresh state")
            .phase,
        RefreshPhase::Idle
    );
}

#[sqlx::test]
async fn two_identical_payloads_hold_one_job_and_a_terminal_state_releases_it(pool: PgPool) {
    awa::migrations::run(&pool).await.expect("the awa schema");

    let entity_id = Uuid::now_v7();
    let payload = MetadataRefreshV1::for_artist(entity_id, "revision", "en", "artist-display.v1");

    let (first, second) = tokio::join!(
        crate::jobs::metadata_refresh::request(&pool, &payload),
        crate::jobs::metadata_refresh::request(&pool, &payload),
    );
    let inserted = [first, second]
        .into_iter()
        .filter(|enqueued| matches!(enqueued, Ok(Enqueued::Inserted { .. })))
        .count();

    assert_eq!(inserted, 1, "two identical payloads insert one job");
    assert_eq!(
        non_terminal_jobs(&pool, payload.operation_id).await,
        1,
        "the two calls hold one non-terminal job"
    );

    let cancelled = awa::admin::drain_queue(&pool, METADATA_REFRESH_QUEUE)
        .await
        .expect("the queue drains");

    assert!(cancelled >= 1, "the non-terminal job is cancelled");
    assert_eq!(non_terminal_jobs(&pool, payload.operation_id).await, 0);

    let third = crate::jobs::metadata_refresh::request(&pool, &payload)
        .await
        .expect("a post-terminal request");

    assert!(
        matches!(third, Enqueued::Inserted { .. }),
        "a terminal job releases the coalescing claim"
    );
}

#[sqlx::test]
async fn an_unadvertised_source_reports_unavailable_without_a_job(pool: PgPool) {
    awa::migrations::run(&pool).await.expect("the awa schema");

    let repository = Arc::new(PostgresTrackRepository::new(pool.clone()));
    let store = store(&pool, &repository);
    let subject = artist_subject(&pool, &repository, "mbid-daft-punk").await;
    let service = service(
        &pool,
        Arc::clone(&store) as Arc<dyn MetadataStore>,
        order_for(FieldId::BiographySummary, &[WIKIPEDIA]),
        &[YOUTUBE_MUSIC],
    );
    let profile = MetadataProfile::artist_display();

    assert_eq!(
        service
            .request_refresh(&subject, &profile, "en", RefreshReason::Read)
            .await
            .expect("a request with no advertised source"),
        RefreshOutcome::Unavailable
    );

    let operation_id = service.payload(&subject, &profile, "en").operation_id;

    assert_eq!(
        non_terminal_jobs(&pool, operation_id).await,
        0,
        "no advertised source means no job"
    );
    assert_eq!(
        service
            .refresh_state(&subject, &profile, "en")
            .await
            .expect("the refresh state")
            .phase,
        RefreshPhase::Unavailable
    );
}

#[sqlx::test]
async fn a_store_failure_is_reported_as_unavailable(pool: PgPool) {
    awa::migrations::run(&pool).await.expect("the awa schema");

    let repository = Arc::new(PostgresTrackRepository::new(pool.clone()));
    let subject = artist_subject(&pool, &repository, "mbid-daft-punk").await;
    let service = service(
        &pool,
        Arc::new(MissingStore),
        order_for(FieldId::BiographySummary, &[WIKIPEDIA]),
        &[WIKIPEDIA],
    );

    assert_eq!(
        service
            .request_refresh(
                &subject,
                &MetadataProfile::artist_display(),
                "en",
                RefreshReason::Read
            )
            .await,
        Err(MetadataError::Unavailable)
    );
}

#[sqlx::test]
async fn a_field_error_is_recorded_against_the_addressed_source_and_the_other_field_is_written(
    pool: PgPool,
) {
    awa::migrations::run(&pool).await.expect("the awa schema");

    let repository = Arc::new(PostgresTrackRepository::new(pool.clone()));
    let store = store(&pool, &repository);
    let subject = artist_subject(&pool, &repository, "mbid-field-error").await;
    let external_id = subject
        .musicbrainz_artist_id()
        .unwrap_or_default()
        .to_owned();
    let address = source(WIKIPEDIA, &external_id);
    let mut document = DocumentOutcome {
        address: address.clone(),
        source_revision: None,
        retrieved_at: at(0),
        candidates: vec![candidate(
            FieldId::BiographySummary,
            WIKIPEDIA,
            &external_id,
            FieldValue::Text("a summary".to_owned()),
        )],
        errors: vec![DocumentFieldError {
            field: FieldId::BiographyContent,
            code: "upstream_error".to_owned(),
            retryable: true,
        }],
    };
    document.errors.shrink_to_fit();
    let service = service_with(
        &pool,
        Arc::clone(&store) as Arc<dyn MetadataStore>,
        Arc::new(AnsweringDocuments(document)),
        FieldOrder::defaults(),
        &[WIKIPEDIA],
    );

    let report = service
        .refresh(&request(subject.clone()))
        .await
        .expect("a per-field error never fails the whole refresh");

    assert_eq!(
        outcome(&report, FieldId::BiographySummary),
        FieldRefreshOutcome::Written
    );
    assert_eq!(
        outcome(&report, FieldId::BiographyContent),
        FieldRefreshOutcome::Failed
    );

    let stored = store
        .read_subject(&SubjectKey::new(
            MetadataEntityKind::Artist,
            *subject.artist_id().as_uuid(),
            subject.identity_revision().to_owned(),
            "en".to_owned(),
            "v1".to_owned(),
        ))
        .await
        .expect("the stored facts");

    assert_eq!(stored.facts.len(), 1);
    assert_eq!(stored.facts[0].field, FieldId::BiographySummary);
    assert_eq!(stored.facts[0].source.provider_id, WIKIPEDIA);
    assert_eq!(
        stored.facts[0].value,
        FieldValue::Text("a summary".to_owned())
    );

    let attempt = stored
        .attempts
        .iter()
        .find(|attempt| attempt.field == FieldId::BiographyContent)
        .expect("the errored field has an attempt");

    assert_eq!(attempt.source_provider_id, WIKIPEDIA);
    assert_eq!(
        attempt.error.as_ref().map(|error| error.class.as_str()),
        Some("upstream_error")
    );
}

#[sqlx::test]
async fn an_over_cap_value_records_field_too_large_and_writes_no_fact(pool: PgPool) {
    awa::migrations::run(&pool).await.expect("the awa schema");

    let repository = Arc::new(PostgresTrackRepository::new(pool.clone()));
    let store = store(&pool, &repository);
    let subject = artist_subject(&pool, &repository, "mbid-too-large").await;
    let external_id = subject
        .musicbrainz_artist_id()
        .unwrap_or_default()
        .to_owned();
    let document = DocumentOutcome {
        address: source(WIKIPEDIA, &external_id),
        source_revision: None,
        retrieved_at: at(0),
        candidates: vec![candidate(
            FieldId::BiographyContent,
            WIKIPEDIA,
            &external_id,
            FieldValue::Text("x".repeat(crate::features::metadata::refresh::MAX_TEXT_BYTES + 1)),
        )],
        errors: Vec::new(),
    };
    let service = service_with(
        &pool,
        Arc::clone(&store) as Arc<dyn MetadataStore>,
        Arc::new(AnsweringDocuments(document)),
        order_for(FieldId::BiographyContent, &[WIKIPEDIA]),
        &[WIKIPEDIA],
    );

    let report = service
        .refresh(&request(subject.clone()))
        .await
        .expect("an over-cap value is a recorded outcome, not an error");

    assert_eq!(
        outcome(&report, FieldId::BiographyContent),
        FieldRefreshOutcome::Failed
    );

    let stored = store
        .read_subject(&SubjectKey::new(
            MetadataEntityKind::Artist,
            *subject.artist_id().as_uuid(),
            subject.identity_revision().to_owned(),
            "en".to_owned(),
            "v1".to_owned(),
        ))
        .await
        .expect("the stored facts");

    assert!(stored.facts.is_empty());
    assert_eq!(stored.attempts.len(), 1);
    assert_eq!(stored.attempts[0].source_provider_id, WIKIPEDIA);
    assert_eq!(
        stored.attempts[0]
            .error
            .as_ref()
            .map(|error| error.class.as_str()),
        Some("field_too_large")
    );
}

#[sqlx::test]
async fn a_candidate_list_over_fifty_is_truncated_locally_not_discarded(pool: PgPool) {
    awa::migrations::run(&pool).await.expect("the awa schema");

    let repository = Arc::new(PostgresTrackRepository::new(pool.clone()));
    let store = store(&pool, &repository);
    let subject = artist_subject(&pool, &repository, "mbid-truncated").await;
    let external_id = subject
        .musicbrainz_artist_id()
        .unwrap_or_default()
        .to_owned();
    let items = (0..60)
        .map(|index| CandidateItem {
            name: format!("Artist {index}"),
            external_id: Some(format!("mbid-{index}")),
            url: None,
            rank: Some(index),
            score: None,
            artists: Vec::new(),
            release_name: None,
            duration_ms: None,
            isrc: None,
            recording_id: None,
            ranking_scope: None,
        })
        .collect::<Vec<_>>();
    let document = DocumentOutcome {
        address: source(WIKIPEDIA, &external_id),
        source_revision: None,
        retrieved_at: at(0),
        candidates: vec![candidate(
            FieldId::Similar,
            WIKIPEDIA,
            &external_id,
            FieldValue::Candidates(items.clone()),
        )],
        errors: Vec::new(),
    };
    let service = service_with(
        &pool,
        Arc::clone(&store) as Arc<dyn MetadataStore>,
        Arc::new(AnsweringDocuments(document)),
        order_for(FieldId::Similar, &[WIKIPEDIA]),
        &[WIKIPEDIA],
    );

    let report = service
        .refresh(&request(subject.clone()))
        .await
        .expect("a long candidate list is truncated, not rejected");

    assert_eq!(
        outcome(&report, FieldId::Similar),
        FieldRefreshOutcome::Written
    );

    let stored = store
        .read_subject(&SubjectKey::new(
            MetadataEntityKind::Artist,
            *subject.artist_id().as_uuid(),
            subject.identity_revision().to_owned(),
            "en".to_owned(),
            "v1".to_owned(),
        ))
        .await
        .expect("the stored facts");

    assert_eq!(stored.facts.len(), 1);
    assert_eq!(
        stored.facts[0].value,
        FieldValue::Candidates(
            items[..crate::features::metadata::refresh::MAX_CANDIDATES].to_vec()
        )
    );
}

#[sqlx::test]
async fn an_uncertain_source_stores_uncertainty_and_its_basis_and_a_settled_one_does_not(
    pool: PgPool,
) {
    awa::migrations::run(&pool).await.expect("the awa schema");

    let repository = Arc::new(PostgresTrackRepository::new(pool.clone()));
    let store = store(&pool, &repository);
    let uncertain_subject = artist_subject(&pool, &repository, "mbid-uncertain").await;
    let settled_subject = artist_subject(&pool, &repository, "mbid-settled").await;

    let mut uncertain_subject = with_source(
        &pool,
        &repository,
        uncertain_subject,
        YOUTUBE_MUSIC,
        "ytm-channel-1",
    )
    .await;
    let mut uncertain_write = write(
        FieldId::ImagePortrait,
        YOUTUBE_MUSIC,
        "ytm-channel-1",
        FieldValue::Reference("https://image.example/portrait.jpg".to_owned()),
    );
    uncertain_write.image = image_member(FieldId::ImagePortrait);
    uncertain_write.terms_basis =
        Some("YouTube Terms of Service: no redistribution granted".to_owned());
    uncertain_write.uncertainty = true;
    store
        .commit_field(&uncertain_subject, &uncertain_write)
        .await
        .expect("the ytm portrait stores its terms and uncertainty");
    uncertain_subject = current_subject(&pool, &repository, &uncertain_subject).await;
    let settled_subject = current_subject(&pool, &repository, &settled_subject).await;
    let settled = an_image_document(&settled_subject, WIKIPEDIA);
    let settled_service = service_with(
        &pool,
        Arc::clone(&store) as Arc<dyn MetadataStore>,
        Arc::new(AnsweringDocuments(settled)),
        order_for(FieldId::ImagePortrait, &[WIKIPEDIA]),
        &[WIKIPEDIA],
    );
    settled_service
        .refresh(&request(settled_subject.clone()))
        .await
        .expect("the settled source refreshes");

    let uncertain_fact = stored_image(&store, &uncertain_subject).await;
    assert_eq!(
        uncertain_fact.source.provider_id, YOUTUBE_MUSIC,
        "the direct write is the ytm fact the refresh then replaces"
    );
    let settled_fact = stored_image(&store, &settled_subject).await;

    assert!(
        uncertain_fact.uncertainty,
        "youtube_music records its uncertainty on the fact"
    );
    assert_eq!(
        uncertain_fact.terms_basis.as_deref(),
        Some("YouTube Terms of Service: no redistribution granted")
    );
    assert!(
        !settled_fact.uncertainty,
        "a settled source does not claim uncertainty"
    );
    assert_eq!(
        settled_fact.terms_basis.as_deref(),
        Some("CC BY-SA 4.0 and GFDL: attribution and a link back required")
    );
}

fn an_image_document(subject: &EnrichmentSubject, provider_id: &str) -> DocumentOutcome {
    let external_id = addressed_external_id(subject, provider_id);

    DocumentOutcome {
        address: source(provider_id, &external_id),
        source_revision: None,
        retrieved_at: at(0),
        candidates: vec![candidate(
            FieldId::ImagePortrait,
            provider_id,
            &external_id,
            FieldValue::Reference("https://image.example/portrait.jpg".to_owned()),
        )],
        errors: Vec::new(),
    }
}

fn addressed_external_id(subject: &EnrichmentSubject, provider_id: &str) -> String {
    subject
        .identities()
        .iter()
        .find(|identity| identity.provider_id().as_str() == provider_id)
        .map(|identity| identity.external_id().to_owned())
        .or_else(|| subject.musicbrainz_artist_id().map(str::to_owned))
        .unwrap_or_default()
}

async fn current_subject(
    pool: &PgPool,
    repository: &PostgresTrackRepository,
    subject: &EnrichmentSubject,
) -> EnrichmentSubject {
    let mut transaction = pool.begin().await.expect("a transaction");
    let current = repository
        .enrichment_subject(&mut transaction, subject.artist_id())
        .await
        .expect("a subject")
        .expect("the artist exists");

    transaction.commit().await.expect("a commit");

    current
}

async fn with_source(
    pool: &PgPool,
    repository: &PostgresTrackRepository,
    subject: EnrichmentSubject,
    provider_id: &str,
    external_id: &str,
) -> EnrichmentSubject {
    repository
        .set_artist_source(subject.artist_id(), &source(provider_id, external_id))
        .await
        .expect("the artist source");

    current_subject(pool, repository, &subject).await
}

async fn stored_image(store: &PostgresMetadataStore, subject: &EnrichmentSubject) -> StoredFact {
    store
        .read_subject(&SubjectKey::new(
            MetadataEntityKind::Artist,
            *subject.artist_id().as_uuid(),
            subject.identity_revision().to_owned(),
            "en".to_owned(),
            "v1".to_owned(),
        ))
        .await
        .expect("the stored facts")
        .facts
        .into_iter()
        .find(|fact| fact.field == FieldId::ImagePortrait)
        .expect("the image fact")
}
