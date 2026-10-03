use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, Utc};

use crate::features::metadata::{
    capabilities::Snapshot,
    domain::{
        AbsenceReason, AttemptError, Attribution, FieldId, FieldValue, FieldWrite, ImageFact,
        MatchBasis, MetricFact,
    },
    freshness,
    licences::{self, LicenceAssertion},
    service::{
        MetadataError, MetadataProfile, MetadataService, attempts_for, facts_for, metadata_error,
    },
    storage::CommitOutcome,
};
use crate::features::music_library::domain::{EnrichmentSubject, ProviderId, SourceIdentity};

pub const ARTIST_DISPLAY_OPERATION: &str = "metadata.artist.display";
pub const MAX_DEFERRAL_HOURS: i64 = 24;
pub const MAX_TEXT_BYTES: usize = 65_536;
pub const MAX_SUMMARY_BYTES: usize = 4_096;
pub const MAX_TAG_BYTES: usize = 32 * 128;
pub const MAX_URL_BYTES: usize = 2_048;
pub const MAX_CANDIDATES: usize = 50;

#[derive(Debug, Clone, PartialEq)]
pub struct DocumentOutcome {
    pub address: SourceIdentity,
    pub source_revision: Option<String>,
    pub retrieved_at: DateTime<Utc>,
    pub candidates: Vec<DocumentCandidate>,
    pub errors: Vec<DocumentFieldError>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DocumentError {
    #[error("the addressed source does not serve this document")]
    Unsupported,
    #[error("the addressed identity does not exist at the source")]
    NotFound,
    #[error("the addressed source is unavailable")]
    Unavailable { retry_after: Option<Duration> },
    #[error("the addressed source answered a malformed document")]
    Malformed,
    #[error("the addressed source could not resolve the identity")]
    ResolutionFailed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocumentFieldError {
    pub field: FieldId,
    pub code: String,
    pub retryable: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CandidateSource {
    pub provider_id: String,
    pub external_id: String,
    pub url: Option<String>,
    pub match_basis: MatchBasis,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DocumentCandidate {
    pub field: FieldId,
    pub source: CandidateSource,
    pub value: FieldValue,
    pub language: Option<String>,
    pub licence: LicenceAssertion,
    pub attribution: Option<Attribution>,
    pub source_url: Option<String>,
    pub rank: Option<u32>,
    pub score: Option<f64>,
    pub image: Option<ImageFact>,
    pub metric: Option<MetricFact>,
}

#[async_trait]
pub trait MetadataDocuments: Send + Sync {
    async fn fetch(
        &self,
        source: &SourceIdentity,
        profile: &MetadataProfile,
        language: &str,
        fields: &[FieldId],
    ) -> Result<DocumentOutcome, DocumentError>;
}

#[derive(Debug, Clone, PartialEq)]
pub struct RefreshRequest {
    pub subject: EnrichmentSubject,
    pub profile: MetadataProfile,
    pub language: String,
    pub requested_at: DateTime<Utc>,
    pub now: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldRefresh {
    pub field: FieldId,
    pub outcome: FieldRefreshOutcome,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldRefreshOutcome {
    Skipped,
    Written,
    Absent,
    Failed,
    Rejected,
    Unavailable,
    Stale,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RefreshReport {
    Applied {
        fields: Vec<FieldRefresh>,
    },
    Deferred {
        retry_after: Duration,
        deadline: DateTime<Utc>,
    },
    IdentityUnestablished,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CandidateRejection {
    Kind,
    Source,
    ExternalId,
    Url,
    TooLarge,
    Rank,
    Score,
    Licence,
}

struct Progress<'a> {
    outcomes: &'a mut [FieldRefreshOutcome],
    present: &'a mut Vec<usize>,
}

impl MetadataService {
    pub async fn refresh(&self, request: &RefreshRequest) -> Result<RefreshReport, MetadataError> {
        let subject = &request.subject;

        if !crate::features::metadata::service::established(subject) {
            return Ok(RefreshReport::IdentityUnestablished);
        }

        let advertised = self.advertised(request.now).await;
        let stored = self
            .store()
            .read_subject(&self.subject_key(subject, &request.language))
            .await
            .map_err(metadata_error)?;
        let fields = request.profile.fields();
        let mut outcomes = vec![FieldRefreshOutcome::Skipped; fields.len()];
        let mut pending: Vec<usize> = Vec::new();

        for (index, field) in fields.iter().enumerate() {
            if !freshness::is_due(
                &facts_for(&stored, *field),
                &attempts_for(&stored, *field),
                field.ttl(),
                self.jitter(),
                request.now,
            ) {
                continue;
            }

            if !self.has_source(subject, &advertised, *field) {
                outcomes[index] = FieldRefreshOutcome::Unavailable;
                continue;
            }

            pending.push(index);
        }

        if pending.is_empty() {
            return Ok(RefreshReport::Applied {
                fields: report(fields, &outcomes),
            });
        }

        let mut sources: Vec<(String, &SourceIdentity)> = Vec::new();

        for index in &pending {
            for identity in self.eligible_sources(subject, &advertised, fields[*index]) {
                let provider_id = identity.provider_id().as_str().to_owned();

                if !sources.iter().any(|(seen, _)| *seen == provider_id) {
                    sources.push((provider_id, identity));
                }
            }
        }

        let mut present: Vec<usize> = Vec::new();
        let mut connected = false;

        for (provider_id, identity) in sources {
            let asked = pending
                .iter()
                .copied()
                .filter(|index| {
                    !present.contains(index)
                        && self.covers(subject, &advertised, fields[*index], &provider_id)
                })
                .collect::<Vec<_>>();

            if asked.is_empty() {
                continue;
            }

            let asked_fields = asked.iter().map(|index| fields[*index]).collect::<Vec<_>>();

            match self
                .documents()
                .fetch(identity, &request.profile, &request.language, &asked_fields)
                .await
            {
                Err(DocumentError::Unsupported) => continue,
                Err(DocumentError::Unavailable {
                    retry_after: Some(retry_after),
                }) => {
                    let deadline =
                        request.requested_at + chrono::Duration::hours(MAX_DEFERRAL_HOURS);
                    let error = AttemptError {
                        class: "provider_unavailable".to_owned(),
                        message: "provider_unavailable".to_owned(),
                    };

                    record(&asked, self, request, &provider_id, &error).await?;

                    connected = true;

                    for index in &asked {
                        outcomes[*index] = FieldRefreshOutcome::Failed;
                    }

                    if request.now < deadline {
                        let remaining = (deadline - request.now).to_std().unwrap_or_default();

                        return Ok(RefreshReport::Deferred {
                            retry_after: retry_after.min(remaining),
                            deadline,
                        });
                    }
                }
                Err(DocumentError::Unavailable { retry_after: None }) => {
                    return Err(MetadataError::ProviderUnavailable);
                }
                Err(error) => {
                    connected = true;

                    let class = match error {
                        DocumentError::NotFound => "source_not_found",
                        DocumentError::Malformed | DocumentError::ResolutionFailed => {
                            "resolution_failed"
                        }
                        _ => "provider_unavailable",
                    };
                    let attempt = AttemptError {
                        class: class.to_owned(),
                        message: class.to_owned(),
                    };

                    record(&asked, self, request, &provider_id, &attempt).await?;

                    for index in &asked {
                        outcomes[*index] = FieldRefreshOutcome::Failed;
                    }
                }
                Ok(document) => {
                    connected = true;

                    if document.address != *identity {
                        let attempt = AttemptError {
                            class: "address_mismatch".to_owned(),
                            message: "address_mismatch".to_owned(),
                        };

                        record(&asked, self, request, &provider_id, &attempt).await?;

                        for index in &asked {
                            outcomes[*index] = FieldRefreshOutcome::Failed;
                        }
                    } else {
                        self.apply(
                            request,
                            &advertised,
                            &document,
                            &asked,
                            fields,
                            Progress {
                                outcomes: &mut outcomes,
                                present: &mut present,
                            },
                        )
                        .await?;
                    }
                }
            }

            if pending.iter().all(|index| present.contains(index)) {
                break;
            }
        }

        if !connected && pending.iter().any(|index| !present.contains(index)) {
            return Err(MetadataError::DisabledSource);
        }

        Ok(RefreshReport::Applied {
            fields: report(fields, &outcomes),
        })
    }

    fn covers(
        &self,
        subject: &EnrichmentSubject,
        advertised: &Snapshot,
        field: FieldId,
        provider_id: &str,
    ) -> bool {
        self.order()
            .sources(field)
            .iter()
            .any(|source| source == provider_id)
            && advertised.advertised(provider_id)
            && subject
                .identities()
                .iter()
                .any(|identity| identity.provider_id().as_str() == provider_id)
    }

    async fn apply(
        &self,
        request: &RefreshRequest,
        advertised: &Snapshot,
        document: &DocumentOutcome,
        asked: &[usize],
        fields: &'static [FieldId],
        progress: Progress<'_>,
    ) -> Result<(), MetadataError> {
        let outcomes = progress.outcomes;
        let present = progress.present;
        for error in &document.errors {
            let Some(index) = asked
                .iter()
                .copied()
                .find(|index| fields[*index] == error.field)
            else {
                continue;
            };

            if present.contains(&index) {
                continue;
            }

            let attempt = AttemptError {
                class: error.code.clone(),
                message: error.code.clone(),
            };

            record(
                &[index],
                self,
                request,
                document.address.provider_id().as_str(),
                &attempt,
            )
            .await?;
            outcomes[index] = FieldRefreshOutcome::Failed;
        }

        for candidate in &document.candidates {
            let Some(index) = asked
                .iter()
                .copied()
                .find(|index| fields[*index] == candidate.field)
            else {
                continue;
            };

            if present.contains(&index) || outcomes[index] == FieldRefreshOutcome::Failed {
                continue;
            }

            let write = match self.write(request, advertised, document, candidate) {
                Ok(write) => write,
                Err(CandidateRejection::TooLarge) => {
                    let attempt = AttemptError {
                        class: AbsenceReason::FieldTooLarge.as_str().to_owned(),
                        message: "the value exceeds the field's size bound".to_owned(),
                    };

                    record(
                        &[index],
                        self,
                        request,
                        document.address.provider_id().as_str(),
                        &attempt,
                    )
                    .await?;
                    outcomes[index] = FieldRefreshOutcome::Failed;
                    continue;
                }
                Err(_) => {
                    outcomes[index] = FieldRefreshOutcome::Rejected;
                    continue;
                }
            };

            match self
                .store()
                .commit_field(&request.subject, &write)
                .await
                .map_err(metadata_error)?
            {
                CommitOutcome::Written => {
                    outcomes[index] = FieldRefreshOutcome::Written;
                    present.push(index);
                }
                CommitOutcome::Obsolete | CommitOutcome::SubjectGone => {
                    outcomes[index] = FieldRefreshOutcome::Stale;
                }
            }
        }

        for index in asked {
            if present.contains(index)
                || outcomes[*index] == FieldRefreshOutcome::Failed
                || outcomes[*index] == FieldRefreshOutcome::Rejected
                || outcomes[*index] == FieldRefreshOutcome::Stale
            {
                continue;
            }

            let write = empty_write(self, request, document, fields[*index]);
            self.store()
                .commit_field(&request.subject, &write)
                .await
                .map_err(metadata_error)?;
            outcomes[*index] = FieldRefreshOutcome::Absent;
        }

        Ok(())
    }

    fn write(
        &self,
        request: &RefreshRequest,
        advertised: &Snapshot,
        document: &DocumentOutcome,
        candidate: &DocumentCandidate,
    ) -> Result<FieldWrite, CandidateRejection> {
        validate(advertised, candidate)?;

        if exceeds_bound(candidate.field, &candidate.value) {
            return Err(CandidateRejection::TooLarge);
        }

        let terms = licences::resolve(&candidate.licence, licences::registry())
            .map_err(|_| CandidateRejection::Licence)?;

        Ok(FieldWrite {
            field: candidate.field,
            source_provider_id: candidate.source.provider_id.clone(),
            source_external_id: candidate.source.external_id.clone(),
            adapter_version: self.adapter_version().to_owned(),
            language: candidate
                .language
                .clone()
                .unwrap_or_else(|| request.language.clone()),
            value: Some(bounded(&candidate.value)),
            licence_id: Some(terms.id),
            licence_revision: Some(terms.revision),
            attribution: candidate.attribution.clone(),
            source_url: candidate
                .source_url
                .clone()
                .or_else(|| candidate.source.url.clone()),
            match_basis: candidate.source.match_basis,
            operation: ARTIST_DISPLAY_OPERATION.to_owned(),
            terms_basis: terms.terms_basis,
            uncertainty: terms.uncertainty,
            image: candidate.image.clone(),
            metric: candidate.metric.clone(),
            rank: candidate.rank,
            score: candidate.score,
            fetched_at: document.retrieved_at,
        })
    }
}

fn validate(
    advertised: &Snapshot,
    candidate: &DocumentCandidate,
) -> Result<(), CandidateRejection> {
    if candidate.value.kind() != candidate.field.kind() {
        return Err(CandidateRejection::Kind);
    }

    if ProviderId::new(candidate.source.provider_id.clone()).is_err()
        || !advertised.advertised(&candidate.source.provider_id)
    {
        return Err(CandidateRejection::Source);
    }

    if candidate.source.external_id.is_empty() || candidate.source.external_id.len() > 512 {
        return Err(CandidateRejection::ExternalId);
    }

    if candidate
        .source
        .url
        .iter()
        .chain(candidate.source_url.iter())
        .any(|url| url.len() > MAX_URL_BYTES)
    {
        return Err(CandidateRejection::Url);
    }

    if candidate
        .rank
        .is_some_and(|rank| rank >= MAX_CANDIDATES as u32)
    {
        return Err(CandidateRejection::Rank);
    }

    if candidate
        .score
        .is_some_and(|score| !score.is_finite() || score < 0.0)
    {
        return Err(CandidateRejection::Score);
    }

    Ok(())
}

fn exceeds_bound(field: FieldId, value: &FieldValue) -> bool {
    match value {
        FieldValue::Text(text) => text.len() > text_bound(field),
        FieldValue::Reference(reference) => reference.len() > MAX_URL_BYTES,
        FieldValue::Count(_) | FieldValue::Candidates(_) => false,
    }
}

fn bounded(value: &FieldValue) -> FieldValue {
    match value {
        FieldValue::Candidates(items) if items.len() > MAX_CANDIDATES => {
            FieldValue::Candidates(items[..MAX_CANDIDATES].to_vec())
        }
        value => value.clone(),
    }
}

fn text_bound(field: FieldId) -> usize {
    match field {
        FieldId::BiographyContent => MAX_TEXT_BYTES,
        FieldId::BiographySummary => MAX_SUMMARY_BYTES,
        FieldId::Tag => MAX_TAG_BYTES,
        _ => MAX_TEXT_BYTES,
    }
}

fn empty_write(
    service: &MetadataService,
    request: &RefreshRequest,
    document: &DocumentOutcome,
    field: FieldId,
) -> FieldWrite {
    FieldWrite {
        field,
        source_provider_id: document.address.provider_id().as_str().to_owned(),
        source_external_id: document.address.external_id().to_owned(),
        adapter_version: service.adapter_version().to_owned(),
        language: request.language.clone(),
        value: None,
        licence_id: None,
        licence_revision: None,
        attribution: None,
        source_url: None,
        match_basis: MatchBasis::Identifier,
        operation: ARTIST_DISPLAY_OPERATION.to_owned(),
        terms_basis: None,
        uncertainty: false,
        image: None,
        metric: None,
        rank: None,
        score: None,
        fetched_at: document.retrieved_at,
    }
}

async fn record(
    asked: &[usize],
    service: &MetadataService,
    request: &RefreshRequest,
    provider_id: &str,
    error: &AttemptError,
) -> Result<(), MetadataError> {
    for index in asked {
        let field = request.profile.fields()[*index];

        service
            .store()
            .record_failure(
                &request.subject,
                field,
                &request.language,
                provider_id,
                error,
            )
            .await
            .map_err(metadata_error)?;
    }

    Ok(())
}

fn report(fields: &'static [FieldId], outcomes: &[FieldRefreshOutcome]) -> Vec<FieldRefresh> {
    fields
        .iter()
        .zip(outcomes.iter())
        .map(|(field, outcome)| FieldRefresh {
            field: *field,
            outcome: *outcome,
        })
        .collect()
}
