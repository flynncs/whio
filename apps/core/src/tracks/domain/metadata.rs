use super::ValidationError;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TrackMetadata {
    title: String,
    artists: Vec<String>,
    duration_ms: Option<u64>,
}

impl TrackMetadata {
    pub fn new(
        title: String,
        artists: Vec<String>,
        duration_ms: Option<u64>,
    ) -> Result<Self, ValidationError> {
        if title.is_empty() {
            return Err(ValidationError::Empty { field: "title" });
        }

        if title.chars().count() > 500 {
            return Err(ValidationError::TooLong { field: "title" });
        }

        if artists.is_empty() {
            return Err(ValidationError::Empty { field: "artists" });
        }

        for artist in &artists {
            if artist.is_empty() {
                return Err(ValidationError::Empty { field: "artist" });
            }

            if artist.chars().count() > 500 {
                return Err(ValidationError::TooLong { field: "artist" });
            }
        }

        Ok(Self {
            title,
            artists,
            duration_ms,
        })
    }

    pub fn title(&self) -> &str {
        &self.title
    }

    pub fn artists(&self) -> &[String] {
        &self.artists
    }

    pub fn duration_ms(&self) -> Option<u64> {
        self.duration_ms
    }

    pub fn into_parts(self) -> (String, Vec<String>, Option<u64>) {
        (self.title, self.artists, self.duration_ms)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metadata_keeps_normalized_values() {
        let metadata = TrackMetadata::new(
            "Instant Crush".to_owned(),
            vec!["Daft Punk".to_owned()],
            Some(337_000),
        )
        .unwrap();

        assert_eq!(metadata.title(), "Instant Crush");
        assert_eq!(metadata.artists(), ["Daft Punk"]);
        assert_eq!(metadata.duration_ms(), Some(337_000));
    }

    #[test]
    fn metadata_rejects_invalid_titles() {
        assert!(matches!(
            TrackMetadata::new(String::new(), vec!["Artist".to_owned()], None),
            Err(ValidationError::Empty { field: "title" })
        ));
        assert!(matches!(
            TrackMetadata::new("a".repeat(501), vec!["Artist".to_owned()], None),
            Err(ValidationError::TooLong { field: "title" })
        ));
    }

    #[test]
    fn metadata_rejects_invalid_artists() {
        assert!(matches!(
            TrackMetadata::new("Title".to_owned(), vec![], None),
            Err(ValidationError::Empty { field: "artists" })
        ));
        assert!(matches!(
            TrackMetadata::new("Title".to_owned(), vec![String::new()], None),
            Err(ValidationError::Empty { field: "artist" })
        ));
        assert!(matches!(
            TrackMetadata::new("Title".to_owned(), vec!["a".repeat(501)], None),
            Err(ValidationError::TooLong { field: "artist" })
        ));
    }
}
