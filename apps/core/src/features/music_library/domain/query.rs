use std::hash::Hash;

use chrono::Datelike;
use uuid::Uuid;

use super::{CreditRole, ValidationError};

pub const MAX_SELECTOR_TEXT: usize = 500;
pub const MAX_SELECTOR_IDS: usize = 100;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SelectorText(String);

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct FullTextSelector {
    query: String,
    terms: Vec<String>,
}

impl FullTextSelector {
    pub fn new(value: &str) -> Result<Option<Self>, ValidationError> {
        let normalized = super::text::normalize_for_search(value);
        let terms = normalized
            .split_whitespace()
            .map(str::to_owned)
            .collect::<Vec<_>>();

        if terms.is_empty() {
            return Ok(None);
        }

        if value.trim().chars().count() > MAX_SELECTOR_TEXT {
            return Err(ValidationError::TooLong { field: "selector" });
        }

        Ok(Some(Self {
            query: value.to_owned(),
            terms,
        }))
    }

    pub fn query(&self) -> &str {
        &self.query
    }

    pub fn terms(&self) -> &[String] {
        &self.terms
    }
}

impl SelectorText {
    pub fn new(value: &str) -> Result<Option<Self>, ValidationError> {
        let matches = super::text::normalize_for_matching(value);

        if matches.is_empty() {
            return Ok(None);
        }

        if value.trim().chars().count() > MAX_SELECTOR_TEXT {
            return Err(ValidationError::TooLong { field: "selector" });
        }

        Ok(Some(Self(matches)))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdSet<Id>(Vec<Id>);

impl<Id: Clone + Eq + Hash> IdSet<Id> {
    pub fn new(values: impl IntoIterator<Item = Id>) -> Result<Self, ValidationError> {
        let mut selected: Vec<Id> = Vec::new();

        for value in values {
            if !selected.contains(&value) {
                selected.push(value);
            }
        }

        if selected.is_empty() {
            return Err(ValidationError::Empty { field: "ids" });
        }

        if selected.len() > MAX_SELECTOR_IDS {
            return Err(ValidationError::TooLong { field: "ids" });
        }

        Ok(Self(selected))
    }

    pub fn iter(&self) -> impl Iterator<Item = &Id> {
        self.0.iter()
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Range<T> {
    from: Option<T>,
    to: Option<T>,
}

impl<T: Ord + Copy> Range<T> {
    pub fn new(from: Option<T>, to: Option<T>) -> Result<Self, ValidationError> {
        if from.is_none() && to.is_none() {
            return Err(ValidationError::Invalid { field: "range" });
        }

        if let (Some(from), Some(to)) = (from, to)
            && from > to
        {
            return Err(ValidationError::Invalid { field: "range" });
        }

        Ok(Self { from, to })
    }

    pub fn from(&self) -> Option<T> {
        self.from
    }

    pub fn to(&self) -> Option<T> {
        self.to
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum SortDirection {
    #[default]
    Ascending,
    Descending,
}

impl SortDirection {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ascending => "asc",
            Self::Descending => "desc",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "asc" => Some(Self::Ascending),
            "desc" => Some(Self::Descending),
            _ => None,
        }
    }

    pub fn sql(self) -> &'static str {
        match self {
            Self::Ascending => "ASC",
            Self::Descending => "DESC",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RoleSelector {
    AlbumArtist,
    Artist,
    Arranger,
    Composer,
    Conductor,
    Djmixer,
    Director,
    Engineer,
    Lyricist,
    Mixer,
    Performer,
    Producer,
    Remixer,
}

impl RoleSelector {
    pub const ALL: [RoleSelector; 13] = [
        RoleSelector::AlbumArtist,
        RoleSelector::Artist,
        RoleSelector::Arranger,
        RoleSelector::Composer,
        RoleSelector::Conductor,
        RoleSelector::Djmixer,
        RoleSelector::Director,
        RoleSelector::Engineer,
        RoleSelector::Lyricist,
        RoleSelector::Mixer,
        RoleSelector::Performer,
        RoleSelector::Producer,
        RoleSelector::Remixer,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::AlbumArtist => "albumartist",
            Self::Artist => "artist",
            Self::Arranger => "arranger",
            Self::Composer => "composer",
            Self::Conductor => "conductor",
            Self::Djmixer => "djmixer",
            Self::Director => "director",
            Self::Engineer => "engineer",
            Self::Lyricist => "lyricist",
            Self::Mixer => "mixer",
            Self::Performer => "performer",
            Self::Producer => "producer",
            Self::Remixer => "remixer",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|role| role.as_str() == value)
    }

    pub fn stored_role(self) -> Option<CreditRole> {
        match self {
            Self::AlbumArtist | Self::Artist => None,
            Self::Arranger => Some(CreditRole::Arranger),
            Self::Composer => Some(CreditRole::Composer),
            Self::Conductor => Some(CreditRole::Conductor),
            Self::Djmixer => Some(CreditRole::Djmixer),
            Self::Director => Some(CreditRole::Director),
            Self::Engineer => Some(CreditRole::Engineer),
            Self::Lyricist => Some(CreditRole::Lyricist),
            Self::Mixer => Some(CreditRole::Mixer),
            Self::Performer => Some(CreditRole::Performer),
            Self::Producer => Some(CreditRole::Producer),
            Self::Remixer => Some(CreditRole::Remixer),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoleSet(IdSet<RoleSelector>);

impl RoleSet {
    pub fn new(values: impl IntoIterator<Item = RoleSelector>) -> Result<Self, ValidationError> {
        IdSet::new(values).map(Self)
    }

    pub fn iter(&self) -> impl Iterator<Item = &RoleSelector> {
        self.0.iter()
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Resource {
    Track,
    Album,
    Artist,
}

impl Resource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Track => "track",
            Self::Album => "album",
            Self::Artist => "artist",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RandomSeed(u64);

impl RandomSeed {
    pub const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    pub const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

    pub fn new(value: u64) -> Self {
        Self(value)
    }

    pub fn for_request(user: Option<Uuid>, resource: Resource, day: chrono::NaiveDate) -> Self {
        let mut hash = Self::FNV_OFFSET;

        match user {
            Some(user) => hash = Self::mix(hash, user.as_bytes()),
            None => hash = Self::mix(hash, b"anonymous"),
        }

        hash = Self::mix(hash, resource.as_str().as_bytes());
        hash = Self::mix(hash, &day.num_days_from_ce().to_le_bytes());

        Self(hash)
    }

    pub fn get(self) -> u64 {
        self.0
    }

    fn mix(mut hash: u64, bytes: &[u8]) -> u64 {
        for byte in bytes {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(Self::FNV_PRIME);
        }

        hash
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selector_text_normalises_and_drops_blanks() {
        assert_eq!(
            SelectorText::new("  It\u{2019}s  Just Forever ")
                .unwrap()
                .unwrap()
                .as_str(),
            "it's just forever"
        );
        assert_eq!(SelectorText::new("   ").unwrap(), None);
        assert_eq!(SelectorText::new("").unwrap(), None);
        assert!(matches!(
            SelectorText::new(&"a".repeat(MAX_SELECTOR_TEXT + 1)),
            Err(ValidationError::TooLong { field: "selector" })
        ));
        assert!(
            SelectorText::new(&"a".repeat(MAX_SELECTOR_TEXT))
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn full_text_selectors_preserve_query_and_normalize_terms() {
        let selector = FullTextSelector::new("  Marías   BEATOPIA ")
            .unwrap()
            .unwrap();
        assert_eq!(selector.query(), "  Marías   BEATOPIA ");
        assert_eq!(selector.terms(), ["marias", "beatopia"]);
        assert_eq!(FullTextSelector::new("  \t ").unwrap(), None);
        assert!(matches!(
            FullTextSelector::new(&"a".repeat(MAX_SELECTOR_TEXT + 1)),
            Err(ValidationError::TooLong { field: "selector" })
        ));
    }

    #[test]
    fn id_sets_dedupe_keep_order_and_bound() {
        let ids = IdSet::new([3u8, 1, 3, 2]).unwrap();
        assert_eq!(ids.iter().copied().collect::<Vec<_>>(), [3, 1, 2]);
        assert_eq!(ids.len(), 3);
        assert!(!ids.is_empty());

        assert!(matches!(
            IdSet::<u8>::new(Vec::new()),
            Err(ValidationError::Empty { field: "ids" })
        ));
        assert!(matches!(
            IdSet::new((0..=MAX_SELECTOR_IDS as u8).collect::<Vec<_>>()),
            Err(ValidationError::TooLong { field: "ids" })
        ));
        assert!(IdSet::new(vec![7u8; 300]).is_ok());
    }

    #[test]
    fn ranges_reject_an_inverted_pair() {
        let range = Range::new(Some(2i32), Some(5)).unwrap();
        assert_eq!(range.from(), Some(2));
        assert_eq!(range.to(), Some(5));
        assert!(Range::new(Some(5i32), Some(5)).is_ok());
        assert!(Range::new(None, Some(5i32)).is_ok());
        assert!(Range::new(Some(5i32), None).is_ok());
        assert!(matches!(
            Range::new(Some(5i32), Some(1)),
            Err(ValidationError::Invalid { field: "range" })
        ));
        assert!(matches!(
            Range::new(None::<i32>, None),
            Err(ValidationError::Invalid { field: "range" })
        ));
    }

    #[test]
    fn sort_direction_parses_exactly() {
        assert_eq!(SortDirection::parse("asc"), Some(SortDirection::Ascending));
        assert_eq!(
            SortDirection::parse("desc"),
            Some(SortDirection::Descending)
        );
        assert_eq!(SortDirection::parse("sideways"), None);
        assert_eq!(SortDirection::parse("ASC"), None);
        assert_eq!(SortDirection::default(), SortDirection::Ascending);
        assert_eq!(SortDirection::Ascending.sql(), "ASC");
        assert_eq!(SortDirection::Descending.sql(), "DESC");
    }

    #[test]
    fn role_selector_is_the_stored_roles_plus_the_two_memberships() {
        assert_eq!(
            RoleSelector::parse("composer"),
            Some(RoleSelector::Composer)
        );
        assert_eq!(
            RoleSelector::parse("albumartist"),
            Some(RoleSelector::AlbumArtist)
        );
        assert_eq!(RoleSelector::parse("artist"), Some(RoleSelector::Artist));
        assert_eq!(RoleSelector::parse("other"), None);
        assert_eq!(RoleSelector::parse("Composer"), None);
        assert_eq!(RoleSelector::parse(""), None);
        assert_eq!(RoleSelector::parse(" "), None);

        assert_eq!(
            RoleSelector::Composer.stored_role(),
            Some(CreditRole::Composer)
        );
        assert_eq!(RoleSelector::Artist.stored_role(), None);
        assert_eq!(RoleSelector::AlbumArtist.stored_role(), None);
        assert_eq!(RoleSelector::ALL.len(), CreditRole::ALL.len() + 2);
    }

    #[test]
    fn role_sets_are_bounded_and_non_empty() {
        let roles = RoleSet::new([RoleSelector::Composer, RoleSelector::Artist]).unwrap();
        assert_eq!(roles.len(), 2);
        assert!(matches!(
            RoleSet::new(Vec::new()),
            Err(ValidationError::Empty { field: "ids" })
        ));
    }

    #[test]
    fn the_seed_is_repeatable_per_principal_resource_and_day() {
        let user = Uuid::from_u128(7);
        let day = chrono::NaiveDate::from_ymd_opt(2026, 9, 26).unwrap();

        let first = RandomSeed::for_request(Some(user), Resource::Track, day);
        let again = RandomSeed::for_request(Some(user), Resource::Track, day);
        assert_eq!(first, again, "the same request keeps its page boundaries");

        assert_ne!(
            first,
            RandomSeed::for_request(Some(user), Resource::Album, day)
        );
        assert_ne!(
            first,
            RandomSeed::for_request(Some(user), Resource::Track, day.succ_opt().unwrap())
        );
        assert_ne!(
            first,
            RandomSeed::for_request(Some(Uuid::from_u128(8)), Resource::Track, day)
        );
        assert_ne!(first, RandomSeed::for_request(None, Resource::Track, day));
        assert_eq!(first.get(), RandomSeed::new(first.get()).get());
    }
}
