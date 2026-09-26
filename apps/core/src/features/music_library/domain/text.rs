use unicode_normalization::UnicodeNormalization;

use super::ValidationError;

pub(crate) fn normalize_for_matching(value: &str) -> String {
    value
        .nfc()
        .map(fold_typographic)
        .collect::<String>()
        .to_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn fold_typographic(character: char) -> char {
    match character {
        '\u{2019}' | '\u{2018}' | '\u{02bc}' => '\'',
        '\u{2013}' | '\u{2014}' | '\u{2011}' => '-',
        other => other,
    }
}

pub(crate) fn required(
    field: &'static str,
    value: String,
    max: usize,
) -> Result<String, ValidationError> {
    let value = value.trim();

    if value.is_empty() {
        return Err(ValidationError::Empty { field });
    }

    if value.chars().count() > max {
        return Err(ValidationError::TooLong { field });
    }

    Ok(value.to_owned())
}

pub(crate) fn optional(
    field: &'static str,
    value: Option<String>,
    max: usize,
) -> Result<Option<String>, ValidationError> {
    let Some(value) = value else {
        return Ok(None);
    };
    let value = value.trim();

    if value.is_empty() {
        return Ok(None);
    }

    if value.chars().count() > max {
        return Err(ValidationError::TooLong { field });
    }

    Ok(Some(value.to_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalization_folds_case_unicode_and_whitespace() {
        assert_eq!(normalize_for_matching("  À  B "), "à b");
        assert_eq!(normalize_for_matching("Discovery"), "discovery");
        assert_eq!(normalize_for_matching("a\t\n b"), "a b");
    }

    #[test]
    fn normalization_folds_typographic_variants_to_ascii() {
        assert_eq!(
            normalize_for_matching("It\u{2019}s Just Forever"),
            normalize_for_matching("It's Just Forever")
        );
        assert_eq!(
            normalize_for_matching("Don\u{2018}t Be Consumed"),
            "don't be consumed"
        );
        assert_eq!(normalize_for_matching("a\u{02bc}b"), "a'b");
        assert_eq!(normalize_for_matching("Song \u{2013} Live"), "song - live");
        assert_eq!(normalize_for_matching("Song \u{2014} Live"), "song - live");
        assert_eq!(normalize_for_matching("Song \u{2011} Live"), "song - live");
    }

    #[test]
    fn required_trims_and_bounds() {
        assert_eq!(
            required("title", "  song  ".to_owned(), 10).unwrap(),
            "song"
        );
        assert!(matches!(
            required("title", "   ".to_owned(), 10),
            Err(ValidationError::Empty { field: "title" })
        ));
        assert!(matches!(
            required("title", "a".repeat(11), 10),
            Err(ValidationError::TooLong { field: "title" })
        ));
    }

    #[test]
    fn optional_drops_blank_values() {
        assert_eq!(optional("label", None, 10).unwrap(), None);
        assert_eq!(optional("label", Some("  ".to_owned()), 10).unwrap(), None);
        assert_eq!(
            optional("label", Some(" EMI ".to_owned()), 10).unwrap(),
            Some("EMI".to_owned())
        );
    }
}
