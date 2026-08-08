use std::{matches, str::FromStr};

use super::ValidationError;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ProviderId(String);

impl ProviderId {
    pub fn new(value: String) -> Result<Self, ValidationError> {
        if value.is_empty() {
            return Err(ValidationError::Empty {
                field: "provider_id",
            });
        }

        if value.chars().count() > 64 {
            return Err(ValidationError::TooLong {
                field: "provider_id",
            });
        }

        let mut chars = value.chars();

        if !chars
            .next()
            .is_some_and(|character| character.is_ascii_lowercase())
        {
            return Err(ValidationError::InvalidProviderId);
        }

        let mut was_previous_char_separator = false;

        for character in chars {
            if character.is_ascii_lowercase() || character.is_ascii_digit() {
                was_previous_char_separator = false;
            } else if matches!(character, '.' | '_' | '-') {
                if was_previous_char_separator {
                    return Err(ValidationError::InvalidProviderId);
                }
                was_previous_char_separator = true;
            } else {
                return Err(ValidationError::InvalidProviderId);
            }
        }

        if was_previous_char_separator {
            return Err(ValidationError::InvalidProviderId);
        }

        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl FromStr for ProviderId {
    type Err = ValidationError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::new(value.to_owned())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SourceIdentity {
    provider_id: ProviderId,
    external_id: String,
}

impl SourceIdentity {
    pub fn new(provider_id: ProviderId, external_id: String) -> Result<Self, ValidationError> {
        if external_id.is_empty() {
            return Err(ValidationError::Empty {
                field: "external_id",
            });
        }

        if external_id.chars().count() > 512 {
            return Err(ValidationError::TooLong {
                field: "external_id",
            });
        }

        Ok(Self {
            provider_id,
            external_id,
        })
    }

    pub fn provider_id(&self) -> &ProviderId {
        &self.provider_id
    }

    pub fn external_id(&self) -> &str {
        &self.external_id
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_id_accepts_contract_identifiers() {
        for value in ["youtube_music", "catalogue.search", "provider-2"] {
            assert!(ProviderId::new(value.to_owned()).is_ok(), "{value}");
        }
    }

    #[test]
    fn provider_id_rejects_invalid_identifiers() {
        for value in ["", "YouTube", "_leading", "trailing-", "two..dots", "a/b"] {
            assert!(ProviderId::new(value.to_owned()).is_err(), "{value}");
        }

        assert!(matches!(
            ProviderId::new("a".repeat(65)),
            Err(ValidationError::TooLong {
                field: "provider_id"
            })
        ));
    }

    #[test]
    fn source_identity_validates_external_id() {
        let provider_id = || ProviderId::new("youtube_music".to_owned()).unwrap();

        assert!(matches!(
            SourceIdentity::new(provider_id(), String::new()),
            Err(ValidationError::Empty {
                field: "external_id"
            })
        ));

        assert!(matches!(
            SourceIdentity::new(provider_id(), "x".repeat(513)),
            Err(ValidationError::TooLong {
                field: "external_id"
            })
        ));
    }
}
