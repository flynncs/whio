use std::{str::FromStr, time::Duration};

use chrono::{DateTime, Utc};
use uuid::Uuid;

use crate::features::metadata::licences::LicenceTerms;

pub const DESCRIPTIVE_TTL: Duration = Duration::from_secs(14 * 24 * 60 * 60);
pub const RELATIONSHIP_TTL: Duration = Duration::from_secs(90 * 24 * 60 * 60);
pub const DISCOVERY_TTL: Duration = Duration::from_secs(24 * 60 * 60);
pub const NEGATIVE_TTL: Duration = Duration::from_secs(7 * 24 * 60 * 60);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MetadataEntityKind {
    Artist,
    Album,
}

impl MetadataEntityKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Artist => "artist",
            Self::Album => "album",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "artist" => Some(Self::Artist),
            "album" => Some(Self::Album),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FieldKind {
    Text,
    Count,
    Reference,
    CandidateList,
}

impl FieldKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Count => "count",
            Self::Reference => "reference",
            Self::CandidateList => "candidate_list",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FieldId {
    IdentityName,
    IdentityMbid,
    IdentityDescription,
    BiographySummary,
    BiographyContent,
    BiographyLanguage,
    ImagePortrait,
    ImageWide,
    PageUrl,
    Tag,
    Similar,
    TopTracks,
    MetricListeners,
    MetricPlays,
    MetricListen,
    MetricListener,
}

