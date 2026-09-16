use std::time::Duration;

use crate::{catalogue::CatalogueSearch, request::RequestContext, tracks::TrackMetadata};
use async_trait::async_trait;
use thiserror::Error;
use whio_navidrome_connector_api::{
    apis::{
        Error as TransportError, configuration::Configuration, default_api::SearchCatalogueError,
    },
    models::{ResolveRequest, SearchRequest},
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpstreamTarget {
    pub base_url: String,
    pub username: String,
    pub password: String,
}

pub struct UpstreamCandidate {
    pub external_id: String,
    pub title: String,
    pub artists: Vec<String>,
    pub duration_ms: Option<u64>,
}

#[derive(Debug, Clone, Error)]
pub enum UpstreamError {
    #[error("upstream credentials were rejected")]
    Auth,
    #[error("upstream user lacks privileges")]
    Forbidden,
    #[error("upstream song was not found")]
    NotFound,
    #[error("upstream request was invalid")]
    InvalidRequest,
    #[error("upstream response was unusable")]
    InvalidResponse,
    #[error("upstream server is unavailable")]
    Unavailable,
}

#[async_trait]
pub trait UpstreamTransport: Send + Sync {
    async fn search(
        &self,
        target: &UpstreamTarget,
        search: &CatalogueSearch,
        context: &RequestContext,
    ) -> Result<Vec<UpstreamCandidate>, UpstreamError>;

    async fn resolve(
        &self,
        target: &UpstreamTarget,
        external_id: &str,
        context: &RequestContext,
    ) -> Result<String, UpstreamError>;
}

pub struct ConnectorUpstreamTransport {
    transport: Configuration,
}

impl ConnectorUpstreamTransport {
    pub fn new(
        connector_url: reqwest::Url,
        connect_timeout: Duration,
        total_timeout: Duration,
    ) -> Result<Self, UpstreamError> {
        let http = reqwest::Client::builder()
            .connect_timeout(connect_timeout)
            .timeout(total_timeout)
            .build()
            .map_err(|_| UpstreamError::Unavailable)?;

        Ok(Self {
            transport: Configuration {
                base_path: connector_url.as_str().trim_end_matches('/').to_owned(),
                client: http,
                ..Configuration::default()
            },
        })
    }
}

#[async_trait]
impl UpstreamTransport for ConnectorUpstreamTransport {
    async fn search(
        &self,
        target: &UpstreamTarget,
        search: &CatalogueSearch,
        context: &RequestContext,
    ) -> Result<Vec<UpstreamCandidate>, UpstreamError> {
        let mut body = SearchRequest::new(
            target.base_url.clone(),
            target.username.clone(),
            target.password.clone(),
            search.query().as_str().to_owned(),
            i32::from(search.limit().get()),
        );
        body.offset = Some(search.offset().get() as i32);

        let response = whio_navidrome_connector_api::apis::default_api::search_catalogue(
            &self.transport,
            body,
            Some(context.request_id().as_str()),
        )
        .await
        .map_err(map_search_error)?;

        Ok(response
            .results
            .into_iter()
            .filter_map(|candidate| {
                if candidate.external_id.is_empty() {
                    return None;
                }
                let duration_ms = candidate
                    .duration_ms
                    .flatten()
                    .and_then(|ms| u64::try_from(ms).ok());
                TrackMetadata::new(candidate.title, candidate.artists, duration_ms)
                    .ok()
                    .map(|metadata| UpstreamCandidate {
                        external_id: candidate.external_id,
                        title: metadata.title().to_owned(),
                        artists: metadata.artists().to_owned(),
                        duration_ms: metadata.duration_ms(),
                    })
            })
            .collect())
    }

    async fn resolve(
        &self,
        target: &UpstreamTarget,
        external_id: &str,
        context: &RequestContext,
    ) -> Result<String, UpstreamError> {
        let body = ResolveRequest::new(
            target.base_url.clone(),
            target.username.clone(),
            target.password.clone(),
            external_id.to_owned(),
        );

        let response = whio_navidrome_connector_api::apis::default_api::resolve_playback(
            &self.transport,
            body,
            Some(context.request_id().as_str()),
        )
        .await
        .map_err(|error| match error {
            TransportError::Reqwest(_) | TransportError::Io(_) => UpstreamError::Unavailable,
            TransportError::Serde(_) | TransportError::SerdePathToError(_) => {
                UpstreamError::InvalidResponse
            }
            TransportError::ResponseError(response) => match response.status {
                reqwest::StatusCode::UNAUTHORIZED => UpstreamError::Auth,
                reqwest::StatusCode::FORBIDDEN => UpstreamError::Forbidden,
                reqwest::StatusCode::BAD_REQUEST => UpstreamError::InvalidRequest,
                reqwest::StatusCode::NOT_FOUND => UpstreamError::NotFound,
                _ => UpstreamError::Unavailable,
            },
        })?;

        Ok(response.url)
    }
}

fn map_search_error(error: TransportError<SearchCatalogueError>) -> UpstreamError {
    match error {
        TransportError::Reqwest(_) | TransportError::Io(_) => UpstreamError::Unavailable,
        TransportError::Serde(_) | TransportError::SerdePathToError(_) => {
            UpstreamError::InvalidResponse
        }
        TransportError::ResponseError(response) => match response.status {
            reqwest::StatusCode::UNAUTHORIZED => UpstreamError::Auth,
            reqwest::StatusCode::FORBIDDEN => UpstreamError::Forbidden,
            reqwest::StatusCode::BAD_REQUEST => UpstreamError::InvalidRequest,
            _ => UpstreamError::Unavailable,
        },
    }
}

pub struct NoUpstreamSources;

#[async_trait]
impl UpstreamTransport for NoUpstreamSources {
    async fn search(
        &self,
        _target: &UpstreamTarget,
        _search: &CatalogueSearch,
        _context: &RequestContext,
    ) -> Result<Vec<UpstreamCandidate>, UpstreamError> {
        Ok(Vec::new())
    }

    async fn resolve(
        &self,
        _target: &UpstreamTarget,
        _external_id: &str,
        _context: &RequestContext,
    ) -> Result<String, UpstreamError> {
        Err(UpstreamError::Unavailable)
    }
}
