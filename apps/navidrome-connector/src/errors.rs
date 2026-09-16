use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde::Serialize;

use crate::request_id::RequestId;

#[derive(Serialize)]
struct ErrorBody {
    code: &'static str,
    message: &'static str,
    request_id: String,
}

#[derive(Debug)]
pub(crate) enum ConnectorError {
    InvalidRequest,
    UpstreamAuth,
    UpstreamForbidden,
    SourceNotFound,
    UpstreamBadResponse,
    ProviderUnavailable,
}

impl ConnectorError {
    fn status(&self) -> StatusCode {
        match self {
            Self::InvalidRequest => StatusCode::BAD_REQUEST,
            Self::UpstreamAuth => StatusCode::UNAUTHORIZED,
            Self::UpstreamForbidden => StatusCode::FORBIDDEN,
            Self::SourceNotFound => StatusCode::NOT_FOUND,
            Self::UpstreamBadResponse => StatusCode::BAD_GATEWAY,
            Self::ProviderUnavailable => StatusCode::SERVICE_UNAVAILABLE,
        }
    }

    fn code(&self) -> &'static str {
        match self {
            Self::InvalidRequest => "invalid_request",
            Self::UpstreamAuth => "upstream_auth_failed",
            Self::UpstreamForbidden => "upstream_forbidden",
            Self::SourceNotFound => "source_not_found",
            Self::UpstreamBadResponse => "upstream_bad_response",
            Self::ProviderUnavailable => "provider_unavailable",
        }
    }
    fn message(&self) -> &'static str {
        match self {
            Self::InvalidRequest => "The request is invalid.",
            Self::UpstreamAuth => "The upstream server rejected the credentials.",
            Self::UpstreamForbidden => "The upstream user lacks the required privileges.",
            Self::SourceNotFound => "The upstream song is unavailable.",
            Self::UpstreamBadResponse => "The upstream response was not understood.",
            Self::ProviderUnavailable => "The upstream server is temporarily unavailable.",
        }
    }

    pub(crate) fn response(&self, request_id: &RequestId) -> Response {
        (
            self.status(),
            Json(ErrorBody {
                code: self.code(),
                message: self.message(),
                request_id: request_id.0.clone(),
            }),
        )
            .into_response()
    }
}
