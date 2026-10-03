use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sqlx::{PgPool, postgres::PgPoolOptions};

use crate::features::metadata::{
    capabilities::{
        CAPABILITY_TTL, CapabilityError, CapabilitySnapshot, CapabilitySource, Snapshot,
    },
    domain::{
        AbsenceReason, AttemptError, DESCRIPTIVE_TTL, FactSource, FieldEntry, FieldId, FieldValue,
        FieldWrite, Freshness, MatchBasis, MetadataEntityKind, MetadataRecord, Presence,
        StoredAttempt, StoredFact, SubjectFacts, SubjectKey, bytes_state,
    },
    freshness::NoJitter,
    licences::{LicenceAssertion, licence_terms},
    refresh::{
        CandidateSource, DocumentCandidate, DocumentError, DocumentFieldError, DocumentOutcome,
        FieldRefreshOutcome, MetadataDocuments, RefreshReport, RefreshRequest,
    },
    selection::{FieldOrder, THEAUDIODB, WIKIPEDIA, YOUTUBE_MUSIC},
    service::{
        MetadataError, MetadataProfile, MetadataService, RefreshOutcome, RefreshPhase,
        RefreshReason,
    },
    storage::{CommitOutcome, MetadataRepositoryError, MetadataStore},
};
use crate::features::music_library::domain::{
    ArtistId, EnrichmentSubject, ProviderId, SourceIdentity, SourceScope,
};

fn at(seconds: i64) -> DateTime<Utc> {
    DateTime::from_timestamp(1_800_000_000 + seconds, 0).expect("a valid instant")
}

fn lazy_pool() -> PgPool {
    PgPoolOptions::new()
        .connect_lazy("postgres://whio:whio@127.0.0.1:1/whio")
        .expect("a lazy pool")
}

fn source(provider_id: &str, external_id: &str) -> SourceIdentity {
    SourceIdentity::new(
        ProviderId::new(provider_id.to_owned()).expect("a provider id"),
        SourceScope::Global,
        external_id.to_owned(),
    )
    .expect("a source identity")
}

fn subject(mbid: Option<&str>, identities: &[(&str, &str)]) -> EnrichmentSubject {
    EnrichmentSubject::new(
        ArtistId::new(),
        "Daft Punk".to_owned(),
        mbid.map(str::to_owned),
        identities
            .iter()
            .map(|(provider_id, external_id)| source(provider_id, external_id))
            .collect(),
    )
}

fn assertion(licence_id: &str) -> LicenceAssertion {
    let terms = licence_terms(licence_id, 1).expect("the registry terms");

    LicenceAssertion {
        id: terms.id,
        url: terms.url,
        attribution_required: terms.attribution_required,
        redistribution_allowed: terms.redistribution_allowed,
        cache_allowed: terms.cache_allowed,
        share_alike: terms.share_alike,
        non_commercial: terms.non_commercial,
        storage_cap_bytes: terms.storage_cap_bytes,
    }
}

fn fact(
    field: FieldId,
    provider_id: &str,
    external_id: &str,
    value: FieldValue,
    fetched_at: DateTime<Utc>,
) -> StoredFact {
    let licence = licence_terms(provider_id, 1).expect("the registry terms");

    StoredFact {
        field,
        source: FactSource {
            provider_id: provider_id.to_owned(),
            external_id: external_id.to_owned(),
            url: None,
            match_basis: MatchBasis::Identifier,
        },
        value,
        bytes: bytes_state(field, &licence),
        licence,
        attribution: None,
        source_url: None,
        operation: "artist-display.v1".to_owned(),
        terms_basis: None,
        uncertainty: false,
        image: None,
        metric: None,
        rank: None,
        score: None,
        stored_bytes: 0,
        fetched_at,
    }
}

struct FakeStore {
    facts: Mutex<Vec<StoredFact>>,
    attempts: Mutex<Vec<StoredAttempt>>,
    commits: Mutex<Vec<FieldWrite>>,
    failures: Mutex<Vec<(FieldId, String)>>,
    read_unavailable: bool,
}

impl FakeStore {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            facts: Mutex::new(Vec::new()),
            attempts: Mutex::new(Vec::new()),
            commits: Mutex::new(Vec::new()),
            failures: Mutex::new(Vec::new()),
            read_unavailable: false,
        })
    }

    fn holding(facts: Vec<StoredFact>) -> Arc<Self> {
        let store = Self::new();
        *store.facts.lock().expect("the fake facts") = facts;

        store
    }

    fn unavailable() -> Arc<Self> {
        Arc::new(Self {
            facts: Mutex::new(Vec::new()),
            attempts: Mutex::new(Vec::new()),
            commits: Mutex::new(Vec::new()),
            failures: Mutex::new(Vec::new()),
            read_unavailable: true,
        })
    }

    fn commits(&self) -> Vec<FieldWrite> {
        self.commits.lock().expect("the fake commits").clone()
    }

    fn wrote_value(&self, field: FieldId) -> bool {
        self.commits()
            .iter()
            .any(|write| write.field == field && write.value.is_some())
    }

    fn fact(&self, field: FieldId) -> Option<StoredFact> {
        self.facts
            .lock()
            .expect("the fake facts")
            .iter()
            .find(|fact| fact.field == field)
            .cloned()
    }

    fn attempt(&self, field: FieldId) -> Option<StoredAttempt> {
        self.attempts
            .lock()
            .expect("the fake attempts")
            .iter()
            .find(|attempt| attempt.field == field)
            .cloned()
    }

    fn failure_classes(&self, field: FieldId) -> Vec<String> {
        self.failures
            .lock()
            .expect("the fake failures")
            .iter()
            .filter(|(failed, _)| *failed == field)
            .map(|(_, class)| class.clone())
            .collect()
    }

    fn failure_providers(&self, field: FieldId) -> Vec<String> {
        self.attempts
            .lock()
            .expect("the fake attempts")
            .iter()
            .filter(|attempt| attempt.field == field && attempt.error.is_some())
            .map(|attempt| attempt.source_provider_id.clone())
            .collect()
    }
}

