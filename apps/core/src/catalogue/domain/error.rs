use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ValidationError {
    #[error("{field} must not be empty")]
    Empty { field: &'static str },

    #[error("{field} must not be too long")]
    TooLong { field: &'static str },

    #[error("{field} must be between {min} and {max}")]
    OutOfRange {
        field: &'static str,
        min: u32,
        max: u32,
    },

    #[error("provider id has an invalid format")]
    InvalidProviderId,

    #[error("duration_ms must not be negative")]
    InvalidDuration,
}
