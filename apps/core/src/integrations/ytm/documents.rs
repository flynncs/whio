use std::sync::Arc;

use async_trait::async_trait;
use chrono::Utc;
use whio_resolver_api::{
    apis::{Error as ApiError, configuration::Configuration, default_api},
    models,
};

use crate::features::{
    metadata::{
        domain::{
            Attribution, CandidateItem, FieldId, FieldValue, ImageFact, MatchBasis, MetricFact,
        },
        licences::LicenceAssertion,
        providers::DocumentProvider,
        refresh::{
            CandidateSource, DocumentCandidate, DocumentError, DocumentFieldError, DocumentOutcome,
        },
        selection::LOCAL,
        service::MetadataProfile,
    },
    music_library::domain::{ProviderId, SourceIdentity, SourceScope},
};

pub struct ResolverDocuments {
    provider_id: String,
    transport: Arc<Configuration>,
}

impl ResolverDocuments {
    pub fn new(provider_id: String, transport: Arc<Configuration>) -> Self {
        Self {
            provider_id,
            transport,
        }
    }

    async fn advertised(
        &self,
    ) -> Result<bool, crate::features::metadata::capabilities::CapabilityError> {
        let response = default_api::get_capabilities(&self.transport, None)
            .await
            .map_err(|_| crate::features::metadata::capabilities::CapabilityError::Unavailable)?;
        Ok(response.providers.iter().any(|provider| {
            provider.provider_id == self.provider_id
                && provider.capabilities.iter().any(|capability| {
                    capability == crate::features::metadata::refresh::ARTIST_DISPLAY_OPERATION
                })
        }))
    }

    async fn fetch_document(
        &self,
        source: &SourceIdentity,
        profile: &MetadataProfile,
        language: &str,
        fields: &[FieldId],
    ) -> Result<DocumentOutcome, DocumentError> {
        let request = models::MetadataArtistDocumentRequest {
            source: Box::new(models::SourceIdentity {
                provider_id: source.provider_id().as_str().to_owned(),
                external_id: source.external_id().to_owned(),
            }),
            language: Some(language.to_owned()),
            profile: profile_model(profile).ok_or(DocumentError::Unsupported)?,
            fields: Some(
                fields
                    .iter()
                    .map(|field| field.as_str().to_owned())
                    .collect(),
            ),
        };
        default_api::fetch_artist_metadata(&self.transport, request, None)
            .await
            .map(document)
            .map_err(document_error)
    }
}

#[async_trait]
impl DocumentProvider for ResolverDocuments {
    fn provider_id(&self) -> &str {
        &self.provider_id
    }

    async fn advertised(
        &self,
    ) -> Result<bool, crate::features::metadata::capabilities::CapabilityError> {
        ResolverDocuments::advertised(self).await
    }

    async fn fetch(
        &self,
        source: &SourceIdentity,
        profile: &MetadataProfile,
        language: &str,
        fields: &[FieldId],
    ) -> Result<DocumentOutcome, DocumentError> {
        self.fetch_document(source, profile, language, fields).await
    }
}

fn profile_model(profile: &MetadataProfile) -> Option<models::MetadataProfile> {
    match profile.as_str() {
        "artist-display.v1" => Some(models::MetadataProfile::ArtistDisplayV1),
        _ => None,
    }
}

fn document(response: models::MetadataArtistDocumentResponse) -> DocumentOutcome {
    DocumentOutcome {
        address: address(&response.address),
        source_revision: response.source_revision.flatten(),
        retrieved_at: response.retrieved_at.with_timezone(&Utc),
        candidates: response
            .candidates
            .into_iter()
            .filter_map(candidate)
            .collect(),
        errors: response
            .errors
            .into_iter()
            .filter_map(|error| {
                FieldId::parse(&error.field).map(|field| DocumentFieldError {
                    field,
                    code: error.code.to_string(),
                    retryable: error.retryable,
                })
            })
            .collect(),
    }
}

fn address(source: &models::SourceIdentity) -> SourceIdentity {
    let provider_id =
        ProviderId::new(source.provider_id.clone()).unwrap_or_else(|_| local_provider());

    SourceIdentity::new(provider_id, SourceScope::Global, source.external_id.clone())
        .unwrap_or_else(|_| local_identity())
}

fn local_provider() -> ProviderId {
    ProviderId::new(LOCAL.to_owned()).expect("the local provider id is a contract identifier")
}

fn local_identity() -> SourceIdentity {
    SourceIdentity::new(local_provider(), SourceScope::Global, "_".to_owned())
        .expect("a local identity is constructible")
}

