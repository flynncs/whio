use axum::{
    Json,
    extract::rejection::JsonRejection,
    http::StatusCode,
    response::{IntoResponse, Response as AxumResponse},
};
use serde::Serialize;

use crate::{
    catalogue::{CatalogueError, ValidationError},
    request::RequestContext,
    resolvers::ResolverError,
};

#[derive(Serialize)]
struct ApiErrorResponse {
    code: &'static str,
    message: &'static str,
    request_id: String,
}

pub(crate) enum ApiError {
    NotFound(RequestContext),
    InvalidRequest {
        context: RequestContext,
        error: ValidationError,
    },
    MalformedRequest {
        context: RequestContext,
        error: JsonRejection,
    },
    Catalogue {
        context: RequestContext,
        error: CatalogueError,
    },
}

impl IntoResponse for ApiError {
    fn into_response(self) -> AxumResponse {
        match self {
            ApiError::NotFound(context) => {
                error_response(StatusCode::NOT_FOUND, "not_found", "Not found", context)
            }
            ApiError::InvalidRequest { context, error } => {
                tracing::debug!(error = %error, "invalid catalogue request");
                invalid_request_response(context)
            }
            ApiError::MalformedRequest { context, error } => {
                tracing::debug!(error = %error, "malformed catalogue request");
                invalid_request_response(context)
            }
            ApiError::Catalogue { context, error } => {
                let (status, code, message) = match &error {
                    CatalogueError::Resolver(
                        ResolverError::ProviderUnavailable
                        | ResolverError::Request(_)
                        | ResolverError::Transport(_),
                    ) => (
                        StatusCode::SERVICE_UNAVAILABLE,
                        "catalogue_unavailable",
                        "The catalogue is temporarily unavailable",
                    ),

                    CatalogueError::Resolver(_) => (
                        StatusCode::BAD_GATEWAY,
                        "resolver_failure",
                        "The catalogue resolver failed",
                    ),

                    CatalogueError::Repository(_) => (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "internal_error",
                        "An unexpected error occurred",
                    ),
                };

                tracing::error!(
                    error = %error,
                    request_id = context.request_id().as_str(),
                    "catalogue search failed"
                );

                error_response(status, code, message, context)
            }
        }
    }
}

fn invalid_request_response(context: RequestContext) -> AxumResponse {
    error_response(
        StatusCode::BAD_REQUEST,
        "invalid_request",
        "The request is invalid",
        context,
    )
}

fn error_response(
    status: StatusCode,
    code: &'static str,
    message: &'static str,
    context: RequestContext,
) -> AxumResponse {
    let body = ApiErrorResponse {
        code,
        message,
        request_id: context.request_id().as_str().to_owned(),
    };

    (status, Json(body)).into_response()
}
