use std::{collections::BTreeSet, sync::Arc};

use async_trait::async_trait;
use chrono::Utc;
use whio_resolver_api::{
    apis::{Error as ApiError, configuration::Configuration, default_api},
    models,
};

use crate::features::metadata::{
    capabilities::{CapabilityError, CapabilitySource, Snapshot},
    domain::{Attribution, CandidateItem, FieldId, FieldValue, ImageFact, MatchBasis, MetricFact},
    licences::LicenceAssertion,
    refresh::{
        ARTIST_DISPLAY_OPERATION, CandidateSource, DocumentCandidate, DocumentError,
        DocumentFieldError, DocumentOutcome, MetadataDocuments,
    },
    selection::{LOCAL, METADATA_PROVIDERS, YOUTUBE_MUSIC},
    service::MetadataProfile,
};
use crate::features::music_library::domain::{ProviderId, SourceIdentity, SourceScope};

const ADDRESSING_PROVIDERS: [&str; 1] = [YOUTUBE_MUSIC];
const CAPABILITY_ADDRESSING_ERROR: &str = "capability_advertised_by_the_wrong_application";

pub struct DocumentRouting {
    youtube: Option<Arc<Configuration>>,
    metadata: Option<Arc<Configuration>>,
}

impl DocumentRouting {
    pub fn new(youtube: Option<Arc<Configuration>>, metadata: Option<Arc<Configuration>>) -> Self {
        Self { youtube, metadata }
    }

    fn client(&self, source: &SourceIdentity) -> Option<&Arc<Configuration>> {
        if ADDRESSING_PROVIDERS.contains(&source.provider_id().as_str()) {
            self.youtube.as_ref()
        } else {
            self.metadata.as_ref()
        }
    }

    async fn documents(
        &self,
        client: &Configuration,
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

        match default_api::fetch_artist_metadata(client, request, None).await {
            Ok(response) => Ok(document(response)),
            Err(error) => Err(document_error(error)),
        }
    }
}

#[async_trait]
impl MetadataDocuments for DocumentRouting {
    async fn fetch(
        &self,
        source: &SourceIdentity,
        profile: &MetadataProfile,
        language: &str,
        fields: &[FieldId],
    ) -> Result<DocumentOutcome, DocumentError> {
        let client = self.client(source).ok_or(DocumentError::Unsupported)?;

        self.documents(client, source, profile, language, fields)
            .await
    }
}

pub struct RoutingCapabilities {
    youtube: Option<Arc<Configuration>>,
    metadata: Option<Arc<Configuration>>,
}

impl RoutingCapabilities {
    pub fn new(youtube: Option<Arc<Configuration>>, metadata: Option<Arc<Configuration>>) -> Self {
        Self { youtube, metadata }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Application {
    Addressing,
    Metadata,
}

impl Application {
    fn owns(self, provider_id: &str) -> bool {
        match self {
            Application::Addressing => ADDRESSING_PROVIDERS.contains(&provider_id),
            Application::Metadata => METADATA_PROVIDERS.contains(&provider_id),
        }
    }
}

fn known_provider(provider_id: &str) -> bool {
    ADDRESSING_PROVIDERS.contains(&provider_id) || METADATA_PROVIDERS.contains(&provider_id)
}

async fn advertised(client: &Configuration) -> Result<Snapshot, CapabilityError> {
    let response = default_api::get_capabilities(client, None)
        .await
        .map_err(|_| CapabilityError::Unavailable)?;

    Ok(Snapshot::new(
        response
            .providers
            .into_iter()
            .filter(|provider| {
                provider
                    .capabilities
                    .iter()
                    .any(|capability| capability == ARTIST_DISPLAY_OPERATION)
            })
            .map(|provider| provider.provider_id)
            .collect::<Vec<_>>(),
    ))
}

#[async_trait]
impl CapabilitySource for RoutingCapabilities {
    async fn fetch(&self) -> Result<Snapshot, CapabilityError> {
        if self.youtube.is_none() && self.metadata.is_none() {
            return Err(CapabilityError::Disabled);
        }

        let mut providers: BTreeSet<String> = BTreeSet::new();
        let mut reachable = false;

        for (application, client) in [
            (Application::Addressing, self.youtube.as_ref()),
            (Application::Metadata, self.metadata.as_ref()),
        ] {
            let Some(client) = client else {
                continue;
            };
            let Ok(snapshot) = advertised(client).await else {
                continue;
            };

            reachable = true;

            for provider_id in snapshot.providers() {
                if application.owns(provider_id) || !known_provider(provider_id) {
                    providers.insert(provider_id.to_owned());
                } else {
                    tracing::warn!(
                        provider_id = %provider_id,
                        reason = CAPABILITY_ADDRESSING_ERROR,
                        "a resolver application advertises a source it does not serve"
                    );
                }
            }
        }

        if reachable {
            Ok(Snapshot::new(providers))
        } else {
            Err(CapabilityError::Unavailable)
        }
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
        default_api::FetchArtistMetadataError::UnknownValue(_) => None,
    }
}

fn document_error(error: ApiError<default_api::FetchArtistMetadataError>) -> DocumentError {
    match error {
        ApiError::ResponseError(response) => match response.status.as_u16() {
            404 => DocumentError::NotFound,
            409 => DocumentError::Unsupported,
            429 => DocumentError::Unavailable { retry_after: None },
            500..=599 => DocumentError::Unavailable { retry_after: None },
            400 => match code(&response.entity) {
                Some(models::ErrorCode::ResolutionFailed) => DocumentError::ResolutionFailed,
                Some(models::ErrorCode::UnsupportedProvider) => DocumentError::Unsupported,
                Some(models::ErrorCode::SourceNotFound) => DocumentError::NotFound,
                _ => DocumentError::Malformed,
            },
            _ => DocumentError::Malformed,
        },
        ApiError::Reqwest(_) => DocumentError::Unavailable { retry_after: None },
        _ => DocumentError::Malformed,
    }
}
