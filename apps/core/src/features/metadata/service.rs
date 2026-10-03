use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sqlx::PgPool;
use uuid::Uuid;

use crate::features::metadata::{
    capabilities::{CapabilitySnapshot, Snapshot},
    domain::{
        AbsenceReason, Disagreement, EntityKey, FactSource, FieldId, MatchBasis,
        MetadataEntityKind, MetadataRecord, StoredAttempt, StoredFact, SubjectFacts, SubjectKey,
    },
    freshness::{self, Jitter},
    refresh::MetadataDocuments,
    selection::{self, FieldOrder, MUSICBRAINZ},
    storage::{MetadataRepositoryError, MetadataStore},
};
use crate::features::music_library::domain::EnrichmentSubject;
use crate::jobs::metadata_refresh::MetadataRefreshV1;
use crate::platform::jobs::Enqueued;

pub const ARTIST_DISPLAY_PROFILE: &str = "artist-display.v1";
pub const METADATA_ADAPTER_VERSION: &str = "v1";

pub const ARTIST_DISPLAY_FIELDS: [FieldId; 16] = [
    FieldId::IdentityName,
    FieldId::IdentityMbid,
    FieldId::IdentityDescription,
    FieldId::BiographySummary,
    FieldId::BiographyContent,
    FieldId::BiographyLanguage,
    FieldId::ImagePortrait,
    FieldId::ImageWide,
    FieldId::PageUrl,
    FieldId::Tag,
    FieldId::Similar,
    FieldId::TopTracks,
    FieldId::MetricListeners,
    FieldId::MetricPlays,
    FieldId::MetricListen,
    FieldId::MetricListener,
];

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum MetadataError {
    #[error("metadata is disabled")]
    Disabled,
    #[error("metadata storage is unavailable")]
    Unavailable,
    #[error("metadata data is invalid")]
    InvalidData,
    #[error("the addressed metadata source is disabled")]
    DisabledSource,
    #[error("the metadata provider is unavailable")]
    ProviderUnavailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefreshOutcome {
    Inserted,
    Coalesced,
    Deferred,
    Unavailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefreshReason {
    Read,
    Discovery,
    Manual,
}

impl RefreshReason {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Discovery => "discovery",
            Self::Manual => "manual",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefreshPhase {
    Idle,
    Pending,
    Running,
    Deferred,
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefreshState {
    pub phase: RefreshPhase,
    pub identity_revision: String,
    pub job_id: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct MetadataProfile(String);

impl MetadataProfile {
    pub fn artist_display() -> Self {
        Self(ARTIST_DISPLAY_PROFILE.to_owned())
    }

    pub fn parse(value: &str) -> Option<Self> {
        (value == ARTIST_DISPLAY_PROFILE).then(|| Self(value.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn fields(&self) -> &'static [FieldId] {
        &ARTIST_DISPLAY_FIELDS
    }
}

#[async_trait]
pub trait SubjectSource: Send + Sync {
    async fn enrichment_subject(
        &self,
        kind: MetadataEntityKind,
        id: Uuid,
    ) -> Result<Option<EnrichmentSubject>, MetadataError>;
}

pub struct MetadataService {
    pool: PgPool,
    store: Arc<dyn MetadataStore>,
    documents: Arc<dyn MetadataDocuments>,
    capabilities: Arc<CapabilitySnapshot>,
    order: FieldOrder,
    adapter_version: String,
    jitter: Arc<dyn Jitter>,
}

impl MetadataService {
    pub fn new(
        pool: PgPool,
        store: Arc<dyn MetadataStore>,
        documents: Arc<dyn MetadataDocuments>,
        capabilities: Arc<CapabilitySnapshot>,
        order: FieldOrder,
        adapter_version: String,
        jitter: Arc<dyn Jitter>,
    ) -> Self {
        Self {
            pool,
            store,
            documents,
            capabilities,
            order,
            adapter_version,
            jitter,
        }
    }

    pub async fn read_artist(
        &self,
        subject: &EnrichmentSubject,
        profile: &MetadataProfile,
        language: &str,
        now: DateTime<Utc>,
    ) -> Result<MetadataRecord, MetadataError> {
        let stored = self
            .store
            .read_subject(&self.subject_key(subject, language))
            .await
            .map_err(metadata_error)?;
        let advertised = self.held_advertised(now);
        let established = established(subject);
        let mut fields = Vec::with_capacity(profile.fields().len());
        let mut disagreements = Vec::new();

        for field in profile.fields() {
            let field_facts = facts_for(&stored, *field);
            let field_attempts = attempts_for(&stored, *field);
            let selection = selection::select(&field_facts, &self.order);
            let mut entry = stored.entry(*field, now);

            if let Some(winner) = selection.winner {
                entry.value = Some(winner.value.clone());
                entry.source = Some(winner.source.clone());
                entry.licence = Some(winner.licence.clone());
                entry.attribution = winner.attribution.clone();
                entry.bytes = winner.bytes;
            }

            let latest_fact_at = field_facts.iter().map(|fact| fact.fetched_at).max();
            entry.freshness =
                freshness::freshness(latest_fact_at, entry.attempt.as_ref(), field.ttl(), now);
            entry.absence = self.absence_for(
                *field,
                &field_facts,
                &field_attempts,
                &advertised,
                established,
                now,
            );

            if !selection.disagreements.is_empty() {
                disagreements.push(Disagreement {
                    field: *field,
                    sources: selection
                        .disagreements
                        .iter()
                        .map(|fact| fact.source.clone())
                        .collect(),
                });
            }

            fields.push(entry);
        }

        Ok(MetadataRecord {
            entity: EntityKey::new(MetadataEntityKind::Artist, *subject.artist_id().as_uuid()),
            display_name: subject.name().to_owned(),
            identity: identity_fact(subject),
            fields,
            contested: !disagreements.is_empty(),
            disagreements,
            generated_at: now,
        })
    }

    pub async fn request_refresh(
        &self,
        subject: &EnrichmentSubject,
        profile: &MetadataProfile,
        language: &str,
        reason: RefreshReason,
    ) -> Result<RefreshOutcome, MetadataError> {
        self.request_refresh_with(subject, profile, language, reason, false)
            .await
    }

    pub async fn force_refresh(
        &self,
        subject: &EnrichmentSubject,
        profile: &MetadataProfile,
        language: &str,
        reason: RefreshReason,
    ) -> Result<RefreshOutcome, MetadataError> {
        self.request_refresh_with(subject, profile, language, reason, true)
            .await
    }

    async fn request_refresh_with(
        &self,
        subject: &EnrichmentSubject,
        profile: &MetadataProfile,
        language: &str,
        reason: RefreshReason,
        force: bool,
    ) -> Result<RefreshOutcome, MetadataError> {
        let now = Utc::now();
        let advertised = self.advertised(now).await;
        let fields = profile.fields();

        if !fields
            .iter()
            .any(|field| self.has_source(subject, &advertised, *field))
        {
            return Ok(RefreshOutcome::Unavailable);
        }

        let stored = self
            .store
            .read_subject(&self.subject_key(subject, language))
            .await
            .map_err(metadata_error)?;
        let due = force
            || fields.iter().any(|field| {
                self.has_source(subject, &advertised, *field)
                    && freshness::is_due(
                        &facts_for(&stored, *field),
                        &attempts_for(&stored, *field),
                        field.ttl(),
                        self.jitter.as_ref(),
                        now,
                    )
            });

        tracing::debug!(
            reason = reason.as_str(),
            artist_id = %subject.artist_id().as_uuid(),
            profile = profile.as_str(),
            "metadata refresh requested"
        );

        if !due {
            return Ok(RefreshOutcome::Deferred);
        }

        let payload = self.payload(subject, profile, language);

        match crate::jobs::metadata_refresh::request(&self.pool, &payload).await {
            Ok(Enqueued::Inserted { .. }) => Ok(RefreshOutcome::Inserted),
            Ok(Enqueued::Coalesced) => Ok(RefreshOutcome::Coalesced),
            Err(_) => Err(MetadataError::Unavailable),
        }
    }

    pub async fn refresh_state(
        &self,
        subject: &EnrichmentSubject,
        profile: &MetadataProfile,
        language: &str,
    ) -> Result<RefreshState, MetadataError> {
        let advertised = self.held_advertised(Utc::now());
        let identity_revision = subject.identity_revision().to_owned();

        if !profile
            .fields()
            .iter()
            .any(|field| self.has_source(subject, &advertised, *field))
        {
            return Ok(RefreshState {
                phase: RefreshPhase::Unavailable,
                identity_revision,
                job_id: None,
            });
        }

        let operation_id = self.payload(subject, profile, language).operation_id;
        let observed = crate::jobs::metadata_refresh::phase(&self.pool, operation_id)
            .await
            .map_err(|_| MetadataError::Unavailable)?;

        Ok(match observed {
            Some(observed) => RefreshState {
                phase: observed.phase,
                identity_revision,
                job_id: Some(observed.job_id),
            },
            None => RefreshState {
                phase: RefreshPhase::Idle,
                identity_revision,
                job_id: None,
            },
        })
    }

    pub(crate) fn payload(
        &self,
        subject: &EnrichmentSubject,
        profile: &MetadataProfile,
        language: &str,
    ) -> MetadataRefreshV1 {
        MetadataRefreshV1::for_artist(
            *subject.artist_id().as_uuid(),
            subject.identity_revision(),
            language,
            profile.as_str(),
        )
    }

    pub(crate) fn subject_key(&self, subject: &EnrichmentSubject, language: &str) -> SubjectKey {
        SubjectKey::new(
            MetadataEntityKind::Artist,
            *subject.artist_id().as_uuid(),
            subject.identity_revision().to_owned(),
            language.to_owned(),
            self.adapter_version.clone(),
        )
    }

    pub(crate) fn documents(&self) -> &dyn MetadataDocuments {
        self.documents.as_ref()
    }

    pub(crate) fn order(&self) -> &FieldOrder {
        &self.order
    }

    pub(crate) fn jitter(&self) -> &dyn Jitter {
        self.jitter.as_ref()
    }

    pub(crate) fn store(&self) -> &dyn MetadataStore {
        self.store.as_ref()
    }

    pub(crate) async fn advertised(&self, now: DateTime<Utc>) -> Snapshot {
        self.capabilities.current(now).await.unwrap_or_default()
    }

    pub(crate) fn held_advertised(&self, now: DateTime<Utc>) -> Snapshot {
        self.capabilities.held(now).unwrap_or_default()
    }

    pub(crate) fn has_source(
        &self,
        subject: &EnrichmentSubject,
        advertised: &Snapshot,
        field: FieldId,
    ) -> bool {
        self.eligible_sources(subject, advertised, field)
            .next()
            .is_some()
    }

    pub(crate) fn eligible_sources<'a>(
        &'a self,
        subject: &'a EnrichmentSubject,
        advertised: &'a Snapshot,
        field: FieldId,
    ) -> impl Iterator<Item = &'a crate::features::music_library::domain::SourceIdentity> {
        self.order
            .sources(field)
            .iter()
            .filter_map(move |provider_id| {
                advertised
                    .advertised(provider_id)
                    .then(|| {
                        subject
                            .identities()
                            .iter()
                            .find(|identity| identity.provider_id().as_str() == provider_id)
                    })
                    .flatten()
            })
    }

    fn absence_for(
        &self,
        field: FieldId,
        facts: &[StoredFact],
        attempts: &[StoredAttempt],
        advertised: &Snapshot,
        established: bool,
        now: DateTime<Utc>,
    ) -> Option<AbsenceReason> {
        if !facts.is_empty() {
            return None;
        }

        if !established {
            return Some(AbsenceReason::IdentityUnestablished);
        }

        let configured = self.order.sources(field);

        if !configured.is_empty() && advertised.enabled_sources(configured).is_empty() {
            return Some(AbsenceReason::CapabilityAbsent);
        }

        freshness::absence(facts, attempts, field.ttl(), now)
    }
}

pub(crate) fn facts_for(stored: &SubjectFacts, field: FieldId) -> Vec<StoredFact> {
    stored
        .facts
        .iter()
        .filter(|fact| fact.field == field)
        .cloned()
        .collect()
}

pub(crate) fn attempts_for(stored: &SubjectFacts, field: FieldId) -> Vec<StoredAttempt> {
    stored
        .attempts
        .iter()
        .filter(|attempt| attempt.field == field)
        .cloned()
        .collect()
}

impl MetadataService {
    pub(crate) fn adapter_version(&self) -> &str {
        &self.adapter_version
    }
}

pub(crate) fn established(subject: &EnrichmentSubject) -> bool {
    subject.musicbrainz_artist_id().is_some() || !subject.identities().is_empty()
}

fn identity_fact(subject: &EnrichmentSubject) -> Option<FactSource> {
    subject.musicbrainz_artist_id().map(|mbid| FactSource {
        provider_id: MUSICBRAINZ.to_owned(),
        external_id: mbid.to_owned(),
        url: None,
        match_basis: MatchBasis::Identifier,
    })
}

pub(crate) fn metadata_error(error: MetadataRepositoryError) -> MetadataError {
    match error {
        MetadataRepositoryError::Unavailable => MetadataError::Unavailable,
        MetadataRepositoryError::InvalidData => MetadataError::InvalidData,
    }
}
