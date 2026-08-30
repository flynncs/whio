use std::convert::Infallible;

use axum::{extract::FromRequestParts, http::request::Parts};

pub(crate) struct RawQuery(Vec<(String, String)>);

impl RawQuery {
    pub(crate) fn parse(query: &str) -> Self {
        Self(
            form_urlencoded::parse(query.as_bytes())
                .map(|(name, value)| (name.into_owned(), value.into_owned()))
                .collect(),
        )
    }

    pub(crate) fn get(&self, name: &str) -> Option<&str> {
        self.0
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }
}

// serde-based query extractors collapse duplicates and reorder, which breaks
// subsonic signing and any future transparent proxying of raw requests
impl<S> FromRequestParts<S> for RawQuery
where
    S: Send + Sync,
{
    type Rejection = Infallible;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        Ok(parts
            .uri
            .query()
            .map(Self::parse)
            .unwrap_or_else(|| Self(Vec::new())))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preserves_parameter_order_and_duplicates() {
        let query = RawQuery::parse("z=1&a=2&z=3&m=4");

        assert_eq!(
            query.0,
            vec![
                ("z".to_owned(), "1".to_owned()),
                ("a".to_owned(), "2".to_owned()),
                ("z".to_owned(), "3".to_owned()),
                ("m".to_owned(), "4".to_owned()),
            ]
        );
    }

    #[test]
    fn decodes_percent_escapes_and_plus_as_space() {
        let query = RawQuery::parse("name=a%20b&pass=c%2Bd&plus=e+f");

        assert_eq!(query.get("name"), Some("a b"));
        assert_eq!(query.get("pass"), Some("c+d"));
        assert_eq!(query.get("plus"), Some("e f"));
    }

    #[test]
    fn get_returns_the_first_occurrence() {
        let query = RawQuery::parse("u=flynn&u=duplicate");

        assert_eq!(query.get("u"), Some("flynn"));
    }

    #[test]
    fn empty_query_yields_no_parameters() {
        let query = RawQuery::parse("");

        assert_eq!(query.get("u"), None);
        assert!(query.0.is_empty());
    }
}
