use axum::{
    Json,
    extract::FromRequestParts,
    http::request::Parts,
    response::{IntoResponse, Response as AxumResponse},
};
use whio_subsonic_api::models;

use super::envelope::failed_envelope;
use super::query_auth;

use crate::{features::identity::service::PresentedAuth, platform::raw_query::RawQuery};

pub(crate) struct SubsonicAuth(pub(crate) PresentedAuth);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SubsonicError {
    MissingParameter,
    ConflictingAuthMechanisms,
}

impl SubsonicError {
    fn code(self) -> models::error::Code {
        match self {
            Self::MissingParameter => models::error::Code::Variant10,
            Self::ConflictingAuthMechanisms => models::error::Code::Variant43,
        }
    }

    fn message(self) -> &'static str {
        match self {
            Self::MissingParameter => "Required parameter is missing.",
            Self::ConflictingAuthMechanisms => {
                "Multiple conflicting authentication mechanisms provided."
            }
        }
    }
}

impl IntoResponse for SubsonicError {
    fn into_response(self) -> AxumResponse {
        Json(failed_envelope(self.code(), self.message())).into_response()
    }
}

impl<S> FromRequestParts<S> for SubsonicAuth
where
    S: Send + Sync,
{
    type Rejection = SubsonicError;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        let query = RawQuery::from_request_parts(parts, _state)
            .await
            .unwrap_or_else(|infallible| match infallible {});

        query_auth::presented(&query)
            .map(Self)
            .map_err(|error| match error {
                query_auth::QueryAuthError::MissingParameter => SubsonicError::MissingParameter,
                query_auth::QueryAuthError::ConflictingAuthMechanisms => {
                    SubsonicError::ConflictingAuthMechanisms
                }
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grammar_errors_map_to_spec_error_codes() {
        assert_eq!(
            SubsonicError::MissingParameter.code(),
            models::error::Code::Variant10
        );
        assert_eq!(
            SubsonicError::ConflictingAuthMechanisms.code(),
            models::error::Code::Variant43
        );
    }
}