#[async_trait]
impl MetadataStore for FakeStore {
    async fn read_subject(
        &self,
        key: &SubjectKey,
    ) -> Result<SubjectFacts, MetadataRepositoryError> {
        if self.read_unavailable {
            return Err(MetadataRepositoryError::Unavailable);
        }

        Ok(SubjectFacts::new(
            key.clone(),
            self.facts.lock().expect("the fake facts").clone(),
            self.attempts.lock().expect("the fake attempts").clone(),
        ))
    }

    async fn commit_field(
        &self,
        _subject: &EnrichmentSubject,
        write: &FieldWrite,
    ) -> Result<CommitOutcome, MetadataRepositoryError> {
        self.commits
            .lock()
            .expect("the fake commits")
            .push(write.clone());
        upsert_success(&mut self.attempts.lock().expect("the fake attempts"), write);

        let Some(value) = write.value.clone() else {
            return Ok(CommitOutcome::Written);
        };
        let licence_id = write
            .licence_id
            .clone()
            .ok_or(MetadataRepositoryError::InvalidData)?;
        let licence_revision = write
            .licence_revision
            .ok_or(MetadataRepositoryError::InvalidData)?;
        let licence = licence_terms(&licence_id, licence_revision)
            .ok_or(MetadataRepositoryError::InvalidData)?;
        let mut facts = self.facts.lock().expect("the fake facts");

        facts.retain(|fact| {
            !(fact.field == write.field
                && fact.source.provider_id == write.source_provider_id
                && fact.source.external_id == write.source_external_id)
        });
        facts.push(StoredFact {
            field: write.field,
            source: FactSource {
                provider_id: write.source_provider_id.clone(),
                external_id: write.source_external_id.clone(),
                url: write.source_url.clone(),
                match_basis: write.match_basis,
            },
            value,
            bytes: bytes_state(write.field, &licence),
            licence,
            attribution: write.attribution.clone(),
            source_url: write.source_url.clone(),
            operation: write.operation.clone(),
            terms_basis: write.terms_basis.clone(),
            uncertainty: write.uncertainty,
            image: write.image.clone(),
            metric: write.metric.clone(),
            rank: write.rank,
            score: write.score,
            stored_bytes: 0,
            fetched_at: write.fetched_at,
        });

        Ok(CommitOutcome::Written)
    }

    async fn record_failure(
        &self,
        _subject: &EnrichmentSubject,
        field: FieldId,
        _language: &str,
        source_provider_id: &str,
        error: &AttemptError,
    ) -> Result<(), MetadataRepositoryError> {
        if source_provider_id.chars().count() > 64 || source_provider_id.is_empty() {
            return Err(MetadataRepositoryError::InvalidData);
        }

        self.failures
            .lock()
            .expect("the fake failures")
            .push((field, error.class.clone()));
        upsert_failure(
            &mut self.attempts.lock().expect("the fake attempts"),
            field,
            source_provider_id,
            Utc::now(),
            error,
        );

        Ok(())
    }

    async fn delete_subject_facts(
        &self,
        _kind: MetadataEntityKind,
        _ids: &[uuid::Uuid],
    ) -> Result<u64, MetadataRepositoryError> {
        Ok(0)
    }
}

fn upsert_success(attempts: &mut Vec<StoredAttempt>, write: &FieldWrite) {
    let existing = attempts.iter_mut().find(|attempt| {
        attempt.field == write.field && attempt.source_provider_id == write.source_provider_id
    });

    match existing {
        Some(attempt) => {
            attempt.fetched_at = Some(write.fetched_at);
            attempt.attempted_at = write.fetched_at;
            attempt.error = None;
        }
        None => attempts.push(StoredAttempt {
            field: write.field,
            source_provider_id: write.source_provider_id.clone(),
            fetched_at: Some(write.fetched_at),
            attempted_at: write.fetched_at,
            error: None,
        }),
    }
}

fn upsert_failure(
    attempts: &mut Vec<StoredAttempt>,
    field: FieldId,
    source_provider_id: &str,
    attempted_at: DateTime<Utc>,
    error: &AttemptError,
) {
    let existing = attempts
        .iter_mut()
        .find(|attempt| attempt.field == field && attempt.source_provider_id == source_provider_id);

    match existing {
        Some(attempt) => {
            attempt.attempted_at = attempted_at;
            attempt.error = Some(error.clone());
        }
        None => attempts.push(StoredAttempt {
            field,
            source_provider_id: source_provider_id.to_owned(),
            fetched_at: None,
            attempted_at,
            error: Some(error.clone()),
        }),
    }
}

struct FakeCapabilities {
    providers: Vec<String>,
    error: Option<CapabilityError>,
    calls: AtomicUsize,
}

impl FakeCapabilities {
    fn advertising(providers: &[&str]) -> Arc<Self> {
        Arc::new(Self {
            providers: providers
                .iter()
                .map(|provider| (*provider).to_owned())
                .collect(),
            error: None,
            calls: AtomicUsize::new(0),
        })
    }

