use std::time::Duration;

use async_trait::async_trait;
use thiserror::Error;
use whio_resolver_api::{
    apis::{
        Error as TransportError, configuration::Configuration, default_api::SearchCatalogueError,
    },
    models::{CatalogueCandidate as CatalogueCandidateDto, CatalogueSearchRequest},
};

use crate::catalogue::{CatalogueCandidate, ProviderId, SourceIdentity, ValidationError};

#[derive(Debug, Error)]
pub enum ResolverError {
    #[error("resolver request failed")]
    Request(#[source] reqwest::Error),

    #[error("resolver returned malformed JSON")]
    MalformedResponse(#[source] Box<dyn std::error::Error + Send + Sync>),

    #[error("resolver transport failed")]
    Transport(#[source] std::io::Error),

    #[error("search limit must be between 1 and 25")]
    InvalidLimit,

    #[error("search query must be between 1 and 500 characters")]
    InvalidQuery,

    #[error("request ID must be between 1 and 128 characters")]
    InvalidRequestId,

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
    transport: Configuration,
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

        Ok(Self {
            transport: Configuration {
                base_path: base_url.as_str().trim_end_matches('/').to_owned(),
                client: http,
                ..Configuration::default()
            },
        })
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
        if !(1..=500).contains(&query.chars().count()) {
            return Err(ResolverError::InvalidQuery);
        }

        if !(1..=25).contains(&limit) {
            return Err(ResolverError::InvalidLimit);
        }

        if request_id.is_some_and(|value| !(1..=128).contains(&value.chars().count())) {
            return Err(ResolverError::InvalidRequestId);
        }

        let body = CatalogueSearchRequest {
            query: query.to_owned(),
            limit: limit as i32,
        };

        let response_dto = whio_resolver_api::apis::default_api::search_catalogue(
            &self.transport,
            body,
            request_id,
        )
        .await
        .map_err(map_search_error)?;

        response_dto
            .results
            .into_iter()
            .map(map_candidate)
            .collect()
    }
}

fn map_search_error(error: TransportError<SearchCatalogueError>) -> ResolverError {
    match error {
        TransportError::Reqwest(error) => ResolverError::Request(error),
        TransportError::Serde(error) => ResolverError::MalformedResponse(Box::new(error)),
        TransportError::SerdePathToError(error) => {
            ResolverError::MalformedResponse(Box::new(error))
        }
        TransportError::Io(error) => ResolverError::Transport(error),
        TransportError::ResponseError(response) => match response.status {
            reqwest::StatusCode::BAD_REQUEST => ResolverError::InvalidRequest,
            reqwest::StatusCode::INTERNAL_SERVER_ERROR => ResolverError::Internal,
            reqwest::StatusCode::SERVICE_UNAVAILABLE => ResolverError::ProviderUnavailable,
            status => ResolverError::UnexpectedStatus(status),
        },
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