fn candidate(candidate: models::MetadataFieldCandidate) -> Option<DocumentCandidate> {
    let field = FieldId::parse(&candidate.field)?;
    let value = field_value(field, &candidate)?;

    Some(DocumentCandidate {
        field,
        source: CandidateSource {
            provider_id: candidate.source.provider_id.clone(),
            external_id: candidate.source.external_id.clone(),
            url: candidate.source.url.clone().flatten(),
            match_basis: match candidate.source.match_basis {
                models::candidate_source::MatchBasis::Identifier => MatchBasis::Identifier,
                models::candidate_source::MatchBasis::EstablishedSource => {
                    MatchBasis::EstablishedSource
                }
            },
        },
        value,
        language: candidate.language.clone().flatten(),
        licence: LicenceAssertion {
            id: candidate.licence.id.clone(),
            url: Some(candidate.licence.url.clone()),
            attribution_required: candidate.licence.attribution_required,
            redistribution_allowed: candidate.licence.redistribution_allowed,
            cache_allowed: candidate.licence.cache_allowed,
            share_alike: candidate.licence.share_alike,
            non_commercial: candidate.licence.non_commercial,
            storage_cap_bytes: candidate
                .licence
                .storage_cap_bytes
                .flatten()
                .and_then(|bytes| u64::try_from(bytes).ok()),
        },
        attribution: candidate
            .attribution
            .clone()
            .flatten()
            .map(|attribution| Attribution {
                text: attribution.text,
                url: attribution.url.flatten(),
            }),
        source_url: candidate.reference.clone().flatten(),
        rank: candidate
            .rank
            .flatten()
            .and_then(|rank| u32::try_from(rank).ok()),
        score: candidate.score.flatten(),
        image: candidate.image.clone().flatten().map(|image| ImageFact {
            role: image.role,
            width: image
                .width
                .flatten()
                .and_then(|width| u32::try_from(width).ok()),
            height: image
                .height
                .flatten()
                .and_then(|height| u32::try_from(height).ok()),
            content_type: image
                .content_type
                .flatten()
                .unwrap_or_else(|| "application/octet-stream".to_owned()),
        }),
        metric: candidate.metric.clone().flatten().map(|metric| MetricFact {
            unit: metric.unit,
            population: metric.population.flatten(),
            period: metric.period.flatten(),
            as_of: metric
                .as_of
                .flatten()
                .map(|as_of| as_of.with_timezone(&Utc)),
        }),
    })
}

fn field_value(field: FieldId, candidate: &models::MetadataFieldCandidate) -> Option<FieldValue> {
    let reference = candidate.reference.clone().flatten().or_else(|| {
        candidate
            .image
            .clone()
            .flatten()
            .map(|image| image.reference)
    });

    match field.kind() {
        crate::features::metadata::domain::FieldKind::Text => candidate
            .text
            .clone()
            .flatten()
            .or(reference)
            .map(FieldValue::Text),
        crate::features::metadata::domain::FieldKind::Count => candidate
            .count
            .flatten()
            .and_then(|count| u64::try_from(count).ok())
            .map(FieldValue::Count),
        crate::features::metadata::domain::FieldKind::Reference => {
            reference.map(FieldValue::Reference)
        }
        crate::features::metadata::domain::FieldKind::CandidateList => {
            candidate.candidates.clone().flatten().map(|items| {
                FieldValue::Candidates(
                    items
                        .into_iter()
                        .map(|item| CandidateItem {
                            name: item.name,
                            external_id: Some(item.source.external_id),
                            url: item.source.url.flatten(),
                            rank: item
                                .rank
                                .flatten()
                                .and_then(|rank| u32::try_from(rank).ok()),
                            score: item.score.flatten(),
                            artists: item.artists.flatten().unwrap_or_default(),
                            release_name: item.release_name.flatten(),
                            duration_ms: item
                                .duration_ms
                                .flatten()
                                .and_then(|duration| u64::try_from(duration).ok()),
                            isrc: None,
                            recording_id: None,
                            ranking_scope: candidate.ranking_scope.clone().flatten(),
                        })
                        .collect(),
                )
            })
        }
    }
}

fn code(entity: &Option<default_api::FetchArtistMetadataError>) -> Option<models::ErrorCode> {
    match entity.as_ref()? {
        default_api::FetchArtistMetadataError::Status400(response) => Some(response.code),
        default_api::FetchArtistMetadataError::Status404(response) => Some(response.code),
        default_api::FetchArtistMetadataError::Status422(response) => Some(response.code),
        default_api::FetchArtistMetadataError::Status500(response) => Some(response.code),
        default_api::FetchArtistMetadataError::Status503(response) => Some(response.code),
        default_api::FetchArtistMetadataError::UnknownValue(value) => value
            .get("code")
            .cloned()
            .and_then(|value| serde_json::from_value(value).ok()),
    }
}

pub(crate) fn document_error(
    error: ApiError<default_api::FetchArtistMetadataError>,
) -> DocumentError {
    match error {
        ApiError::ResponseError(response) => {
            let code = code(&response.entity);
            match code {
                Some(models::ErrorCode::SourceNotFound) => DocumentError::NotFound,
                Some(models::ErrorCode::UnsupportedProvider) => DocumentError::Unsupported,
                Some(models::ErrorCode::ResolutionFailed) => DocumentError::ResolutionFailed,
                Some(models::ErrorCode::ProviderUnavailable) => {
                    DocumentError::Unavailable { retry_after: None }
                }
                Some(models::ErrorCode::InvalidRequest | models::ErrorCode::InternalError) => {
                    DocumentError::Malformed
                }
                None => match response.status.as_u16() {
                    404 => DocumentError::NotFound,
                    422 => DocumentError::Unsupported,
                    429 | 500..=599 => DocumentError::Unavailable { retry_after: None },
                    _ => DocumentError::Malformed,
                },
            }
        }
        ApiError::Reqwest(_) => DocumentError::Unavailable { retry_after: None },
        _ => DocumentError::Malformed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognized_unknown_response_codes_classify_before_status() {
        let cases = [
            ("source_not_found", DocumentError::NotFound),
            ("unsupported_provider", DocumentError::Unsupported),
            ("resolution_failed", DocumentError::ResolutionFailed),
            (
                "provider_unavailable",
                DocumentError::Unavailable { retry_after: None },
            ),
            ("invalid_request", DocumentError::Malformed),
            ("internal_error", DocumentError::Malformed),
        ];

        for (code, expected) in cases {
            let error = ApiError::ResponseError(whio_resolver_api::apis::ResponseContent {
                status: reqwest::StatusCode::BAD_GATEWAY,
                content: String::new(),
                entity: Some(default_api::FetchArtistMetadataError::UnknownValue(
                    serde_json::json!({"code": code}),
                )),
            });
            assert_eq!(document_error(error), expected, "{code}");
        }
    }
}