    fn failing(error: CapabilityError) -> Arc<Self> {
        Arc::new(Self {
            providers: Vec::new(),
            error: Some(error),
            calls: AtomicUsize::new(0),
        })
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl CapabilitySource for FakeCapabilities {
    async fn fetch(&self) -> Result<Snapshot, CapabilityError> {
        self.calls.fetch_add(1, Ordering::SeqCst);

        match &self.error {
            Some(error) => Err(error.clone()),
            None => Ok(Snapshot::new(self.providers.clone())),
        }
    }
}

struct FakeDocuments {
    responses: Mutex<HashMap<String, Result<DocumentOutcome, DocumentError>>>,
    calls: Mutex<Vec<(String, Vec<FieldId>)>>,
}

impl FakeDocuments {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            responses: Mutex::new(HashMap::new()),
            calls: Mutex::new(Vec::new()),
        })
    }

    fn answering(self: &Arc<Self>, provider_id: &str, outcome: DocumentOutcome) -> Arc<Self> {
        self.responses
            .lock()
            .expect("the fake responses")
            .insert(provider_id.to_owned(), Ok(outcome));

        Arc::clone(self)
    }

    fn failing(self: &Arc<Self>, provider_id: &str, error: DocumentError) -> Arc<Self> {
        self.responses
            .lock()
            .expect("the fake responses")
            .insert(provider_id.to_owned(), Err(error));

        Arc::clone(self)
    }

    fn calls(&self) -> Vec<(String, Vec<FieldId>)> {
        self.calls.lock().expect("the fake calls").clone()
    }

    fn call_count(&self) -> usize {
        self.calls.lock().expect("the fake calls").len()
    }
}

#[async_trait]
impl MetadataDocuments for FakeDocuments {
    async fn fetch(
        &self,
        source: &SourceIdentity,
        _profile: &MetadataProfile,
        _language: &str,
        fields: &[FieldId],
    ) -> Result<DocumentOutcome, DocumentError> {
        let provider_id = source.provider_id().as_str().to_owned();

        self.calls
            .lock()
            .expect("the fake calls")
            .push((provider_id.clone(), fields.to_vec()));

        self.responses
            .lock()
            .expect("the fake responses")
            .get(&provider_id)
            .cloned()
            .unwrap_or(Err(DocumentError::Unsupported))
    }
}

fn candidate(
    field: FieldId,
    provider_id: &str,
    external_id: &str,
    value: FieldValue,
) -> DocumentCandidate {
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
        licence: assertion(provider_id),
        attribution: None,
        source_url: None,
        rank: None,
        score: None,
        image: None,
        metric: None,
    }
}

fn document(address: &SourceIdentity, candidates: Vec<DocumentCandidate>) -> DocumentOutcome {
    DocumentOutcome {
        address: address.clone(),
        source_revision: None,
        retrieved_at: at(0),
        candidates,
        errors: Vec::new(),
    }
}

fn service(
    store: Arc<FakeStore>,
    documents: Arc<FakeDocuments>,
    capabilities: Arc<FakeCapabilities>,
    order: FieldOrder,
) -> MetadataService {
    MetadataService::new(
        lazy_pool(),
        store,
        documents,
        Arc::new(CapabilitySnapshot::new(capabilities, CAPABILITY_TTL)),
        order,
        "v1".to_owned(),
        Arc::new(NoJitter),
    )
}

fn service_with_snapshot(
    store: Arc<FakeStore>,
    documents: Arc<FakeDocuments>,
    capabilities: Arc<FakeCapabilities>,
    order: FieldOrder,
) -> (MetadataService, Arc<CapabilitySnapshot>) {
    let snapshot = Arc::new(CapabilitySnapshot::new(capabilities, CAPABILITY_TTL));

    (
        MetadataService::new(
            lazy_pool(),
            store,
            documents,
            Arc::clone(&snapshot),
            order,
            "v1".to_owned(),
            Arc::new(NoJitter),
        ),
        snapshot,
    )
}

fn narrow_order(entries: &[(FieldId, &[&str])]) -> FieldOrder {
    FieldOrder::from_entries(entries.iter().map(|(field, sources)| {
        (
            *field,
            sources.iter().map(|source| (*source).to_owned()).collect(),
        )
    }))
}

fn all_providers() -> FieldOrder {
    FieldOrder::defaults()
}

fn request(
    subject: EnrichmentSubject,
    requested_at: DateTime<Utc>,
    now: DateTime<Utc>,
) -> RefreshRequest {
    RefreshRequest {
        subject,
        profile: MetadataProfile::artist_display(),
        language: "en".to_owned(),
        requested_at,
        now,
    }
}

