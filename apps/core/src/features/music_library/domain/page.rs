use chrono::{DateTime, Utc};

use super::{
    AlbumId, ArtistId, FullTextSelector, IdSet, RandomSeed, Range, RoleSet, SelectorText,
    SortDirection, TrackView, ValidationError,
};
use crate::features::music_library::library::LibraryId;

pub const MAX_PAGE_LIMIT: u32 = 500;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PageLimit(u16);

impl PageLimit {
    pub fn new(value: u32) -> Result<Self, ValidationError> {
        let out_of_range = || ValidationError::OutOfRange {
            field: "limit",
            min: 1,
            max: i64::from(MAX_PAGE_LIMIT),
        };

        if !(1..=MAX_PAGE_LIMIT).contains(&value) {
            return Err(out_of_range());
        }

        let value = u16::try_from(value).map_err(|_| out_of_range())?;

        Ok(Self(value))
    }

    pub fn get(self) -> u32 {
        u32::from(self.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum TrackOrder {
    #[default]
    Title,
    Id,
    Album,
    Track,
    Artist,
    AlbumArtist,
    Duration,
    Genre,
    Year,
    ReleaseYear,
    DateAdded,
    StarredAt,
    Rating,
    PlayCount,
    PlayDate,
    Channels,
    Random,
    RecentlyAdded,
}

impl TrackOrder {
    pub const ALL: [TrackOrder; 18] = [
        TrackOrder::Title,
        TrackOrder::Id,
        TrackOrder::Album,
        TrackOrder::Track,
        TrackOrder::Artist,
        TrackOrder::AlbumArtist,
        TrackOrder::Duration,
        TrackOrder::Genre,
        TrackOrder::Year,
        TrackOrder::ReleaseYear,
        TrackOrder::DateAdded,
        TrackOrder::StarredAt,
        TrackOrder::Rating,
        TrackOrder::PlayCount,
        TrackOrder::PlayDate,
        TrackOrder::Channels,
        TrackOrder::Random,
        TrackOrder::RecentlyAdded,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Title => "title",
            Self::Id => "id",
            Self::Album => "album",
            Self::Track => "track",
            Self::Artist => "artist",
            Self::AlbumArtist => "album_artist",
            Self::Duration => "duration",
            Self::Genre => "genre",
            Self::Year => "year",
            Self::ReleaseYear => "release_year",
            Self::DateAdded => "date_added",
            Self::StarredAt => "starred_at",
            Self::Rating => "rating",
            Self::PlayCount => "play_count",
            Self::PlayDate => "play_date",
            Self::Channels => "channels",
            Self::Random => "random",
            Self::RecentlyAdded => "recently_added",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|order| order.as_str() == value)
    }

    pub fn default_direction(self) -> SortDirection {
        match self {
            Self::DateAdded
            | Self::StarredAt
            | Self::Rating
            | Self::PlayCount
            | Self::PlayDate
            | Self::RecentlyAdded => SortDirection::Descending,
            _ => SortDirection::Ascending,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum TrackListScope {
    #[default]
    Engaged,
    Owned,
}

impl TrackListScope {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Engaged => "engaged",
            Self::Owned => "owned",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "engaged" => Some(Self::Engaged),
            "owned" => Some(Self::Owned),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TrackListFilter {
    scope: TrackListScope,
    title: Option<FullTextSelector>,
    provider_discovered: bool,
    album_id: Option<IdSet<AlbumId>>,
    album_artist_id: Option<IdSet<ArtistId>>,
    artists_id: Option<IdSet<ArtistId>>,
    role: Option<RoleSet>,
    genre: Option<IdSet<SelectorText>>,
    year: Option<Range<i32>>,
    compilation: Option<bool>,
    starred: Option<bool>,
    has_rating: Option<bool>,
    rating: Option<Range<u8>>,
    play_count: Option<Range<u32>>,
    recently_played: Option<bool>,
    date_added: Option<Range<DateTime<Utc>>>,
    missing: Option<bool>,
    path: Option<SelectorText>,
    duration: Option<Range<u64>>,
    track_number: Option<Range<u16>>,
    disc_number: Option<Range<u16>>,
    disc_total: Option<Range<u16>>,
    track_total: Option<Range<u16>>,
    bitrate: Option<Range<i64>>,
    bitdepth: Option<Range<i64>>,
    samplerate: Option<Range<i64>>,
    size: Option<Range<i64>>,
    channels: Option<Range<u16>>,
    codec: Option<SelectorText>,
    filetype: Option<SelectorText>,
    library_id: Option<IdSet<LibraryId>>,
    sort_direction: Option<SortDirection>,
    random_seed: Option<RandomSeed>,
}

impl TrackListFilter {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_scope(mut self, scope: TrackListScope) -> Self {
        self.scope = scope;
        self
    }

    pub fn with_title(mut self, title: Option<FullTextSelector>) -> Self {
        self.title = title;
        self
    }

    pub fn with_provider_discovered(mut self, provider_discovered: bool) -> Self {
        self.provider_discovered = provider_discovered;
        self
    }

    pub fn with_album_id(mut self, album_id: Option<IdSet<AlbumId>>) -> Self {
        self.album_id = album_id;
        self
    }

    pub fn with_album_artist_id(mut self, album_artist_id: Option<IdSet<ArtistId>>) -> Self {
        self.album_artist_id = album_artist_id;
        self
    }

    pub fn with_artists_id(mut self, artists_id: Option<IdSet<ArtistId>>) -> Self {
        self.artists_id = artists_id;
        self
    }

    pub fn with_role(mut self, role: Option<RoleSet>) -> Self {
        self.role = role;
        self
    }

    pub fn with_genre(mut self, genre: Option<IdSet<SelectorText>>) -> Self {
        self.genre = genre;
        self
    }

    pub fn with_year(mut self, year: Option<Range<i32>>) -> Self {
        self.year = year;
        self
    }

    pub fn with_compilation(mut self, compilation: Option<bool>) -> Self {
        self.compilation = compilation;
        self
    }

    pub fn with_starred(mut self, starred: Option<bool>) -> Self {
        self.starred = starred;
        self
    }

    pub fn with_has_rating(mut self, has_rating: Option<bool>) -> Self {
        self.has_rating = has_rating;
        self
    }

    pub fn with_rating(mut self, rating: Option<Range<u8>>) -> Self {
        self.rating = rating;
        self
    }

    pub fn with_play_count(mut self, play_count: Option<Range<u32>>) -> Self {
        self.play_count = play_count;
        self
    }

    pub fn with_recently_played(mut self, recently_played: Option<bool>) -> Self {
        self.recently_played = recently_played;
        self
    }

    pub fn with_date_added(mut self, date_added: Option<Range<DateTime<Utc>>>) -> Self {
        self.date_added = date_added;
        self
    }

    pub fn with_missing(mut self, missing: Option<bool>) -> Self {
        self.missing = missing;
        self
    }

    pub fn with_path(mut self, path: Option<SelectorText>) -> Self {
        self.path = path;
        self
    }

    pub fn with_duration(mut self, duration: Option<Range<u64>>) -> Self {
        self.duration = duration;
        self
    }

    pub fn with_track_number(mut self, track_number: Option<Range<u16>>) -> Self {
        self.track_number = track_number;
        self
    }

    pub fn with_disc_number(mut self, disc_number: Option<Range<u16>>) -> Self {
        self.disc_number = disc_number;
        self
    }

    pub fn with_disc_total(mut self, disc_total: Option<Range<u16>>) -> Self {
        self.disc_total = disc_total;
        self
    }

    pub fn with_track_total(mut self, track_total: Option<Range<u16>>) -> Self {
        self.track_total = track_total;
        self
    }

    pub fn with_bitrate(mut self, bitrate: Option<Range<i64>>) -> Self {
        self.bitrate = bitrate;
        self
    }

    pub fn with_bitdepth(mut self, bitdepth: Option<Range<i64>>) -> Self {
        self.bitdepth = bitdepth;
        self
    }

    pub fn with_samplerate(mut self, samplerate: Option<Range<i64>>) -> Self {
        self.samplerate = samplerate;
        self
    }

    pub fn with_size(mut self, size: Option<Range<i64>>) -> Self {
        self.size = size;
        self
    }

    pub fn with_channels(mut self, channels: Option<Range<u16>>) -> Self {
        self.channels = channels;
        self
    }

    pub fn with_codec(mut self, codec: Option<SelectorText>) -> Self {
        self.codec = codec;
        self
    }

    pub fn with_filetype(mut self, filetype: Option<SelectorText>) -> Self {
        self.filetype = filetype;
        self
    }

    pub fn with_library_id(mut self, library_id: Option<IdSet<LibraryId>>) -> Self {
        self.library_id = library_id;
        self
    }

    pub fn with_sort_direction(mut self, sort_direction: Option<SortDirection>) -> Self {
        self.sort_direction = sort_direction;
        self
    }

    pub fn with_random_seed(mut self, random_seed: Option<RandomSeed>) -> Self {
        self.random_seed = random_seed;
        self
    }

    pub fn scope(&self) -> TrackListScope {
        self.scope
    }

    pub fn title(&self) -> Option<&FullTextSelector> {
        self.title.as_ref()
    }

    pub fn provider_discovered(&self) -> bool {
        self.provider_discovered
    }

    pub fn album_id(&self) -> Option<&IdSet<AlbumId>> {
        self.album_id.as_ref()
    }

    pub fn album_artist_id(&self) -> Option<&IdSet<ArtistId>> {
        self.album_artist_id.as_ref()
    }

    pub fn artists_id(&self) -> Option<&IdSet<ArtistId>> {
        self.artists_id.as_ref()
    }

    pub fn role(&self) -> Option<&RoleSet> {
        self.role.as_ref()
    }

    pub fn genre(&self) -> Option<&IdSet<SelectorText>> {
        self.genre.as_ref()
    }

    pub fn year(&self) -> Option<Range<i32>> {
        self.year
    }

    pub fn compilation(&self) -> Option<bool> {
        self.compilation
    }

    pub fn starred(&self) -> Option<bool> {
        self.starred
    }

    pub fn has_rating(&self) -> Option<bool> {
        self.has_rating
    }

    pub fn rating(&self) -> Option<Range<u8>> {
        self.rating
    }

    pub fn play_count(&self) -> Option<Range<u32>> {
        self.play_count
    }

    pub fn recently_played(&self) -> Option<bool> {
        self.recently_played
    }

    pub fn date_added(&self) -> Option<Range<DateTime<Utc>>> {
        self.date_added
    }

    pub fn missing(&self) -> Option<bool> {
        self.missing
    }

    pub fn path(&self) -> Option<&SelectorText> {
        self.path.as_ref()
    }

    pub fn duration(&self) -> Option<Range<u64>> {
        self.duration
    }

    pub fn track_number(&self) -> Option<Range<u16>> {
        self.track_number
    }

    pub fn disc_number(&self) -> Option<Range<u16>> {
        self.disc_number
    }

    pub fn disc_total(&self) -> Option<Range<u16>> {
        self.disc_total
    }

    pub fn track_total(&self) -> Option<Range<u16>> {
        self.track_total
    }

    pub fn bitrate(&self) -> Option<Range<i64>> {
        self.bitrate
    }

    pub fn bitdepth(&self) -> Option<Range<i64>> {
        self.bitdepth
    }

    pub fn samplerate(&self) -> Option<Range<i64>> {
        self.samplerate
    }

    pub fn size(&self) -> Option<Range<i64>> {
        self.size
    }

    pub fn channels(&self) -> Option<Range<u16>> {
        self.channels
    }

    pub fn codec(&self) -> Option<&SelectorText> {
        self.codec.as_ref()
    }

    pub fn filetype(&self) -> Option<&SelectorText> {
        self.filetype.as_ref()
    }

    pub fn library_id(&self) -> Option<&IdSet<LibraryId>> {
        self.library_id.as_ref()
    }

    pub fn sort_direction(&self) -> Option<SortDirection> {
        self.sort_direction
    }

    pub fn random_seed(&self) -> Option<RandomSeed> {
        self.random_seed
    }

    pub fn resolved_direction(&self, order: TrackOrder) -> SortDirection {
        self.sort_direction
            .unwrap_or_else(|| order.default_direction())
    }

    pub fn resolved_seed(&self, order: TrackOrder) -> Result<Option<RandomSeed>, ValidationError> {
        match (order, self.random_seed) {
            (TrackOrder::Random, seed) => Ok(seed),
            (_, Some(_)) => Err(ValidationError::Invalid {
                field: "random_seed",
            }),
            (_, None) => Ok(None),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrackPage {
    items: Vec<TrackView>,
    total: u32,
}

impl TrackPage {
    pub(crate) fn new(items: Vec<TrackView>, total: u32) -> Self {
        Self { items, total }
    }

    pub fn items(&self) -> &[TrackView] {
        &self.items
    }

    pub fn total(&self) -> u32 {
        self.total
    }

    pub fn into_parts(self) -> (Vec<TrackView>, u32) {
        (self.items, self.total)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn page_limit_rejects_zero_and_above_max() {
        assert_eq!(PageLimit::new(1).unwrap().get(), 1);
        assert_eq!(
            PageLimit::new(MAX_PAGE_LIMIT).unwrap().get(),
            MAX_PAGE_LIMIT
        );

        for value in [0, MAX_PAGE_LIMIT + 1] {
            assert!(
                matches!(
                    PageLimit::new(value),
                    Err(ValidationError::OutOfRange {
                        field: "limit",
                        min: 1,
                        max: 500,
                    })
                ),
                "limit {value} is rejected"
            );
        }
    }

    #[test]
    fn order_names_parse_exactly_and_never_fall_back() {
        for order in TrackOrder::ALL {
            assert_eq!(TrackOrder::parse(order.as_str()), Some(order));
        }

        assert_eq!(TrackOrder::ALL.len(), 18);
        assert_eq!(TrackOrder::parse("bpm"), None);
        assert_eq!(TrackOrder::parse("comment"), None);
        assert_eq!(TrackOrder::parse("explicitStatus"), None);
        assert_eq!(TrackOrder::parse("Title"), None);
        assert_eq!(TrackOrder::parse("order_album_artist_name"), None);
        assert_eq!(TrackOrder::parse("releaseYear"), None);
        assert_eq!(TrackOrder::parse("createdAt"), None);
        assert_eq!(TrackOrder::parse(""), None);
    }

    #[test]
    fn every_order_has_its_natural_direction() {
        for order in [
            TrackOrder::DateAdded,
            TrackOrder::StarredAt,
            TrackOrder::Rating,
            TrackOrder::PlayCount,
            TrackOrder::PlayDate,
            TrackOrder::RecentlyAdded,
        ] {
            assert_eq!(
                order.default_direction(),
                SortDirection::Descending,
                "{order:?}"
            );
        }

        for order in [TrackOrder::Title, TrackOrder::Album, TrackOrder::Year] {
            assert_eq!(
                order.default_direction(),
                SortDirection::Ascending,
                "{order:?}"
            );
        }
    }

    #[test]
    fn the_filter_defaults_to_the_engaged_scope_natural_direction_and_no_seed() {
        let filter = TrackListFilter::new();
        assert_eq!(filter.scope(), TrackListScope::Engaged);
        assert_eq!(filter.sort_direction(), None);
        assert_eq!(filter.random_seed(), None);
        assert_eq!(
            filter.resolved_direction(TrackOrder::Rating),
            SortDirection::Descending
        );
        assert_eq!(
            filter.resolved_direction(TrackOrder::Title),
            SortDirection::Ascending
        );
        assert_eq!(
            filter
                .clone()
                .with_sort_direction(Some(SortDirection::Descending))
                .resolved_direction(TrackOrder::Title),
            SortDirection::Descending
        );
        assert_eq!(filter.resolved_seed(TrackOrder::Title).unwrap(), None);
    }

    #[test]
    fn a_seed_with_a_non_random_order_is_rejected() {
        let filter = TrackListFilter::new().with_random_seed(Some(RandomSeed::new(7)));

        assert!(matches!(
            filter.resolved_seed(TrackOrder::Title),
            Err(ValidationError::Invalid {
                field: "random_seed"
            })
        ));
        assert_eq!(
            filter.resolved_seed(TrackOrder::Random).unwrap(),
            Some(RandomSeed::new(7))
        );
        assert_eq!(
            TrackListFilter::new()
                .resolved_seed(TrackOrder::Random)
                .unwrap(),
            None
        );
    }

    #[test]
    fn selectors_are_absent_until_set() {
        let filter = TrackListFilter::new();
        assert!(filter.title().is_none());
        assert!(filter.album_id().is_none());
        assert!(filter.album_artist_id().is_none());
        assert!(filter.artists_id().is_none());
        assert!(filter.role().is_none());
        assert!(filter.genre().is_none());
        assert!(filter.year().is_none());
        assert!(filter.compilation().is_none());
        assert!(filter.starred().is_none());
        assert!(filter.has_rating().is_none());
        assert!(filter.rating().is_none());
        assert!(filter.play_count().is_none());
        assert!(filter.recently_played().is_none());
        assert!(filter.date_added().is_none());
        assert!(filter.missing().is_none());
        assert!(filter.path().is_none());
        assert!(filter.duration().is_none());
        assert!(filter.track_number().is_none());
        assert!(filter.disc_number().is_none());
        assert!(filter.disc_total().is_none());
        assert!(filter.track_total().is_none());
        assert!(filter.bitrate().is_none());
        assert!(filter.bitdepth().is_none());
        assert!(filter.samplerate().is_none());
        assert!(filter.size().is_none());
        assert!(filter.channels().is_none());
        assert!(filter.codec().is_none());
        assert!(filter.filetype().is_none());
        assert!(filter.library_id().is_none());
    }

    #[test]
    fn the_text_and_id_selectors_keep_their_typed_values() {
        let title = FullTextSelector::new("  Discovery ").unwrap();
        let genre = IdSet::new([
            SelectorText::new("Rock").unwrap().unwrap(),
            SelectorText::new("rock").unwrap().unwrap(),
        ])
        .unwrap();
        let filter = TrackListFilter::new()
            .with_title(title)
            .with_genre(Some(genre))
            .with_compilation(Some(false))
            .with_missing(Some(true));

        assert_eq!(
            filter.title().map(FullTextSelector::terms),
            Some(["discovery".to_owned()].as_slice())
        );
        assert_eq!(filter.genre().map(IdSet::len), Some(1));
        assert_eq!(filter.compilation(), Some(false));
        assert_eq!(filter.missing(), Some(true));
    }
}
