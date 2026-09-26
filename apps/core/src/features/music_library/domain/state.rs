use super::{
    AlbumId, AlbumSummary, ArtistId, ArtistSummary, TrackHit, TrackId, ValidationError,
    text::required,
};

pub const MIN_RATING: u8 = 1;
pub const MAX_RATING: u8 = 5;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct LibraryState {
    starred_at: Option<chrono::DateTime<chrono::Utc>>,
    rating: Option<u8>,
    play_count: u32,
    last_played_at: Option<chrono::DateTime<chrono::Utc>>,
}

impl LibraryState {
    pub fn new(
        starred_at: Option<chrono::DateTime<chrono::Utc>>,
        rating: Option<u8>,
        play_count: u32,
        last_played_at: Option<chrono::DateTime<chrono::Utc>>,
    ) -> Result<Self, ValidationError> {
        Ok(Self {
            starred_at,
            rating: checked_rating(rating)?,
            play_count,
            last_played_at,
        })
    }

    pub fn starred_at(&self) -> Option<chrono::DateTime<chrono::Utc>> {
        self.starred_at
    }

    pub fn is_starred(&self) -> bool {
        self.starred_at.is_some()
    }

    pub fn rating(&self) -> Option<u8> {
        self.rating
    }

    pub fn play_count(&self) -> u32 {
        self.play_count
    }

    pub fn last_played_at(&self) -> Option<chrono::DateTime<chrono::Utc>> {
        self.last_played_at
    }
}

pub fn checked_rating(rating: Option<u8>) -> Result<Option<u8>, ValidationError> {
    match rating {
        None | Some(0) => Ok(None),
        Some(rating) if (MIN_RATING..=MAX_RATING).contains(&rating) => Ok(Some(rating)),
        Some(_) => Err(ValidationError::OutOfRange {
            field: "rating",
            min: i64::from(MIN_RATING),
            max: i64::from(MAX_RATING),
        }),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StateTarget<'a> {
    Track(&'a TrackId),
    Album(&'a AlbumId),
    Artist(&'a ArtistId),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GenreSummary {
    genre: String,
    song_count: u32,
    album_count: u32,
}

impl GenreSummary {
    pub fn new(genre: String, song_count: u32, album_count: u32) -> Result<Self, ValidationError> {
        Ok(Self {
            genre: required("genre", genre, 200)?,
            song_count,
            album_count,
        })
    }

    pub fn genre(&self) -> &str {
        &self.genre
    }

    pub fn song_count(&self) -> u32 {
        self.song_count
    }

    pub fn album_count(&self) -> u32 {
        self.album_count
    }
}

#[derive(Debug, Clone)]
pub struct Starred {
    tracks: Vec<TrackHit>,
    albums: Vec<AlbumSummary>,
    artists: Vec<ArtistSummary>,
}

impl Starred {
    pub fn new(
        tracks: Vec<TrackHit>,
        albums: Vec<AlbumSummary>,
        artists: Vec<ArtistSummary>,
    ) -> Self {
        Self {
            tracks,
            albums,
            artists,
        }
    }

    pub fn tracks(&self) -> &[TrackHit] {
        &self.tracks
    }

    pub fn albums(&self) -> &[AlbumSummary] {
        &self.albums
    }

    pub fn artists(&self) -> &[ArtistSummary] {
        &self.artists
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rating_zero_clears_and_outliers_are_rejected() {
        assert_eq!(checked_rating(None).unwrap(), None);
        assert_eq!(checked_rating(Some(0)).unwrap(), None);
        assert_eq!(checked_rating(Some(5)).unwrap(), Some(5));
        assert!(matches!(
            checked_rating(Some(6)),
            Err(ValidationError::OutOfRange {
                field: "rating",
                ..
            })
        ));
    }

    #[test]
    fn library_state_reports_what_it_holds() {
        let at = chrono::Utc::now();
        let state = LibraryState::new(Some(at), Some(4), 7, Some(at)).unwrap();

        assert!(state.is_starred());
        assert_eq!(state.starred_at(), Some(at));
        assert_eq!(state.rating(), Some(4));
        assert_eq!(state.play_count(), 7);
        assert_eq!(state.last_played_at(), Some(at));

        let empty = LibraryState::default();
        assert!(!empty.is_starred());
        assert_eq!(empty.play_count(), 0);
    }

    #[test]
    fn genre_summary_trims_and_bounds() {
        let genre = GenreSummary::new("  Rock ".to_owned(), 3, 2).unwrap();
        assert_eq!(genre.genre(), "Rock");
        assert_eq!(genre.song_count(), 3);
        assert_eq!(genre.album_count(), 2);
        assert!(matches!(
            GenreSummary::new("   ".to_owned(), 0, 0),
            Err(ValidationError::Empty { field: "genre" })
        ));
    }
}
