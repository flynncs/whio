use std::time::Duration;

use async_trait::async_trait;
use reqwest::StatusCode;

use super::errors::ResolverError;
use crate::{
    catalogue::{CatalogueCandidate, CatalogueResolver, CatalogueSearch},
    playback::{PlayableMedia, PlaybackResolver},
    request::RequestContext,
    tracks::SourceIdentity,
};
use whio_resolver_api::{
    apis::{
        Error as TransportError,
        configuration::Configuration,
        default_api::{ResolvePlaybackError, SearchCatalogueError},
    },
    models::{CatalogueSearchRequest, PlaybackResolveRequest, SourceIdentity as SourceIdentityDto},
};

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
        search: &CatalogueSearch,
        context: &RequestContext,
    ) -> Result<Vec<CatalogueCandidate>, ResolverError> {
        let body = CatalogueSearchRequest {
            query: search.query().as_str().to_owned(),
            limit: i32::from(search.limit().get()),
        };

        let response_dto = whio_resolver_api::apis::default_api::search_catalogue(
            &self.transport,
            body,
            Some(context.request_id().as_str()),
        )
        .await
        .map_err(map_search_error)?;

        response_dto
            .results
            .into_iter()
            .map(|candidate| candidate.try_into().map_err(ResolverError::InvalidResponse))
            .collect()
    }
}

#[async_trait]
impl PlaybackResolver for ResolverClient {
    async fn resolve(
        &self,
        source: &SourceIdentity,
        context: &RequestContext,
    ) -> Result<PlayableMedia, ResolverError> {
        let source = SourceIdentityDto::new(
            source.provider_id().as_str().to_owned(),
            source.external_id().to_owned(),
        );

        let body = PlaybackResolveRequest::new(source);

        let response = whio_resolver_api::apis::default_api::resolve_playback(
            &self.transport,
            body,
            Some(context.request_id().as_str()),
        )
        .await
        .map_err(map_playback_error)?;

        response
            .try_into()
            .map_err(ResolverError::InvalidPlaybackResponse)
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

fn map_playback_error(error: TransportError<ResolvePlaybackError>) -> ResolverError {
    match error {
        TransportError::Reqwest(error) => ResolverError::Request(error),
        TransportError::Serde(error) => ResolverError::MalformedResponse(Box::new(error)),
        TransportError::SerdePathToError(error) => {
            ResolverError::MalformedResponse(Box::new(error))
        }
        TransportError::Io(error) => ResolverError::Transport(error),

        TransportError::ResponseError(response) => match response.status {
            StatusCode::BAD_REQUEST => ResolverError::InvalidRequest,
            StatusCode::NOT_FOUND => ResolverError::SourceNotFound,
            StatusCode::UNPROCESSABLE_ENTITY => ResolverError::UnsupportedProvider,
            StatusCode::INTERNAL_SERVER_ERROR => ResolverError::Internal,
            StatusCode::BAD_GATEWAY => ResolverError::ResolutionFailed,
            StatusCode::SERVICE_UNAVAILABLE => ResolverError::ProviderUnavailable,
            status => ResolverError::UnexpectedStatus(status),
        },
    }
}
