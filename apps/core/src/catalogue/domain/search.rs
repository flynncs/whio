use super::ValidationError;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchQuery(String);

impl SearchQuery {
    pub fn new(value: String) -> Result<Self, ValidationError> {
        if value.is_empty() {
            return Err(ValidationError::Empty { field: "query" });
        }

        if value.chars().count() > 500 {
            return Err(ValidationError::TooLong { field: "query" });
        }

        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SearchLimit(u8);

impl SearchLimit {
    pub fn new(value: u32) -> Result<Self, ValidationError> {
        if !(1..=25).contains(&value) {
            return Err(ValidationError::OutOfRange {
                field: "limit",
                min: 1,
                max: 25,
            });
        }

        Ok(Self(value as u8))
    }

    pub fn get(self) -> u8 {
        self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogueSearch {
    query: SearchQuery,
    limit: SearchLimit,
}

impl CatalogueSearch {
    pub fn new(query: SearchQuery, limit: SearchLimit) -> Self {
        Self { query, limit }
    }

    pub fn query(&self) -> &SearchQuery {
        &self.query
    }

    pub fn limit(&self) -> SearchLimit {
        self.limit
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_query_accepts_boundary_lengths() {
        assert!(SearchQuery::new("x".to_owned()).is_ok());
        assert!(SearchQuery::new("x".repeat(500)).is_ok());
    }

    #[test]
    fn search_query_rejects_boundary_violations() {
        assert!(matches!(
            SearchQuery::new(String::new()),
            Err(ValidationError::Empty { field: "query" })
        ));
        assert!(matches!(
            SearchQuery::new("x".repeat(501)),
            Err(ValidationError::TooLong { field: "query" })
        ));
    }

    #[test]
    fn search_query_counts_unicode_characters() {
        assert!(SearchQuery::new("🎵".repeat(500)).is_ok());
        assert!(SearchQuery::new("🎵".repeat(501)).is_err());
    }

    #[test]
    fn search_limit_accepts_boundaries() {
        assert_eq!(SearchLimit::new(1).unwrap().get(), 1);
        assert_eq!(SearchLimit::new(25).unwrap().get(), 25);
    }

    #[test]
    fn search_limit_rejects_boundary_violations() {
        for value in [0, 26, u32::MAX] {
            assert!(matches!(
                SearchLimit::new(value),
                Err(ValidationError::OutOfRange {
                    field: "limit",
                    min: 1,
                    max: 25,
                })
            ));
        }
    }
}
