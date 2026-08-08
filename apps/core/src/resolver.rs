use std::time::Duration;

use async_trait::async_trait;
use thiserror::Error;

use crate::{
    catalogue::{CatalogueCandidate, ProviderId, SourceIdentity, ValidationError},
    resolver::generated::{
        CatalogueCandidate as CatalogueCandidateDto, CatalogueSearchRequest,
        CatalogueSearchResponse,
    },
};

#[allow(dead_code)]
mod generated;

#[derive(Debug, Error)]
pub enum ResolverError {
    #[error("resolver request failed")]
    Request(#[source] reqwest::Error),

    #[error("resolver URL is invalid")]
    InvalidUrl,

    #[error("search limit must be between 1 and 25")]
    InvalidLimit,

    #[error("resolver returned invalid catalogue data")]
    InvalidResponse(#[source] ValidationError),

    #[error("resolver rejected the request")]
    InvalidRequest,

    #[error("resolver provider is unavailable")]
    ProviderUnavailable,

    #[error("resolver failed internally")]
    Internal,

    #[error("resolver returned an unexpected HTTP status: {0}")]
    UnexpectedStatus(reqwest::StatusCode),
}

#[async_trait]
pub trait CatalogueResolver: Send + Sync {
    async fn search(
        &self,
        query: &str,
        limit: u32,
        request_id: Option<&str>,
    ) -> Result<Vec<CatalogueCandidate>, ResolverError>;
}

pub struct ResolverClient {
    http: reqwest::Client,
    base_url: reqwest::Url,
}

impl ResolverClient {
    pub fn new(
        base_url: reqwest::Url,
        connect_timeout: Duration,
        total_timeout: Duration,
    ) -> Result<Self, ResolverError> {
        let http = reqwest::Client::builder()
            .connect_timeout(connect_timeout)
            .timeout(total_timeout)
            .build()
            .map_err(ResolverError::Request)?;

        Ok(Self { http, base_url })
    }
}

#[async_trait]
impl CatalogueResolver for ResolverClient {
    async fn search(
        &self,
        query: &str,
        limit: u32,
        request_id: Option<&str>,
    ) -> Result<Vec<CatalogueCandidate>, ResolverError> {
        if !(1..=25).contains(&limit) {
            return Err(ResolverError::InvalidLimit);
        }

        let url = self
            .base_url
            .join("v1/catalogue/search")
            .map_err(|_| ResolverError::InvalidUrl)?;

        let body = CatalogueSearchRequest {
            query: query.to_owned(),
            limit: limit as i32,
        };

        let mut request = self.http.post(url).json(&body);

        if let Some(request_id) = request_id {
            request = request.header("X-Request-ID", request_id);
        }

        let response = request.send().await.map_err(ResolverError::Request)?;

        let status = response.status();

        if !status.is_success() {
            return Err(match status {
                reqwest::StatusCode::BAD_REQUEST => ResolverError::InvalidRequest,
                reqwest::StatusCode::SERVICE_UNAVAILABLE => ResolverError::ProviderUnavailable,
                _ => ResolverError::UnexpectedStatus(status),
            });
        }

        let response_dto = response
            .json::<CatalogueSearchResponse>()
            .await
            .map_err(ResolverError::Request)?;

        response_dto
            .results
            .into_iter()
            .map(map_candidate)
            .collect()
    }
}

fn map_candidate(candidate: CatalogueCandidateDto) -> Result<CatalogueCandidate, ResolverError> {
    let provider_id =
        ProviderId::new(candidate.source.provider_id).map_err(ResolverError::InvalidResponse)?;

    let source = SourceIdentity::new(provider_id, candidate.source.external_id)
        .map_err(ResolverError::InvalidResponse)?;

    let duration_ms = candidate
        .duration_ms
        .flatten()
        .map(u64::try_from)
        .transpose()
        .map_err(|_| ResolverError::InvalidResponse(ValidationError::InvalidDuration))?;

    CatalogueCandidate::new(source, candidate.title, candidate.artists, duration_ms)
        .map_err(ResolverError::InvalidResponse)
}