impl FieldId {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::IdentityName => "identity.name",
            Self::IdentityMbid => "identity.mbid",
            Self::IdentityDescription => "identity.description",
            Self::BiographySummary => "biography.summary",
            Self::BiographyContent => "biography.content",
            Self::BiographyLanguage => "biography.language",
            Self::ImagePortrait => "image.portrait",
            Self::ImageWide => "image.wide",
            Self::PageUrl => "page.url",
            Self::Tag => "tag",
            Self::Similar => "similar",
            Self::TopTracks => "top_tracks",
            Self::MetricListeners => "metric.listeners",
            Self::MetricPlays => "metric.plays",
            Self::MetricListen => "metric.listen",
            Self::MetricListener => "metric.listener",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "identity.name" => Some(Self::IdentityName),
            "identity.mbid" => Some(Self::IdentityMbid),
            "identity.description" => Some(Self::IdentityDescription),
            "biography.summary" => Some(Self::BiographySummary),
            "biography.content" => Some(Self::BiographyContent),
            "biography.language" => Some(Self::BiographyLanguage),
            "image.portrait" => Some(Self::ImagePortrait),
            "image.wide" => Some(Self::ImageWide),
            "page.url" => Some(Self::PageUrl),
            "tag" => Some(Self::Tag),
            "similar" => Some(Self::Similar),
            "top_tracks" => Some(Self::TopTracks),
            "metric.listeners" => Some(Self::MetricListeners),
            "metric.plays" => Some(Self::MetricPlays),
            "metric.listen" => Some(Self::MetricListen),
            "metric.listener" => Some(Self::MetricListener),
            _ => None,
        }
    }

    pub fn kind(&self) -> FieldKind {
        match self {
            Self::ImagePortrait | Self::ImageWide | Self::PageUrl => FieldKind::Reference,
            Self::Similar | Self::TopTracks => FieldKind::CandidateList,
            Self::MetricListeners
            | Self::MetricPlays
            | Self::MetricListen
            | Self::MetricListener => FieldKind::Count,
            _ => FieldKind::Text,
        }
    }

    pub fn ttl(&self) -> Duration {
        match self {
            Self::IdentityName | Self::IdentityMbid => RELATIONSHIP_TTL,
            Self::Similar
            | Self::TopTracks
            | Self::MetricListeners
            | Self::MetricPlays
            | Self::MetricListen
            | Self::MetricListener => DISCOVERY_TTL,
            _ => DESCRIPTIVE_TTL,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("unknown metadata field `{name}`")]
pub struct UnknownField {
    name: String,
}

impl UnknownField {
    fn new(name: &str) -> Self {
        Self {
            name: name.to_owned(),
        }
    }

    pub fn name(&self) -> &str {
        &self.name
    }
}

impl FromStr for FieldId {
    type Err = UnknownField;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::parse(value).ok_or_else(|| UnknownField::new(value))
    }
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct CandidateItem {
    pub name: String,
    pub external_id: Option<String>,
    pub url: Option<String>,
    pub rank: Option<u32>,
    pub score: Option<f64>,
    #[serde(default)]
    pub artists: Vec<String>,
    #[serde(default)]
    pub release_name: Option<String>,
    #[serde(default)]
    pub duration_ms: Option<u64>,
    #[serde(default)]
    pub isrc: Option<String>,
    #[serde(default)]
    pub recording_id: Option<String>,
    #[serde(default)]
    pub ranking_scope: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum FieldValue {
    Text(String),
    Count(u64),
    Reference(String),
    Candidates(Vec<CandidateItem>),
}

impl FieldValue {
    pub fn kind(&self) -> FieldKind {
        match self {
            Self::Text(_) => FieldKind::Text,
            Self::Count(_) => FieldKind::Count,
            Self::Reference(_) => FieldKind::Reference,
            Self::Candidates(_) => FieldKind::CandidateList,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Presence {
    Present,
    Absent,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Freshness {
    Fresh,
    Stale,
    Failed(String),
    Unrequested,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AbsenceReason {
    Unrequested,
    ProviderReportedNone,
    StaleAbsent,
    NotLicensed,
    CapabilityAbsent,
    IdentityUnestablished,
    FieldTooLarge,
    StoreBudgetExceeded,
    CredentialMissing,
}

impl AbsenceReason {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Unrequested => "unrequested",
            Self::ProviderReportedNone => "provider_reported_none",
            Self::StaleAbsent => "stale_absent",
            Self::NotLicensed => "not_licensed",
            Self::CapabilityAbsent => "capability_absent",
            Self::IdentityUnestablished => "identity_unestablished",
            Self::FieldTooLarge => "field_too_large",
            Self::StoreBudgetExceeded => "store_budget_exceeded",
            Self::CredentialMissing => "credential_missing",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BytesState {
    Servable,
    ReferenceOnly,
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attribution {
    pub text: String,
    pub url: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MatchBasis {
    Identifier,
    EstablishedSource,
}

impl MatchBasis {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Identifier => "identifier",
            Self::EstablishedSource => "established_source",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "identifier" => Some(Self::Identifier),
            "established_source" => Some(Self::EstablishedSource),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FactSource {
    pub provider_id: String,
    pub external_id: String,
    pub url: Option<String>,
    pub match_basis: MatchBasis,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageFact {
    pub role: String,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub content_type: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MetricFact {
    pub unit: String,
    pub population: Option<String>,
    pub period: Option<String>,
    pub as_of: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttemptError {
    pub class: String,
    pub message: String,
}

impl AttemptError {
    pub fn store_budget_exceeded() -> Self {
        Self {
            class: AbsenceReason::StoreBudgetExceeded.as_str().to_owned(),
            message: "the metadata store budget was exceeded".to_owned(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubjectKey {
    pub entity_kind: MetadataEntityKind,
    pub entity_id: Uuid,
    pub identity_revision: String,
    pub language: String,
    pub adapter_version: String,
}

impl SubjectKey {
    pub fn new(
        entity_kind: MetadataEntityKind,
        entity_id: Uuid,
        identity_revision: String,
        language: String,
        adapter_version: String,
    ) -> Self {
        Self {
            entity_kind,
            entity_id,
            identity_revision,
            language,
            adapter_version,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct FieldWrite {
    pub field: FieldId,
    pub source_provider_id: String,
    pub source_external_id: String,
    pub adapter_version: String,
    pub language: String,
    pub value: Option<FieldValue>,
    pub licence_id: Option<String>,
    pub licence_revision: Option<u32>,
    pub attribution: Option<Attribution>,
    pub source_url: Option<String>,
    pub match_basis: MatchBasis,
    pub operation: String,
    pub terms_basis: Option<String>,
    pub uncertainty: bool,
    pub image: Option<ImageFact>,
    pub metric: Option<MetricFact>,
    pub rank: Option<u32>,
    pub score: Option<f64>,
    pub fetched_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct StoredFact {
    pub field: FieldId,
    pub source: FactSource,
    pub value: FieldValue,
    pub licence: LicenceTerms,
    pub attribution: Option<Attribution>,
    pub source_url: Option<String>,
    pub operation: String,
    pub terms_basis: Option<String>,
    pub uncertainty: bool,
    pub image: Option<ImageFact>,
    pub metric: Option<MetricFact>,
    pub rank: Option<u32>,
    pub score: Option<f64>,
    pub bytes: Option<BytesState>,
    pub stored_bytes: u64,
    pub fetched_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredAttempt {
    pub field: FieldId,
    pub source_provider_id: String,
    pub fetched_at: Option<DateTime<Utc>>,
    pub attempted_at: DateTime<Utc>,
    pub error: Option<AttemptError>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntityKey {
    pub kind: MetadataEntityKind,
    pub id: Uuid,
}

impl EntityKey {
    pub fn new(kind: MetadataEntityKind, id: Uuid) -> Self {
        Self { kind, id }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Disagreement {
    pub field: FieldId,
    pub sources: Vec<FactSource>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FieldEntry {
    pub field: FieldId,
    pub presence: Presence,
    pub value: Option<FieldValue>,
    pub source: Option<FactSource>,
    pub licence: Option<LicenceTerms>,
    pub attribution: Option<Attribution>,
    pub bytes: Option<BytesState>,
    pub facts: Vec<StoredFact>,
    pub attempt: Option<StoredAttempt>,
    pub retrieved_at: Option<DateTime<Utc>>,
    pub freshness: Freshness,
    pub absence: Option<AbsenceReason>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MetadataRecord {
    pub entity: EntityKey,
    pub display_name: String,
    pub identity: Option<FactSource>,
    pub fields: Vec<FieldEntry>,
    pub disagreements: Vec<Disagreement>,
    pub contested: bool,
    pub generated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SubjectFacts {
    pub key: SubjectKey,
    pub facts: Vec<StoredFact>,
    pub attempts: Vec<StoredAttempt>,
    pub disagreements: Vec<Disagreement>,
}

impl SubjectFacts {
    pub fn new(key: SubjectKey, facts: Vec<StoredFact>, attempts: Vec<StoredAttempt>) -> Self {
        let disagreements = disagreements(&facts);

        Self {
            key,
            facts,
            attempts,
            disagreements,
        }
    }

    pub fn entry(&self, field: FieldId, now: DateTime<Utc>) -> FieldEntry {
        let facts = self
            .facts
            .iter()
            .filter(|fact| fact.field == field)
            .cloned()
            .collect::<Vec<_>>();
        let attempt = self
            .attempts
            .iter()
            .filter(|attempt| attempt.field == field)
            .max_by_key(|attempt| attempt.attempted_at)
            .cloned();
        let latest_success = facts
            .iter()
            .map(|fact| fact.fetched_at)
            .chain(attempt.iter().filter_map(|attempt| attempt.fetched_at))
            .max();
        let failure = attempt.as_ref().and_then(|attempt| {
            attempt
                .error
                .as_ref()
                .map(|error| (attempt.attempted_at, error.message.clone()))
        });
        let failed = failure.filter(|(attempted_at, _)| {
            latest_success.is_none_or(|success| *attempted_at > success)
        });
        let freshness = match (&failed, latest_success) {
            (Some((_, message)), _) => Freshness::Failed(message.clone()),
            (None, Some(success)) => {
                let ttl = if facts.is_empty() {
                    NEGATIVE_TTL
                } else {
                    field.ttl()
                };
                let age = now.signed_duration_since(success).num_seconds();

                if age < ttl.as_secs() as i64 {
                    Freshness::Fresh
                } else {
                    Freshness::Stale
                }
            }
            (None, None) => Freshness::Unrequested,
        };
        let absence = if !facts.is_empty() {
            None
        } else {
            match attempt.as_ref().and_then(|attempt| attempt.fetched_at) {
                Some(fetched_at) if failed.is_none() => {
                    let age = now.signed_duration_since(fetched_at).num_seconds();

                    if age >= NEGATIVE_TTL.as_secs() as i64 {
                        Some(AbsenceReason::StaleAbsent)
                    } else {
                        Some(AbsenceReason::ProviderReportedNone)
                    }
                }
                _ => None,
            }
        };
        let single = if facts.len() == 1 {
            facts.first()
        } else {
            None
        };

        FieldEntry {
            field,
            presence: if facts.is_empty() {
                Presence::Absent
            } else {
                Presence::Present
            },
            value: single.map(|fact| fact.value.clone()),
            source: single.map(|fact| fact.source.clone()),
            licence: single.map(|fact| fact.licence.clone()),
            attribution: single.and_then(|fact| fact.attribution.clone()),
            bytes: single.and_then(|fact| fact.bytes),
            facts,
            attempt,
            retrieved_at: latest_success,
            freshness,
            absence,
        }
    }
}

pub(crate) fn bytes_state(field: FieldId, licence: &LicenceTerms) -> Option<BytesState> {
    match field {
        FieldId::ImagePortrait | FieldId::ImageWide => Some(if licence.redistribution_allowed {
            BytesState::Servable
        } else if licence.cache_allowed {
            BytesState::ReferenceOnly
        } else {
            BytesState::Unavailable
        }),
        _ => None,
    }
}

fn disagreements(facts: &[StoredFact]) -> Vec<Disagreement> {
    let mut grouped: Vec<(FieldId, Vec<FactSource>)> = Vec::new();

    for fact in facts {
        match grouped.iter_mut().find(|(field, _)| *field == fact.field) {
            Some((_, sources)) => sources.push(fact.source.clone()),
            None => grouped.push((fact.field, vec![fact.source.clone()])),
        }
    }

    grouped
        .into_iter()
        .filter(|(_, sources)| sources.len() > 1)
        .map(|(field, sources)| Disagreement { field, sources })
        .collect()
}
