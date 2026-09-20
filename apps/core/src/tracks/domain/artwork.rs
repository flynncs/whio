use super::ValidationError;

/// where one source keeps its cover art.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ArtworkRef(String);

impl ArtworkRef {
    pub fn url(value: String) -> Result<Self, ValidationError> {
        validate(&value, 2048, "artwork_url")?;
        Ok(Self(value))
    }

    pub fn from_stored(reference: String) -> Result<Self, ValidationError> {
        Self::url(reference)
    }

    pub fn reference(&self) -> &str {
        &self.0
    }
}

fn validate(value: &str, max: usize, field: &'static str) -> Result<(), ValidationError> {
    if value.trim().is_empty() {
        return Err(ValidationError::Empty { field });
    }

    if value.chars().count() > max {
        return Err(ValidationError::TooLong { field });
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn artwork_references_round_trip_through_storage() {
        let artwork = ArtworkRef::url("https://art.test/cover.jpg".to_owned()).unwrap();
        let restored = ArtworkRef::from_stored(artwork.reference().to_owned()).unwrap();

        assert_eq!(restored, artwork);
    }

    #[test]
    fn artwork_references_reject_empty_and_too_long_values() {
        assert!(matches!(
            ArtworkRef::url("  ".to_owned()),
            Err(ValidationError::Empty {
                field: "artwork_url"
            })
        ));
        assert!(matches!(
            ArtworkRef::url("a".repeat(2049)),
            Err(ValidationError::TooLong {
                field: "artwork_url"
            })
        ));
    }
}
