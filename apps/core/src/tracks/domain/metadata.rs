use super::{ValidationError, text::optional};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TrackMetadata {
    title: String,
    artists: Vec<String>,
    duration_ms: Option<u64>,
    album: Option<String>,
    musicbrainz_recording_id: Option<String>,
    isrc: Option<String>,
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
            album: None,
            musicbrainz_recording_id: None,
            isrc: None,
        })
    }

    pub fn with_recording_id(
        mut self,
        recording_id: Option<String>,
    ) -> Result<Self, ValidationError> {
        self.musicbrainz_recording_id = optional("musicbrainz_recording_id", recording_id, 64)?;
        Ok(self)
    }

    pub fn with_isrc(mut self, isrc: Option<String>) -> Result<Self, ValidationError> {
        self.isrc = optional("isrc", isrc, 32)?;
        Ok(self)
    }

    pub fn with_album(mut self, album: Option<String>) -> Result<Self, ValidationError> {
        let album = album.and_then(|album| {
            let album = album.trim();
            (!album.is_empty()).then(|| album.to_owned())
        });

        if let Some(album) = &album
            && album.chars().count() > 500
        {
            return Err(ValidationError::TooLong { field: "album" });
        }

        self.album = album;
        Ok(self)
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

    pub fn album(&self) -> Option<&str> {
        self.album.as_deref()
    }

    pub fn musicbrainz_recording_id(&self) -> Option<&str> {
        self.musicbrainz_recording_id.as_deref()
    }

    pub fn isrc(&self) -> Option<&str> {
        self.isrc.as_deref()
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
        assert_eq!(metadata.album(), None);
    }

    #[test]
    fn metadata_trims_and_bounds_album() {
        let metadata = TrackMetadata::new("Title".to_owned(), vec!["Artist".to_owned()], None)
            .unwrap()
            .with_album(Some("  Discovery  ".to_owned()))
            .unwrap();
        assert_eq!(metadata.album(), Some("Discovery"));

        let blank = metadata.clone().with_album(Some("   ".to_owned())).unwrap();
        assert_eq!(blank.album(), None);

        assert!(matches!(
            metadata.with_album(Some("a".repeat(501))),
            Err(ValidationError::TooLong { field: "album" })
        ));
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
    fn metadata_allows_a_track_with_no_credited_artist() {
        let metadata = TrackMetadata::new("Title".to_owned(), vec![], None).unwrap();
        assert!(metadata.artists().is_empty());
    }

    #[test]
    fn metadata_rejects_blank_artist_values() {
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
