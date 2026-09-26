use super::{ArtistCredit, PartialDate, ValidationError, text::optional};

const MAX_GENRE: usize = 200;
const MAX_GENRES: usize = 16;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TrackMetadata {
    title: String,
    artists: Vec<ArtistCredit>,
    credits: Vec<ArtistCredit>,
    duration_ms: Option<u64>,
    musicbrainz_recording_id: Option<String>,
    isrc: Option<String>,
    genres: Vec<String>,
    recording_date: Option<PartialDate>,
    original_date: Option<PartialDate>,
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
            credits: Vec::new(),
            duration_ms,
            musicbrainz_recording_id: None,
            isrc: None,
            genres: Vec::new(),
            recording_date: None,
            original_date: None,
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

    pub fn with_genres(mut self, genres: Vec<String>) -> Self {
        self.genres = genres
            .into_iter()
            .filter_map(|genre| optional("genre", Some(genre), MAX_GENRE).ok().flatten())
            .take(MAX_GENRES)
            .collect();
        self
    }

    pub fn with_credits(mut self, credits: Vec<ArtistCredit>) -> Self {
        self.credits = credits;
        self
    }

    pub fn with_recording_date(mut self, recording_date: Option<PartialDate>) -> Self {
        self.recording_date = recording_date;
        self
    }

    pub fn with_original_date(mut self, original_date: Option<PartialDate>) -> Self {
        self.original_date = original_date;
        self
    }

    pub fn title(&self) -> &str {
        &self.title
    }

    pub fn artists(&self) -> &[ArtistCredit] {
        &self.artists
    }

    pub fn credits(&self) -> &[ArtistCredit] {
        &self.credits
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

    pub fn genres(&self) -> &[String] {
        &self.genres
    }

    pub fn primary_genre(&self) -> Option<&str> {
        self.genres.first().map(String::as_str)
    }

    pub fn recording_date(&self) -> Option<PartialDate> {
        self.recording_date
    }

    pub fn original_date(&self) -> Option<PartialDate> {
        self.original_date
    }

    pub fn into_parts(self) -> (String, Vec<ArtistCredit>, Option<u64>) {
        (self.title, self.artists, self.duration_ms)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::features::music_library::domain::CreditRole;

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
    fn metadata_trims_the_genre_set_and_drops_invalid_values() {
        let metadata = TrackMetadata::from_names("Title".to_owned(), vec![], None)
            .unwrap()
            .with_genres(vec![
                "  Rock ".to_owned(),
                "   ".to_owned(),
                "Pop".to_owned(),
                "a".repeat(201),
            ]);

        assert_eq!(metadata.genres(), ["Rock", "Pop"]);
        assert_eq!(metadata.primary_genre(), Some("Rock"));

        let none = TrackMetadata::from_names("Title".to_owned(), vec![], None).unwrap();
        assert!(none.genres().is_empty());
        assert_eq!(none.primary_genre(), None);
    }

    #[test]
    fn metadata_keeps_the_first_sixteen_genres() {
        let genres = (0..20).map(|index| format!("genre-{index}")).collect();
        let metadata = TrackMetadata::from_names("Title".to_owned(), vec![], None)
            .unwrap()
            .with_genres(genres);

        assert_eq!(metadata.genres().len(), 16);
        assert_eq!(metadata.genres()[0], "genre-0");
        assert_eq!(metadata.genres()[15], "genre-15");
    }

    #[test]
    fn metadata_keeps_role_credits_apart_from_the_primary_artists() {
        let metadata = TrackMetadata::from_names("Title".to_owned(), vec!["A".to_owned()], None)
            .unwrap()
            .with_credits(vec![
                ArtistCredit::new("B".to_owned(), None)
                    .unwrap()
                    .with_role(CreditRole::Composer),
            ]);

        assert_eq!(metadata.artists().len(), 1);
        assert_eq!(metadata.artists()[0].name(), "A");
        assert_eq!(metadata.credits().len(), 1);
        assert_eq!(metadata.credits()[0].name(), "B");
        assert_eq!(metadata.credits()[0].role(), Some(CreditRole::Composer));
    }

    #[test]
    fn metadata_records_each_date_with_its_own_precision() {
        let recorded = PartialDate::parse("1987").unwrap();
        let original = PartialDate::parse("1979-05-02").unwrap();
        let metadata = TrackMetadata::from_names("Title".to_owned(), vec![], None)
            .unwrap()
            .with_recording_date(Some(recorded))
            .with_original_date(Some(original));

        assert_eq!(metadata.recording_date().unwrap().as_str(), "1987");
        assert_eq!(metadata.original_date().unwrap().as_str(), "1979-05-02");
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
