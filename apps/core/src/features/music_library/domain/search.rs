use super::{ArtistCredit, text::normalize_for_search};

const STOPWORDS: &[&str] = &[
    "a", "an", "and", "are", "as", "at", "be", "but", "by", "for", "from", "i", "in", "is", "it",
    "me", "my", "of", "on", "or", "that", "the", "to", "you", "your",
];

const MIN_SIGNAL_LENGTH: usize = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum MatchQuality {
    Exact,
    Prefix,
    WordBoundary,
    Substring,
    ArtistOnly,
}

impl MatchQuality {
    pub fn is_confident(self) -> bool {
        matches!(self, Self::Exact | Self::Prefix | Self::WordBoundary)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Exact => "exact",
            Self::Prefix => "prefix",
            Self::WordBoundary => "word_boundary",
            Self::Substring => "substring",
            Self::ArtistOnly => "artist_only",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchConfidence {
    Strong,
    Weak,
    None,
}

impl SearchConfidence {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Strong => "strong",
            Self::Weak => "weak",
            Self::None => "none",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchTerms {
    normalized: String,
    tokens: Vec<String>,
    low_signal: bool,
}

impl SearchTerms {
    pub fn new(query: &str) -> Self {
        let normalized = normalize_for_search(query);
        let tokens = normalized
            .split_whitespace()
            .map(str::to_owned)
            .collect::<Vec<_>>();
        let low_signal = tokens.is_empty()
            || tokens
                .iter()
                .all(|token| is_stopword(token) || token.chars().count() < MIN_SIGNAL_LENGTH);

        Self {
            normalized,
            tokens,
            low_signal,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.tokens.is_empty()
    }

    pub fn low_signal(&self) -> bool {
        self.low_signal
    }

    pub fn classify(&self, value: &str) -> Option<MatchQuality> {
        if self.is_empty() {
            return None;
        }

        let normalized = normalize_for_search(value);

        if normalized == self.normalized {
            return Some(MatchQuality::Exact);
        }
        if normalized.starts_with(&self.normalized) {
            return Some(MatchQuality::Prefix);
        }

        let words = normalized.split_whitespace().collect::<Vec<_>>();
        if self
            .tokens
            .iter()
            .all(|token| words.iter().any(|word| *word == token))
        {
            return Some(MatchQuality::WordBoundary);
        }
        if self.tokens.len() > 1 && normalized.contains(&self.normalized) {
            return Some(MatchQuality::Substring);
        }

        None
    }
}

pub(crate) fn track_quality(
    terms: &SearchTerms,
    title: &str,
    artists: &[ArtistCredit],
) -> Option<MatchQuality> {
    terms.classify(title).or_else(|| {
        artists
            .iter()
            .any(|artist| terms.classify(artist.name()).is_some())
            .then_some(MatchQuality::ArtistOnly)
    })
}

pub(crate) fn album_quality(
    terms: &SearchTerms,
    title: &str,
    album_artist: Option<&str>,
) -> Option<MatchQuality> {
    terms.classify(title).or_else(|| {
        album_artist
            .is_some_and(|name| terms.classify(name).is_some())
            .then_some(MatchQuality::ArtistOnly)
    })
}

pub(crate) fn best_field_quality<'a>(
    terms: &SearchTerms,
    values: impl Iterator<Item = &'a str>,
) -> Option<MatchQuality> {
    values.filter_map(|value| terms.classify(value)).min()
}

fn is_stopword(token: &str) -> bool {
    STOPWORDS.contains(&token)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn terms(query: &str) -> SearchTerms {
        SearchTerms::new(query)
    }

    #[test]
    fn an_exact_title_beats_a_prefix_and_a_substring() {
        let terms = terms("zebra");
        assert_eq!(terms.classify("Zebra"), Some(MatchQuality::Exact));
        assert_eq!(terms.classify("Zebra Crossing"), Some(MatchQuality::Prefix));
        assert_eq!(
            terms.classify("The Zebra Story"),
            Some(MatchQuality::WordBoundary)
        );
    }

    #[test]
    fn a_single_short_token_does_not_match_inside_a_word() {
        let terms = terms("me");
        assert_eq!(terms.classify("all-american bitch"), None);
        assert_eq!(terms.classify("Me"), Some(MatchQuality::Exact));
        assert_eq!(terms.classify("Me and You"), Some(MatchQuality::Prefix));
    }

    #[test]
    fn the_word_boundary_tier_requires_every_token_as_a_word() {
        let terms = terms("just forever");
        assert_eq!(
            terms.classify("It's Just Forever"),
            Some(MatchQuality::WordBoundary)
        );
        assert_eq!(
            terms.classify("Forever Just"),
            Some(MatchQuality::WordBoundary)
        );
        assert_eq!(terms.classify("Just In Time"), None);
    }

    #[test]
    fn a_multi_token_phrase_may_match_mid_title() {
        let terms = terms("heart of gl");
        assert_eq!(
            terms.classify("A Heart of Glass Reprise"),
            Some(MatchQuality::Substring)
        );
    }

    #[test]
    fn a_lone_stopword_or_short_token_is_low_signal() {
        assert!(terms("the").low_signal());
        assert!(terms("me").low_signal());
        assert!(terms("a").low_signal());
        assert!(!terms("daft punk").low_signal());
        assert!(!terms("love").low_signal());
    }

    #[test]
    fn accent_folding_matches_plain_composed_and_decomposed_text() {
        for (query, value) in [
            ("marias", "The Marías"),
            ("beyonce", "Beyoncé"),
            ("sigur ros", "Sigur Rós"),
            ("Jose\u{301}", "José"),
        ] {
            assert!(terms(query).classify(value).is_some(), "{query} / {value}");
        }
        assert_eq!(terms("strasse").classify("Straße"), None);
        assert_eq!(terms("smore").classify("Smørrebrød"), None);
        assert_eq!(terms("lodz").classify("Łódź"), None);
    }

    #[test]
    fn l25_plain_query_matches_the_accented_artist_name() {
        assert_eq!(
            terms("marias").classify("The Marías"),
            Some(MatchQuality::WordBoundary)
        );
    }

    #[test]
    fn quality_ranks_best_first() {
        let mut qualities = [
            MatchQuality::ArtistOnly,
            MatchQuality::Substring,
            MatchQuality::Exact,
            MatchQuality::WordBoundary,
            MatchQuality::Prefix,
        ];
        qualities.sort();
        assert_eq!(
            qualities,
            [
                MatchQuality::Exact,
                MatchQuality::Prefix,
                MatchQuality::WordBoundary,
                MatchQuality::Substring,
                MatchQuality::ArtistOnly,
            ]
        );
    }

    #[test]
    fn track_quality_falls_back_to_an_artist_credit() {
        let terms = terms("daft punk");
        let artists = vec![ArtistCredit::new("Daft Punk".to_owned(), None).unwrap()];
        assert_eq!(
            track_quality(&terms, "Instant Crush", &artists),
            Some(MatchQuality::ArtistOnly)
        );
        assert_eq!(track_quality(&terms, "Instant Crush", &[]), None);
    }

    #[test]
    fn album_quality_matches_its_album_artist() {
        let terms = terms("daft");
        assert_eq!(
            album_quality(&terms, "Discovery", Some("Daft Punk")),
            Some(MatchQuality::ArtistOnly)
        );
    }

    #[test]
    fn the_best_field_quality_is_the_strongest_match() {
        let terms = terms("zebra");
        let values = ["Zebra Crossing", "Zebra"];
        assert_eq!(
            best_field_quality(&terms, values.into_iter()),
            Some(MatchQuality::Exact)
        );
    }
}
