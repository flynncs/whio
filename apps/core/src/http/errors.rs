use axum::{
    Json,
    extract::rejection::JsonRejection,
    http::StatusCode,
    response::{IntoResponse, Response as AxumResponse},
};
use serde::Serialize;

use crate::{
    catalogue::{CatalogueError, ValidationError},
    playback::PlaybackError,
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
    Playback {
        context: RequestContext,
        error: PlaybackError,
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
            ApiError::InvalidTrackId { context, error } => {
                tracing::debug!(error = %error, "invalid playback track id");
                invalid_request_response(context)
            }
            ApiError::MalformedRequest { context, error } => {
                tracing::debug!(error = %error, "malformed request");
                invalid_request_response(context)
            }
            ApiError::Catalogue { context, error } => {
                let (status, code, message) = match &error {
                    CatalogueError::Resolver(
                        ResolverError::Disabled
                        | ResolverError::ProviderUnavailable
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
            ApiError::Playback { context, error } => {
                let (status, code, message) = match &error {
                    PlaybackError::TrackNotFound => (
                        StatusCode::NOT_FOUND,
                        "track_not_found",
                        "The track was not found",
                    ),

                    PlaybackError::Resolver(
                        ResolverError::Disabled
                        | ResolverError::ProviderUnavailable
                        | ResolverError::Request(_)
                        | ResolverError::Transport(_),
                    ) => (
                        StatusCode::SERVICE_UNAVAILABLE,
                        "playback_unavailable",
                        "Playback is temporarily unavailable",
                    ),

                    PlaybackError::Resolver(_) => (
                        StatusCode::BAD_GATEWAY,
                        "resolver_failure",
                        "The playback resolver failed",
                    ),

                    PlaybackError::Repository(_) => (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "internal_error",
                        "An unexpected error occurred",
                    ),
                };

                match &error {
                    PlaybackError::TrackNotFound => tracing::debug!(
                        error = %error,
                        request_id = context.request_id().as_str(),
                        "playback track was not found"
                    ),
                    _ => tracing::error!(
                        error = %error,
                        request_id = context.request_id().as_str(),
                        "playback resolution failed"
                    ),
                }

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

#[cfg(test)]
mod tests {
    use axum::body::to_bytes;
    use serde_json::{Value, json};

    use super::*;
    use crate::{request::RequestId, tracks::TrackRepositoryError};

    fn context() -> RequestContext {
        RequestContext::new(RequestId::new("request-123".to_owned()).unwrap())
    }

    async fn assert_error(error: ApiError, status: StatusCode, expected: Value) {
        let response = error.into_response();

        assert_eq!(response.status(), status);
        let body = to_bytes(response.into_body(), 4096).await.unwrap();
        assert_eq!(serde_json::from_slice::<Value>(&body).unwrap(), expected);
    }

    #[tokio::test]
    async fn playback_errors_have_safe_public_responses() {
        assert_error(
            ApiError::Playback {
                context: context(),
                error: PlaybackError::TrackNotFound,
            },
            StatusCode::NOT_FOUND,
            json!({
                "code": "track_not_found",
                "message": "The track was not found",
                "request_id": "request-123"
            }),
        )
        .await;

        assert_error(
            ApiError::Playback {
                context: context(),
                error: PlaybackError::Resolver(ResolverError::SourceNotFound),
            },
            StatusCode::BAD_GATEWAY,
            json!({
                "code": "resolver_failure",
                "message": "The playback resolver failed",
                "request_id": "request-123"
            }),
        )
        .await;

        assert_error(
            ApiError::Playback {
                context: context(),
                error: PlaybackError::Resolver(ResolverError::ProviderUnavailable),
            },
            StatusCode::SERVICE_UNAVAILABLE,
            json!({
                "code": "playback_unavailable",
                "message": "Playback is temporarily unavailable",
                "request_id": "request-123"
            }),
        )
        .await;

        assert_error(
            ApiError::Playback {
                context: context(),
                error: PlaybackError::Repository(TrackRepositoryError::Unavailable),
            },
            StatusCode::INTERNAL_SERVER_ERROR,
            json!({
                "code": "internal_error",
                "message": "An unexpected error occurred",
                "request_id": "request-123"
            }),
        )
        .await;
    }

    #[tokio::test]
    async fn invalid_track_id_uses_the_safe_invalid_request_response() {
        let error = "not-a-track-id".parse::<uuid::Uuid>().unwrap_err();

        assert_error(
            ApiError::InvalidTrackId {
                context: context(),
                error,
            },
            StatusCode::BAD_REQUEST,
            json!({
                "code": "invalid_request",
                "message": "The request is invalid",
                "request_id": "request-123"
            }),
        )
        .await;
    }
}
