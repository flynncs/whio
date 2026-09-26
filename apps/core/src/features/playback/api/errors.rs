use crate::{
    features::playback::{DeliveryError, PlaybackError},
    platform::{
        http_response::{error_response, invalid_request_response},
        request::RequestContext,
    },
};
use axum::{
    http::{HeaderValue, StatusCode, header::CONTENT_RANGE},
    response::{IntoResponse, Response},
};

pub(crate) enum ApiError {
    InvalidTrackId {
        context: RequestContext,
        error: uuid::Error,
    },
    PlaybackStream {
        context: RequestContext,
        error: PlaybackError,
    },
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        match self {
            ApiError::InvalidTrackId { context, error } => {
                tracing::debug!(error = %error, "invalid playback track id");
                invalid_request_response(context)
            }
            ApiError::PlaybackStream { context, error } => match error {
                PlaybackError::Delivery(DeliveryError::Range {
                    content_length,
                    error,
                }) => {
                    tracing::debug!(
                        error = %error,
                        request_id = context.request_id().as_str(),
                        "invalid media range"
                    );

                    let mut response = error_response(
                        StatusCode::RANGE_NOT_SATISFIABLE,
                        "range_not_satisfiable",
                        "The requested range is not satisfiable",
                        context,
                    );
                    response.headers_mut().insert(
                        CONTENT_RANGE,
                        HeaderValue::from_str(&format!("bytes */{content_length}"))
                            .expect("formatted content range is a valid header value"),
                    );
                    response
                }
                PlaybackError::Delivery(DeliveryError::Media(error)) => {
                    let (status, code, message) = media_error_details(&error);

                    tracing::error!(
                        error = %error,
                        request_id = context.request_id().as_str(),
                        "media stream failed"
                    );

                    error_response(status, code, message, context)
                }
                error => playback_error_response(error, context),
            },
        }
    }
}

fn playback_error_response(error: PlaybackError, context: RequestContext) -> Response {
    let (status, code, message) = playback_error_details(&error);

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

fn playback_error_details(error: &PlaybackError) -> (StatusCode, &'static str, &'static str) {
    match error {
        PlaybackError::TrackNotFound | PlaybackError::SourceNotFound => (
            StatusCode::NOT_FOUND,
            "track_not_found",
            "The track was not found",
        ),
        PlaybackError::CredentialsStale
        | PlaybackError::Unavailable
        | PlaybackError::Delivery(DeliveryError::Overloaded) => (
            StatusCode::SERVICE_UNAVAILABLE,
            "playback_unavailable",
            "Playback is temporarily unavailable",
        ),
        PlaybackError::Library(_)
        | PlaybackError::Internal(_)
        | PlaybackError::Delivery(DeliveryError::Spool | DeliveryError::Local(_)) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal_error",
            "An unexpected error occurred",
        ),
        PlaybackError::Provider(_) => (
            StatusCode::BAD_GATEWAY,
            "resolver_failure",
            "The playback resolver failed",
        ),
        // range and media failures are mapped by the caller, which adds the
        // status-specific headers and detail.
        PlaybackError::Delivery(DeliveryError::Range { .. } | DeliveryError::Media(_)) => (
            StatusCode::BAD_GATEWAY,
            "stream_failure",
            "The upstream media response was invalid",
        ),
    }
}

fn media_error_details(
    error: &crate::features::playback::media::MediaFetchError,
) -> (StatusCode, &'static str, &'static str) {
    match error {
        crate::features::playback::media::MediaFetchError::Request(_) => (
            StatusCode::SERVICE_UNAVAILABLE,
            "stream_unavailable",
            "The stream is temporarily unavailable",
        ),
        crate::features::playback::media::MediaFetchError::UnexpectedStatus(_)
        | crate::features::playback::media::MediaFetchError::DisallowedDestination
        | crate::features::playback::media::MediaFetchError::MissingContentLength
        | crate::features::playback::media::MediaFetchError::InvalidContentRange
        | crate::features::playback::media::MediaFetchError::ResponseTooLarge
        | crate::features::playback::media::MediaFetchError::RangesUnsupported => (
            StatusCode::BAD_GATEWAY,
            "stream_failure",
            "The upstream media response was invalid",
        ),
    }
}

#[cfg(test)]
mod tests {
    use axum::body::to_bytes;
    use serde_json::{Value, json};

    use super::*;
    use crate::{features::music_library::sources::SourceQueryError, platform::request::RequestId};

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
            ApiError::PlaybackStream {
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
            ApiError::PlaybackStream {
                context: context(),
                error: PlaybackError::SourceNotFound,
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
            ApiError::PlaybackStream {
                context: context(),
                error: PlaybackError::Unavailable,
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
            ApiError::PlaybackStream {
                context: context(),
                error: PlaybackError::Library(SourceQueryError::Unavailable),
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
