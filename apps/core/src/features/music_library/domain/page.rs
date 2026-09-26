use super::{TrackView, ValidationError};

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
    RecentlyAdded,
}

impl TrackOrder {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Title => "title",
            Self::RecentlyAdded => "recently_added",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "title" => Some(Self::Title),
            "recently_added" => Some(Self::RecentlyAdded),
            _ => None,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TrackListFilter {
    scope: TrackListScope,
}

impl TrackListFilter {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_scope(mut self, scope: TrackListScope) -> Self {
        self.scope = scope;
        self
    }

    pub fn scope(&self) -> TrackListScope {
        self.scope
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
}
