use super::{ArtistCredit, ValidationError, text::optional};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TrackMetadata {
    title: String,
    artists: Vec<ArtistCredit>,
    duration_ms: Option<u64>,
    musicbrainz_recording_id: Option<String>,
    isrc: Option<String>,
    genre: Option<String>,
}

impl TrackMetadata {
    pub fn new(
        title: String,
        artists: Vec<ArtistCredit>,
        duration_ms: Option<u64>,
    ) -> Result<Self, ValidationError> {
        if title.is_empty() {
            return Err(ValidationError::Empty { field: "title" });
        }

        if title.chars().count() > 500 {
            return Err(ValidationError::TooLong { field: "title" });
        }

        Ok(Self {
            title,
            artists,
            duration_ms,
            musicbrainz_recording_id: None,
            isrc: None,
            genre: None,
        })
    }

    pub fn from_names(
        title: String,
        artists: Vec<String>,
        duration_ms: Option<u64>,
    ) -> Result<Self, ValidationError> {
        let artists = artists
            .into_iter()
            .map(|name| ArtistCredit::new(name, None))
            .collect::<Result<Vec<_>, _>>()?;

        Self::new(title, artists, duration_ms)
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

    pub fn with_genre(mut self, genre: Option<String>) -> Result<Self, ValidationError> {
        self.genre = optional("genre", genre, 200)?;
        Ok(self)
    }

    pub fn title(&self) -> &str {
        &self.title
    }

    pub fn artists(&self) -> &[ArtistCredit] {
        &self.artists
    }

    pub fn duration_ms(&self) -> Option<u64> {
        self.duration_ms
    }

    pub fn musicbrainz_recording_id(&self) -> Option<&str> {
        self.musicbrainz_recording_id.as_deref()
    }

    pub fn isrc(&self) -> Option<&str> {
        self.isrc.as_deref()
    }

    pub fn genre(&self) -> Option<&str> {
        self.genre.as_deref()
    }

    pub fn into_parts(self) -> (String, Vec<ArtistCredit>, Option<u64>) {
        (self.title, self.artists, self.duration_ms)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metadata_keeps_normalized_values() {
        let metadata = TrackMetadata::from_names(
            "Instant Crush".to_owned(),
            vec!["Daft Punk".to_owned()],
            Some(337_000),
        )
        .unwrap();

        assert_eq!(metadata.title(), "Instant Crush");
        assert_eq!(
            metadata
                .artists()
                .iter()
                .map(|artist| artist.name())
                .collect::<Vec<_>>(),
            ["Daft Punk"]
        );
        assert_eq!(metadata.duration_ms(), Some(337_000));
    }

    #[test]
    fn metadata_rejects_invalid_titles() {
        assert!(matches!(
            TrackMetadata::from_names(String::new(), vec!["Artist".to_owned()], None),
            Err(ValidationError::Empty { field: "title" })
        ));
        assert!(matches!(
            TrackMetadata::from_names("a".repeat(501), vec!["Artist".to_owned()], None),
            Err(ValidationError::TooLong { field: "title" })
        ));
    }

    #[test]
    fn metadata_keeps_a_trimmed_genre_and_drops_a_blank_one() {
        let metadata = TrackMetadata::from_names("Title".to_owned(), vec![], None)
            .unwrap()
            .with_genre(Some("  Rock ".to_owned()))
            .unwrap();
        assert_eq!(metadata.genre(), Some("Rock"));

        let blank = TrackMetadata::from_names("Title".to_owned(), vec![], None)
            .unwrap()
            .with_genre(Some("   ".to_owned()))
            .unwrap();
        assert_eq!(blank.genre(), None);
    }

    #[test]
    fn metadata_allows_a_track_with_no_credited_artist() {
        let metadata = TrackMetadata::from_names("Title".to_owned(), vec![], None).unwrap();
        assert!(metadata.artists().is_empty());
    }

    #[test]
    fn metadata_rejects_blank_artist_values() {
        assert!(matches!(
            TrackMetadata::from_names("Title".to_owned(), vec![String::new()], None),
            Err(ValidationError::Empty { field: "artist" })
        ));
        assert!(matches!(
            TrackMetadata::from_names("Title".to_owned(), vec!["a".repeat(501)], None),
            Err(ValidationError::TooLong { field: "artist" })
        ));
    }
}
