use axum::http::HeaderValue;
use thiserror::Error;
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum RequestIdError {
    #[error("request ID must not be empty")]
    Empty,

    #[error("request ID must not exceed 128 characters")]
    TooLong,

    #[error("request ID contains characters that cannot be used in an HTTP header")]
    InvalidHeaderValue,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RequestId(String);

impl RequestId {
    pub fn new(value: String) -> Result<Self, RequestIdError> {
        if value.is_empty() {
            return Err(RequestIdError::Empty);
        }

        if value.chars().count() > 128 {
            return Err(RequestIdError::TooLong);
        }

        HeaderValue::from_str(&value).map_err(|_| RequestIdError::InvalidHeaderValue)?;

        Ok(Self(value))
    }

    pub fn generate() -> Self {
        Self(Uuid::now_v7().to_string())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn into_string(self) -> String {
        self.0
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RequestContext {
    request_id: RequestId,
}

impl RequestContext {
    pub fn new(request_id: RequestId) -> Self {
        Self { request_id }
    }

    pub fn request_id(&self) -> &RequestId {
        &self.request_id
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_id_accepts_contract_boundaries() {
        assert!(RequestId::new("x".to_owned()).is_ok());
        assert!(RequestId::new("x".repeat(128)).is_ok());
    }

    #[test]
    fn request_id_rejects_invalid_values() {
        assert_eq!(RequestId::new(String::new()), Err(RequestIdError::Empty));
        assert_eq!(
            RequestId::new("x".repeat(129)),
            Err(RequestIdError::TooLong)
        );
        assert_eq!(
            RequestId::new("line\nbreak".to_owned()),
            Err(RequestIdError::InvalidHeaderValue)
        );
    }
}
