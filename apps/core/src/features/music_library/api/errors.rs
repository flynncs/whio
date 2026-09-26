use crate::{
    features::music_library::{
        artwork::ArtworkError,
        catalogue::{CatalogueError, ProviderError as CatalogueProviderError, ValidationError},
    },
    platform::{
        http_response::{error_response, invalid_request_response},
        request::RequestContext,
    },
};
use axum::{
    extract::rejection::JsonRejection,
    http::StatusCode,
    response::{IntoResponse, Response},
};

pub(crate) enum ApiError {
    NotFound(RequestContext),
    InvalidRequest {
        context: RequestContext,
        error: ValidationError,
    },
    InvalidTrackId {
        context: RequestContext,
        error: uuid::Error,
    },
    MalformedRequest {
        context: RequestContext,
        error: JsonRejection,
    },
    Catalogue {
        context: RequestContext,
        error: CatalogueError,
    },
    Artwork {
        context: RequestContext,
        error: ArtworkError,
    },
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        match self {
            ApiError::NotFound(context) => {
                error_response(StatusCode::NOT_FOUND, "not_found", "Not found", context)
            }
            ApiError::InvalidRequest { context, error } => {
                tracing::debug!(error = %error, "invalid catalogue request");
                invalid_request_response(context)
            }
            ApiError::InvalidTrackId { context, error } => {
                tracing::debug!(error = %error, "invalid artwork id");
                invalid_request_response(context)
            }
            ApiError::MalformedRequest { context, error } => {
                tracing::debug!(error = %error, "malformed request");
                invalid_request_response(context)
            }
            ApiError::Catalogue { context, error } => {
                let (status, code, message) = match &error {
                    CatalogueError::Provider(
                        CatalogueProviderError::Disabled
                        | CatalogueProviderError::ProviderUnavailable
                        | CatalogueProviderError::Request(_),
                    ) => (
                        StatusCode::SERVICE_UNAVAILABLE,
                        "catalogue_unavailable",
                        "The catalogue is temporarily unavailable",
                    ),

                    CatalogueError::Provider(CatalogueProviderError::Access)
                    | CatalogueError::Repository(_) => (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "internal_error",
                        "An unexpected error occurred",
                    ),

                    CatalogueError::Provider(_) => (
                        StatusCode::BAD_GATEWAY,
                        "resolver_failure",
                        "The catalogue resolver failed",
                    ),
                };

                tracing::error!(
                    error = %error,
                    request_id = context.request_id().as_str(),
                    "catalogue search failed"
                );

                error_response(status, code, message, context)
            }
            ApiError::Artwork { context, error } => {
                let (status, code, message) = artwork_error_details(&error);

                tracing::error!(
                    error = %error,
                    request_id = context.request_id().as_str(),
                    "artwork resolution failed"
                );

                error_response(status, code, message, context)
            }
        }
    }
}

fn artwork_error_details(
    error: &crate::features::music_library::artwork::ArtworkError,
) -> (StatusCode, &'static str, &'static str) {
    use crate::features::music_library::artwork::ArtworkError;
    use crate::platform::http::FetchError;

    match error {
        ArtworkError::Fetch(FetchError::Request(_)) => (
            StatusCode::SERVICE_UNAVAILABLE,
            "artwork_unavailable",
            "Artwork is temporarily unavailable",
        ),
        ArtworkError::Fetch(_) => (
            StatusCode::BAD_GATEWAY,
            "artwork_failure",
            "The upstream artwork response was invalid",
        ),
        ArtworkError::Repository(_) | ArtworkError::Unavailable => (
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal_error",
            "An unexpected error occurred",
        ),
    }
}
