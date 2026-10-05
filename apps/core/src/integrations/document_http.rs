use std::time::Duration;

use reqwest::{Response, StatusCode};
use serde_json::Value;

use crate::features::metadata::{
    domain::FieldId,
    refresh::{DocumentError, DocumentFieldError},
};

#[derive(Clone, Copy)]
pub(crate) enum NotFoundIs {
    SourceNotFound,
    ResolutionFailed,
}

pub(crate) async fn json(
    sent: Result<Response, reqwest::Error>,
    not_found: NotFoundIs,
) -> Result<Value, DocumentError> {
    let response = prepare(sent, not_found)?;
    response
        .json::<Value>()
        .await
        .map_err(|_| DocumentError::ResolutionFailed)
}

pub(crate) fn prepare(
    sent: Result<Response, reqwest::Error>,
    not_found: NotFoundIs,
) -> Result<Response, DocumentError> {
    let response = sent.map_err(|_| DocumentError::Unavailable { retry_after: None })?;
    let status = response.status();
    if status == StatusCode::NOT_FOUND {
        return Err(match not_found {
            NotFoundIs::SourceNotFound => DocumentError::NotFound,
            NotFoundIs::ResolutionFailed => DocumentError::ResolutionFailed,
        });
    }
    if status == StatusCode::TOO_MANY_REQUESTS || status.is_server_error() {
        let retry_after = response
            .headers()
            .get(reqwest::header::RETRY_AFTER)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.parse::<f64>().ok())
            .filter(|seconds| seconds.is_finite() && *seconds >= 0.0)
            .and_then(|seconds| Duration::try_from_secs_f64(seconds).ok());
        return Err(DocumentError::Unavailable { retry_after });
    }
    if status.as_u16() >= 400 {
        return Err(DocumentError::ResolutionFailed);
    }
    Ok(response)
}

pub(crate) fn field_error(field: FieldId, error: &DocumentError) -> DocumentFieldError {
    let (code, retryable) = match error {
        DocumentError::NotFound => ("source_not_found", false),
        DocumentError::Unavailable { .. } => ("provider_unavailable", true),
        _ => ("resolution_failed", false),
    };
    DocumentFieldError {
        field,
        code: code.to_owned(),
        retryable,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{Router, http::StatusCode, response::IntoResponse, routing::get};
    use tokio::net::TcpListener;

    async fn url(
        status: StatusCode,
        retry_after: Option<&'static str>,
        body: &'static str,
    ) -> String {
        let app = Router::new().route(
            "/",
            get(move || async move {
                let mut response = (status, body).into_response();
                if let Some(value) = retry_after {
                    response
                        .headers_mut()
                        .insert(reqwest::header::RETRY_AFTER, value.parse().unwrap());
                }
                response
            }),
        );
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        format!("http://{address}/")
    }

    async fn sent(url: &str) -> Result<Response, reqwest::Error> {
        reqwest::Client::new().get(url).send().await
    }

    #[tokio::test]
    async fn transport_errors_are_unavailable() {
        let error = reqwest::Client::new()
            .get("http://127.0.0.1:0")
            .send()
            .await;
        assert_eq!(
            json(error, NotFoundIs::SourceNotFound).await,
            Err(DocumentError::Unavailable { retry_after: None })
        );
    }

    #[tokio::test]
    async fn not_found_policy_is_provider_specific() {
        let url = url(StatusCode::NOT_FOUND, None, "{}").await;
        assert_eq!(
            json(sent(&url).await, NotFoundIs::SourceNotFound).await,
            Err(DocumentError::NotFound)
        );
        assert_eq!(
            json(sent(&url).await, NotFoundIs::ResolutionFailed).await,
            Err(DocumentError::ResolutionFailed)
        );
    }

    #[tokio::test]
    async fn throttling_and_server_errors_read_retry_after() {
        for status in [
            StatusCode::TOO_MANY_REQUESTS,
            StatusCode::BAD_GATEWAY,
            StatusCode::INTERNAL_SERVER_ERROR,
        ] {
            let url = url(status, Some("12"), "{}").await;
            assert_eq!(
                json(sent(&url).await, NotFoundIs::SourceNotFound).await,
                Err(DocumentError::Unavailable {
                    retry_after: Some(Duration::from_secs(12))
                })
            );
        }
        let decimal_url = url(StatusCode::SERVICE_UNAVAILABLE, Some("1.5"), "{}").await;
        assert_eq!(
            json(sent(&decimal_url).await, NotFoundIs::SourceNotFound).await,
            Err(DocumentError::Unavailable {
                retry_after: Some(Duration::from_millis(1500))
            })
        );
        let url = url(StatusCode::SERVICE_UNAVAILABLE, Some("-2"), "{}").await;
        assert_eq!(
            json(sent(&url).await, NotFoundIs::SourceNotFound).await,
            Err(DocumentError::Unavailable { retry_after: None })
        );
    }

    #[tokio::test]
    async fn other_client_errors_are_resolution_failures() {
        let url = url(StatusCode::UNPROCESSABLE_ENTITY, None, "{}").await;
        assert_eq!(
            json(sent(&url).await, NotFoundIs::SourceNotFound).await,
            Err(DocumentError::ResolutionFailed)
        );
    }

    #[tokio::test]
    async fn malformed_json_is_a_resolution_failure_and_valid_json_is_returned() {
        let malformed = url(StatusCode::OK, None, "not json").await;
        assert_eq!(
            json(sent(&malformed).await, NotFoundIs::SourceNotFound).await,
            Err(DocumentError::ResolutionFailed)
        );
        let valid = url(StatusCode::OK, None, r#"{"ok":true}"#).await;
        assert_eq!(
            json(sent(&valid).await, NotFoundIs::SourceNotFound).await,
            Ok(serde_json::json!({"ok": true}))
        );
    }

    #[test]
    fn field_errors_preserve_the_document_error_class() {
        let field = FieldId::IdentityName;
        let cases = [
            (DocumentError::NotFound, "source_not_found", false),
            (
                DocumentError::Unavailable { retry_after: None },
                "provider_unavailable",
                true,
            ),
            (DocumentError::Unsupported, "resolution_failed", false),
            (DocumentError::ResolutionFailed, "resolution_failed", false),
            (DocumentError::Malformed, "resolution_failed", false),
        ];
        for (error, code, retryable) in cases {
            let result = field_error(field, &error);
            assert_eq!(result.field, field);
            assert_eq!(result.code, code);
            assert_eq!(result.retryable, retryable);
        }
    }
}