fn entry(record: &MetadataRecord, field: FieldId) -> &FieldEntry {
    record
        .fields
        .iter()
        .find(|entry| entry.field == field)
        .expect("the record names every requested field")
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

fn text(value: &str) -> FieldValue {
    FieldValue::Text(value.to_owned())
}

#[tokio::test]
async fn a_cold_read_returns_every_requested_field_as_unrequested() {
    let store = FakeStore::new();
    let documents = FakeDocuments::new();
    let capabilities = FakeCapabilities::advertising(&[
        "youtube_music",
        "theaudiodb",
        "wikipedia",
        "musicbrainz",
        "listenbrainz",
        "lastfm",
        "local",
    ]);
    let (service, snapshot) = service_with_snapshot(
        Arc::clone(&store),
        Arc::clone(&documents),
        Arc::clone(&capabilities),
        all_providers(),
    );
    snapshot
        .current(at(0))
        .await
        .expect("the startup warmer holds an advertisement");
    let fetches = capabilities.calls();
    let subject = subject(Some("mbid-daft-punk"), &[]);

    let record = service
        .read_artist(&subject, &MetadataProfile::artist_display(), "en", at(0))
        .await
        .expect("a cold read");
    assert_eq!(record.fields.len(), 16);
    assert_eq!(record.display_name, "Daft Punk");
    assert_eq!(record.generated_at, at(0));
    assert!(!record.contested);
    assert!(record.disagreements.is_empty());

    for entry in &record.fields {
        assert_eq!(entry.presence, Presence::Absent, "{:?}", entry.field);
        assert_eq!(entry.freshness, Freshness::Unrequested, "{:?}", entry.field);
        assert_eq!(
            entry.absence,
            Some(AbsenceReason::Unrequested),
            "{:?}",
            entry.field
        );
        assert!(entry.value.is_none());
        assert!(entry.retrieved_at.is_none());
    }

    assert_eq!(
        capabilities.calls(),
        fetches,
        "a read never refreshes the advertisement"
    );
    assert_eq!(documents.call_count(), 0, "a read never calls a provider");
}

#[tokio::test]
async fn a_read_with_an_expired_advertisement_is_capability_absent_and_does_not_refetch() {
    let store = FakeStore::new();
    let documents = FakeDocuments::new();
    let capabilities = FakeCapabilities::advertising(&[WIKIPEDIA]);
    let subject = subject(Some("mbid-daft-punk"), &[(WIKIPEDIA, "mbid-daft-punk")]);
    let (service, snapshot) = service_with_snapshot(
        Arc::clone(&store),
        Arc::clone(&documents),
        Arc::clone(&capabilities),
        narrow_order(&[(FieldId::BiographySummary, &[WIKIPEDIA])]),
    );

    snapshot
        .current(at(0))
        .await
        .expect("the startup warmer holds an advertisement");
    let fetches = capabilities.calls();

    let record = service
        .read_artist(&subject, &MetadataProfile::artist_display(), "en", at(600))
        .await
        .expect("an expired advertisement is not a read error");

    assert_eq!(
        entry(&record, FieldId::BiographySummary).absence,
        Some(AbsenceReason::CapabilityAbsent)
    );
    assert_eq!(
        capabilities.calls(),
        fetches,
        "a read with an expired advertisement must not refresh it"
    );
}

#[tokio::test]
async fn refresh_state_consults_only_the_held_advertisement() {
    let store = FakeStore::new();
    let documents = FakeDocuments::new();
    let capabilities = FakeCapabilities::advertising(&[WIKIPEDIA]);
    let subject = subject(Some("mbid-daft-punk"), &[(WIKIPEDIA, "mbid-daft-punk")]);
    let (service, snapshot) = service_with_snapshot(
        Arc::clone(&store),
        Arc::clone(&documents),
        Arc::clone(&capabilities),
        narrow_order(&[(FieldId::BiographySummary, &[WIKIPEDIA])]),
    );

    snapshot
        .current(at(0))
        .await
        .expect("the startup warmer holds an advertisement");
    let fetches = capabilities.calls();

    let state = service
        .refresh_state(&subject, &MetadataProfile::artist_display(), "en")
        .await;

    assert_eq!(
        capabilities.calls(),
        fetches,
        "a refresh state read must not refresh the advertisement"
    );
    assert_eq!(
        state.expect("a refresh state").phase,
        RefreshPhase::Unavailable
    );
}

#[tokio::test]
async fn a_stale_fact_is_served_as_present_and_stale() {
    let store = FakeStore::holding(vec![fact(
        FieldId::BiographySummary,
        WIKIPEDIA,
        "mbid-daft-punk",
        text("a summary"),
        at(0),
    )]);
    let documents = FakeDocuments::new();
    let capabilities = FakeCapabilities::advertising(&[WIKIPEDIA]);
    let subject = subject(Some("mbid-daft-punk"), &[(WIKIPEDIA, "mbid-daft-punk")]);
    let service = service(
        Arc::clone(&store),
        Arc::clone(&documents),
        Arc::clone(&capabilities),
        narrow_order(&[(FieldId::BiographySummary, &[WIKIPEDIA])]),
    );
    let now = at(DESCRIPTIVE_TTL.as_secs() as i64 + 1);

    let record = service
        .read_artist(&subject, &MetadataProfile::artist_display(), "en", now)
        .await
        .expect("a warm read");
    let summary = entry(&record, FieldId::BiographySummary);

    assert_eq!(summary.presence, Presence::Present);
    assert_eq!(summary.freshness, Freshness::Stale);
    assert_eq!(summary.absence, None);
    assert_eq!(summary.value, Some(text("a summary")));
    assert_eq!(summary.retrieved_at, Some(at(0)));
    assert_eq!(
        summary
            .source
            .as_ref()
            .map(|source| source.provider_id.as_str()),
        Some(WIKIPEDIA)
    );
}

#[tokio::test]
async fn the_configured_order_picks_the_winner_and_records_the_disagreement() {
    let store = FakeStore::holding(vec![
        fact(
            FieldId::BiographySummary,
            THEAUDIODB,
            "mbid-daft-punk",
            text("audio db summary"),
            at(10),
        ),
        fact(
            FieldId::BiographySummary,
            WIKIPEDIA,
            "mbid-daft-punk",
            text("wikipedia summary"),
            at(0),
        ),
    ]);
    let documents = FakeDocuments::new();
    let capabilities = FakeCapabilities::advertising(&[WIKIPEDIA, THEAUDIODB]);
    let subject = subject(Some("mbid-daft-punk"), &[]);
    let service = service(
        Arc::clone(&store),
        documents,
        capabilities,
        narrow_order(&[(FieldId::BiographySummary, &[WIKIPEDIA, THEAUDIODB])]),
    );

    let record = service
        .read_artist(&subject, &MetadataProfile::artist_display(), "en", at(20))
        .await
        .expect("a contested read");
    let summary = entry(&record, FieldId::BiographySummary);

    assert_eq!(summary.value, Some(text("wikipedia summary")));
    assert_eq!(summary.facts.len(), 2);
    assert!(record.contested);
    assert_eq!(record.disagreements.len(), 1);
    assert_eq!(record.disagreements[0].field, FieldId::BiographySummary);
    assert_eq!(record.disagreements[0].sources.len(), 1);
    assert_eq!(record.disagreements[0].sources[0].provider_id, THEAUDIODB);
}

#[tokio::test]
async fn a_refresh_skips_fresh_fields_and_calls_no_provider() {
    let store = FakeStore::holding(vec![
        fact(
            FieldId::BiographySummary,
            WIKIPEDIA,
            "mbid-daft-punk",
            text("a summary"),
            at(0),
        ),
        fact(
            FieldId::BiographyContent,
            WIKIPEDIA,
            "mbid-daft-punk",
            text("a biography"),
            at(0),
        ),
    ]);
    let documents = FakeDocuments::new();
    let capabilities = FakeCapabilities::advertising(&[WIKIPEDIA]);
    let subject = subject(Some("mbid-daft-punk"), &[(WIKIPEDIA, "mbid-daft-punk")]);
    let service = service(
        Arc::clone(&store),
        Arc::clone(&documents),
        capabilities,
        narrow_order(&[
            (FieldId::BiographySummary, &[WIKIPEDIA]),
            (FieldId::BiographyContent, &[WIKIPEDIA]),
        ]),
    );

    let report = service
        .refresh(&request(subject, at(0), at(0)))
        .await
        .expect("a duplicate execution");

    assert_eq!(
        outcome(&report, FieldId::BiographySummary),
        FieldRefreshOutcome::Skipped
    );
    assert_eq!(
        outcome(&report, FieldId::BiographyContent),
        FieldRefreshOutcome::Skipped
    );
    assert_eq!(
        documents.call_count(),
        0,
        "a duplicate execution calls no provider"
    );
    assert!(store.commits().is_empty());
}

#[tokio::test]
async fn a_refresh_walks_the_order_and_stops_once_every_field_is_present() {
    let store = FakeStore::new();
    let subject = subject(
        Some("mbid-daft-punk"),
        &[(YOUTUBE_MUSIC, "channel-1"), (THEAUDIODB, "artist-1")],
    );
    let youtube = source(YOUTUBE_MUSIC, "channel-1");
    let theaudiodb = source(THEAUDIODB, "artist-1");
    let documents = FakeDocuments::new()
        .answering(
            YOUTUBE_MUSIC,
            document(
                &youtube,
                vec![candidate(
                    FieldId::BiographyContent,
                    YOUTUBE_MUSIC,
                    "channel-1",
                    text("youtube biography"),
                )],
            ),
        )
        .answering(
            THEAUDIODB,
            document(
                &theaudiodb,
                vec![candidate(
                    FieldId::BiographyContent,
                    THEAUDIODB,
                    "artist-1",
                    text("audio db biography"),
                )],
            ),
        );
    let capabilities = FakeCapabilities::advertising(&[YOUTUBE_MUSIC, THEAUDIODB]);
    let service = service(
        Arc::clone(&store),
        Arc::clone(&documents),
        capabilities,
        narrow_order(&[(FieldId::BiographyContent, &[YOUTUBE_MUSIC, THEAUDIODB])]),
    );

    let report = service
        .refresh(&request(subject, at(0), at(0)))
        .await
        .expect("a refresh");

    assert_eq!(
        outcome(&report, FieldId::BiographyContent),
        FieldRefreshOutcome::Written
    );
    assert_eq!(
        store
            .fact(FieldId::BiographyContent)
            .map(|fact| fact.source.provider_id),
        Some(YOUTUBE_MUSIC.to_owned())
    );
    assert_eq!(
        documents.call_count(),
        1,
        "the first present candidate stops the walk"
    );
    assert_eq!(documents.calls()[0].0, YOUTUBE_MUSIC);
}

#[tokio::test]
async fn a_failed_field_does_not_fail_another() {
    let store = FakeStore::new();
    let subject = subject(Some("mbid-daft-punk"), &[(WIKIPEDIA, "mbid-daft-punk")]);
    let mut outcome_for_wikipedia = document(
        &source(WIKIPEDIA, "mbid-daft-punk"),
        vec![candidate(
            FieldId::BiographySummary,
            WIKIPEDIA,
            "mbid-daft-punk",
            text("a summary"),
        )],
    );
    outcome_for_wikipedia.errors = vec![DocumentFieldError {
        field: FieldId::BiographyContent,
        code: "upstream_error".to_owned(),
        retryable: true,
    }];
    let documents = FakeDocuments::new().answering(WIKIPEDIA, outcome_for_wikipedia);
    let capabilities = FakeCapabilities::advertising(&[WIKIPEDIA]);
    let service = service(
        Arc::clone(&store),
        Arc::clone(&documents),
        capabilities,
        narrow_order(&[
            (FieldId::BiographySummary, &[WIKIPEDIA]),
            (FieldId::BiographyContent, &[WIKIPEDIA]),
        ]),
    );

    let report = service
        .refresh(&request(subject, at(0), at(0)))
        .await
        .expect("a partial refresh");

    assert_eq!(
        outcome(&report, FieldId::BiographySummary),
        FieldRefreshOutcome::Written
    );
    assert_eq!(
        outcome(&report, FieldId::BiographyContent),
        FieldRefreshOutcome::Failed
    );
    assert!(store.wrote_value(FieldId::BiographySummary));
    assert!(!store.wrote_value(FieldId::BiographyContent));
    assert_eq!(
        store.failure_classes(FieldId::BiographyContent),
        vec!["upstream_error".to_owned()]
    );
    assert_eq!(
        store.failure_providers(FieldId::BiographyContent),
        vec![WIKIPEDIA.to_owned()],
        "a field error is recorded against the source that was addressed"
    );
    assert_eq!(documents.call_count(), 1);
}

#[tokio::test]
async fn an_empty_document_records_a_successful_absence() {
    let store = FakeStore::new();
    let subject = subject(Some("mbid-daft-punk"), &[(WIKIPEDIA, "mbid-daft-punk")]);
    let documents = FakeDocuments::new().answering(
        WIKIPEDIA,
        document(&source(WIKIPEDIA, "mbid-daft-punk"), Vec::new()),
    );
    let capabilities = FakeCapabilities::advertising(&[WIKIPEDIA]);
    let service = service(
        Arc::clone(&store),
        documents,
        capabilities,
        narrow_order(&[(FieldId::BiographySummary, &[WIKIPEDIA])]),
    );

    let report = service
        .refresh(&request(subject.clone(), at(0), at(0)))
        .await
        .expect("an empty document");

    assert_eq!(
        outcome(&report, FieldId::BiographySummary),
        FieldRefreshOutcome::Absent
    );
    assert!(!store.wrote_value(FieldId::BiographySummary));

    let attempt = store
        .attempt(FieldId::BiographySummary)
        .expect("the empty answer is recorded");
    assert_eq!(attempt.fetched_at, Some(at(0)));
    assert!(attempt.error.is_none());

    let record = service
        .read_artist(&subject, &MetadataProfile::artist_display(), "en", at(60))
        .await
        .expect("a read after an empty answer");
    let summary = entry(&record, FieldId::BiographySummary);

    assert_eq!(summary.presence, Presence::Absent);
    assert_eq!(summary.freshness, Freshness::Fresh);
    assert_eq!(summary.absence, Some(AbsenceReason::ProviderReportedNone));
    assert_eq!(summary.retrieved_at, Some(at(0)));
}

#[tokio::test]
async fn a_provider_not_found_is_a_failed_attempt_never_an_absence() {
    let store = FakeStore::new();
    let subject = subject(Some("mbid-daft-punk"), &[(WIKIPEDIA, "mbid-daft-punk")]);
    let documents = FakeDocuments::new().failing(WIKIPEDIA, DocumentError::NotFound);
    let capabilities = FakeCapabilities::advertising(&[WIKIPEDIA]);
    let service = service(
        Arc::clone(&store),
        documents,
        capabilities,
        narrow_order(&[(FieldId::BiographySummary, &[WIKIPEDIA])]),
    );

    let report = service
        .refresh(&request(subject.clone(), at(0), at(0)))
        .await
        .expect("a refresh against a missing identity");

    assert_eq!(
        outcome(&report, FieldId::BiographySummary),
        FieldRefreshOutcome::Failed
    );
    assert_eq!(
        store.failure_classes(FieldId::BiographySummary),
        vec!["source_not_found".to_owned()]
    );

    let attempt = store
        .attempt(FieldId::BiographySummary)
        .expect("the not-found is recorded");
    assert_eq!(attempt.fetched_at, None);
    assert!(attempt.error.is_some());

    let record = service
        .read_artist(&subject, &MetadataProfile::artist_display(), "en", at(60))
        .await
        .expect("a read after a not-found");
    let summary = entry(&record, FieldId::BiographySummary);

    assert!(matches!(summary.freshness, Freshness::Failed(_)));
    assert_ne!(summary.absence, Some(AbsenceReason::ProviderReportedNone));
    assert_eq!(summary.absence, None);
}

#[tokio::test]
async fn a_quota_wait_defers_with_an_explicit_deadline() {
    let store = FakeStore::new();
    let subject = subject(Some("mbid-daft-punk"), &[(WIKIPEDIA, "mbid-daft-punk")]);
    let documents = FakeDocuments::new().failing(
        WIKIPEDIA,
        DocumentError::Unavailable {
            retry_after: Some(Duration::from_secs(60)),
        },
    );
    let capabilities = FakeCapabilities::advertising(&[WIKIPEDIA]);
    let service = service(
        Arc::clone(&store),
        documents,
        capabilities,
        narrow_order(&[(FieldId::BiographySummary, &[WIKIPEDIA])]),
    );

    let report = service
        .refresh(&request(subject, at(0), at(60)))
        .await
        .expect("a rate-limited refresh");

    assert_eq!(
        report,
        RefreshReport::Deferred {
            retry_after: Duration::from_secs(60),
            deadline: at(0) + chrono::Duration::hours(24),
        }
    );
    assert!(
        !store.wrote_value(FieldId::BiographySummary),
        "a rate limit never writes a value"
    );
    assert_eq!(
        store.failure_classes(FieldId::BiographySummary),
        vec!["provider_unavailable".to_owned()]
    );
}

#[tokio::test]
async fn a_quota_wait_past_the_deadline_stops_deferring() {
    let store = FakeStore::new();
    let subject = subject(Some("mbid-daft-punk"), &[(WIKIPEDIA, "mbid-daft-punk")]);
    let documents = FakeDocuments::new().failing(
        WIKIPEDIA,
        DocumentError::Unavailable {
            retry_after: Some(Duration::from_secs(60)),
        },
    );
    let capabilities = FakeCapabilities::advertising(&[WIKIPEDIA]);
    let service = service(
        Arc::clone(&store),
        documents,
        capabilities,
        narrow_order(&[(FieldId::BiographySummary, &[WIKIPEDIA])]),
    );
    let deadline = at(24 * 60 * 60 + 1);

    let report = service
        .refresh(&request(subject, at(0), deadline))
        .await
        .expect("a refresh past its deferral deadline");

    assert_eq!(
        outcome(&report, FieldId::BiographySummary),
        FieldRefreshOutcome::Failed
    );
    assert!(!store.wrote_value(FieldId::BiographySummary));
}

#[tokio::test]
async fn an_unreachable_transport_is_retryable() {
    let store = FakeStore::new();
    let subject = subject(Some("mbid-daft-punk"), &[(WIKIPEDIA, "mbid-daft-punk")]);
    let documents =
        FakeDocuments::new().failing(WIKIPEDIA, DocumentError::Unavailable { retry_after: None });
    let capabilities = FakeCapabilities::advertising(&[WIKIPEDIA]);
    let service = service(
        Arc::clone(&store),
        documents,
        capabilities,
        narrow_order(&[(FieldId::BiographySummary, &[WIKIPEDIA])]),
    );

    assert_eq!(
        service.refresh(&request(subject, at(0), at(0))).await,
        Err(MetadataError::ProviderUnavailable)
    );
    assert!(store.commits().is_empty());
}

#[tokio::test]
async fn an_unadvertised_source_is_capability_absent_with_no_call() {
    let store = FakeStore::holding(vec![fact(
        FieldId::BiographySummary,
        WIKIPEDIA,
        "mbid-daft-punk",
        text("a summary"),
        at(0),
    )]);
    let documents = FakeDocuments::new();
    let capabilities = FakeCapabilities::advertising(&[YOUTUBE_MUSIC]);
    let subject = subject(Some("mbid-daft-punk"), &[(WIKIPEDIA, "mbid-daft-punk")]);
    let service = service(
        Arc::clone(&store),
        Arc::clone(&documents),
        Arc::clone(&capabilities),
        narrow_order(&[
            (FieldId::BiographySummary, &[WIKIPEDIA]),
            (FieldId::BiographyContent, &[WIKIPEDIA]),
        ]),
    );

    let record = service
        .read_artist(&subject, &MetadataProfile::artist_display(), "en", at(0))
        .await
        .expect("a read");
    let content = entry(&record, FieldId::BiographyContent);

    assert_eq!(content.absence, Some(AbsenceReason::CapabilityAbsent));
    assert_eq!(content.presence, Presence::Absent);
    assert_eq!(
        entry(&record, FieldId::BiographySummary).presence,
        Presence::Present
    );

    let report = service
        .refresh(&request(subject.clone(), at(0), at(0)))
        .await
        .expect("a refresh with no advertised source");
    let summary_report = match &report {
        RefreshReport::Applied { fields } => fields
            .iter()
            .find(|entry| entry.field == FieldId::BiographySummary)
            .map(|entry| entry.outcome),
        other => panic!("expected an applied report, found {other:?}"),
    };

    assert_eq!(summary_report, Some(FieldRefreshOutcome::Skipped));
    assert_eq!(
        outcome(&report, FieldId::BiographyContent),
        FieldRefreshOutcome::Unavailable
    );
    assert_eq!(
        documents.call_count(),
        0,
        "no advertised source means no call"
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
        Ok(RefreshOutcome::Unavailable)
    );
    assert_eq!(
        service
            .refresh_state(&subject, &MetadataProfile::artist_display(), "en")
            .await
            .expect("a refresh state")
            .phase,
        RefreshPhase::Unavailable
    );
}

#[tokio::test]
async fn an_unreachable_advertisement_is_capability_absent_not_an_error() {
    let store = FakeStore::new();
    let documents = FakeDocuments::new();
    let capabilities = FakeCapabilities::failing(CapabilityError::Unavailable);
    let subject = subject(Some("mbid-daft-punk"), &[(WIKIPEDIA, "mbid-daft-punk")]);
    let service = service(
        Arc::clone(&store),
        Arc::clone(&documents),
        Arc::clone(&capabilities),
        narrow_order(&[(FieldId::BiographySummary, &[WIKIPEDIA])]),
    );

    let record = service
        .read_artist(&subject, &MetadataProfile::artist_display(), "en", at(0))
        .await
        .expect("an unreachable advertisement is not a read error");
    let summary = entry(&record, FieldId::BiographySummary);

    assert_eq!(summary.absence, Some(AbsenceReason::CapabilityAbsent));
    assert_eq!(
        capabilities.calls(),
        0,
        "a read never refreshes the advertisement"
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
        Ok(RefreshOutcome::Unavailable)
    );
    assert_eq!(
        capabilities.calls(),
        1,
        "the refresh surface is the path that refreshes the advertisement"
    );
    assert_eq!(documents.call_count(), 0);
}

#[tokio::test]
async fn the_refresh_job_refreshes_the_advertisement_it_reads() {
    let store = FakeStore::new();
    let documents = FakeDocuments::new().answering(
        WIKIPEDIA,
        document(&source(WIKIPEDIA, "mbid-daft-punk"), Vec::new()),
    );
    let capabilities = FakeCapabilities::advertising(&[WIKIPEDIA]);
    let subject = subject(Some("mbid-daft-punk"), &[(WIKIPEDIA, "mbid-daft-punk")]);
    let service = service(
        Arc::clone(&store),
        documents,
        Arc::clone(&capabilities),
        narrow_order(&[(FieldId::BiographySummary, &[WIKIPEDIA])]),
    );

    assert_eq!(capabilities.calls(), 0);

    service
        .refresh(&request(subject, at(0), at(0)))
        .await
        .expect("a refresh with an advertised source");

    assert_eq!(
        capabilities.calls(),
        1,
        "the refresh job is the path that refreshes the advertisement"
    );
}

#[tokio::test]
async fn an_untrusted_licence_is_rejected_and_writes_nothing() {
    let store = FakeStore::new();
    let subject = subject(Some("mbid-daft-punk"), &[(WIKIPEDIA, "mbid-daft-punk")]);
    let mut untrusted = candidate(
        FieldId::BiographySummary,
        WIKIPEDIA,
        "mbid-daft-punk",
        text("a summary"),
    );
    untrusted.licence.redistribution_allowed = false;
    let documents = FakeDocuments::new().answering(
        WIKIPEDIA,
        document(&source(WIKIPEDIA, "mbid-daft-punk"), vec![untrusted]),
    );
    let capabilities = FakeCapabilities::advertising(&[WIKIPEDIA]);
    let service = service(
        Arc::clone(&store),
        documents,
        capabilities,
        narrow_order(&[(FieldId::BiographySummary, &[WIKIPEDIA])]),
    );

    let report = service
        .refresh(&request(subject, at(0), at(0)))
        .await
        .expect("a refresh with an untrusted candidate");

    assert_eq!(
        outcome(&report, FieldId::BiographySummary),
        FieldRefreshOutcome::Rejected
    );
    assert!(
        store.commits().is_empty(),
        "a rejected candidate writes nothing"
    );
    assert!(store.fact(FieldId::BiographySummary).is_none());
}

#[tokio::test]
async fn a_mismatched_address_echo_writes_nothing() {
    let store = FakeStore::new();
    let subject = subject(Some("mbid-daft-punk"), &[(WIKIPEDIA, "mbid-daft-punk")]);
    let documents = FakeDocuments::new().answering(
        WIKIPEDIA,
        document(
            &source(WIKIPEDIA, "some-other-identity"),
            vec![candidate(
                FieldId::BiographySummary,
                WIKIPEDIA,
                "mbid-daft-punk",
                text("a summary"),
            )],
        ),
    );
    let capabilities = FakeCapabilities::advertising(&[WIKIPEDIA]);
    let service = service(
        Arc::clone(&store),
        documents,
        capabilities,
        narrow_order(&[(FieldId::BiographySummary, &[WIKIPEDIA])]),
    );

    let report = service
        .refresh(&request(subject, at(0), at(0)))
        .await
        .expect("a refresh with a mismatched echo");

    assert_eq!(
        outcome(&report, FieldId::BiographySummary),
        FieldRefreshOutcome::Failed
    );
    assert!(store.commits().is_empty());
    assert_eq!(
        store.failure_classes(FieldId::BiographySummary),
        vec!["address_mismatch".to_owned()]
    );
}

#[tokio::test]
async fn an_unestablished_subject_reports_identity_unestablished() {
    let store = FakeStore::new();
    let documents = FakeDocuments::new();
    let capabilities = FakeCapabilities::advertising(&[WIKIPEDIA]);
    let subject = subject(None, &[]);
    let service = service(
        Arc::clone(&store),
        Arc::clone(&documents),
        capabilities,
        narrow_order(&[(FieldId::BiographySummary, &[WIKIPEDIA])]),
    );

    assert_eq!(
        service
            .refresh(&request(subject.clone(), at(0), at(0)))
            .await,
        Ok(RefreshReport::IdentityUnestablished)
    );
    assert_eq!(documents.call_count(), 0);

    let record = service
        .read_artist(&subject, &MetadataProfile::artist_display(), "en", at(0))
        .await
        .expect("a read for an unidentified subject");

    assert_eq!(
        entry(&record, FieldId::BiographySummary).absence,
        Some(AbsenceReason::IdentityUnestablished)
    );
    assert!(record.identity.is_none());
}

#[tokio::test]
async fn a_store_failure_is_a_typed_error() {
    let store = FakeStore::unavailable();
    let documents = FakeDocuments::new();
    let capabilities = FakeCapabilities::advertising(&[WIKIPEDIA]);
    let subject = subject(Some("mbid-daft-punk"), &[(WIKIPEDIA, "mbid-daft-punk")]);
    let service = service(
        Arc::clone(&store),
        documents,
        capabilities,
        narrow_order(&[(FieldId::BiographySummary, &[WIKIPEDIA])]),
    );

    assert_eq!(
        service
            .read_artist(&subject, &MetadataProfile::artist_display(), "en", at(0))
            .await,
        Err(MetadataError::Unavailable)
    );
    assert_eq!(
        service.refresh(&request(subject, at(0), at(0))).await,
        Err(MetadataError::Unavailable)
    );
}

#[test]
fn the_profile_names_the_frozen_field_set_in_order() {
    let profile = MetadataProfile::artist_display();

    assert_eq!(profile.as_str(), "artist-display.v1");
    assert_eq!(
        MetadataProfile::parse("artist-display.v1"),
        Some(profile.clone())
    );
    assert_eq!(MetadataProfile::parse("biography-only.v1"), None);
    assert_eq!(profile.fields().len(), 16);
    assert_eq!(profile.fields()[0], FieldId::IdentityName);
    assert_eq!(profile.fields()[15], FieldId::MetricListener);
    assert_eq!(
        crate::features::metadata::service::METADATA_ADAPTER_VERSION,
        "v1"
    );
}
