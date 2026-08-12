use crate::media::ByteRange;

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum RangeRequest {
    Full,
    Partial(ByteRange),
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub(crate) enum RangeError {
    #[error("range unit is not bytes")]
    InvalidUnit,

    #[error("multiple ranges are not supported")]
    MultipleRanges,

    #[error("range is malformed")]
    Malformed,

    #[error("range is not satisfiable")]
    Unsatisfiable,
}

pub(crate) fn parse_range_header(
    header: Option<&str>,
    content_length: u64,
) -> Result<RangeRequest, RangeError> {
    let Some(header) = header else {
        return Ok(RangeRequest::Full);
    };

    let (unit, spec) = header.trim().split_once('=').ok_or(RangeError::Malformed)?;
    if !unit.eq_ignore_ascii_case("bytes") {
        return Err(RangeError::InvalidUnit);
    }
    if spec.contains(',') {
        return Err(RangeError::MultipleRanges);
    }

    let (left, right) = spec.trim().split_once('-').ok_or(RangeError::Malformed)?;
    let start = parse_side(left)?;
    let end = parse_side(right)?;

    match (start, end) {
        (None, None) => Err(RangeError::Malformed),
        (None, Some(suffix)) => {
            if suffix == 0 || content_length == 0 {
                return Err(RangeError::Unsatisfiable);
            }

            Ok(RangeRequest::Partial(ByteRange {
                start: content_length.saturating_sub(suffix),
                end: content_length - 1,
            }))
        }
        (Some(start), None) => {
            if content_length == 0 || start >= content_length {
                return Err(RangeError::Unsatisfiable);
            }

            Ok(RangeRequest::Partial(ByteRange {
                start,
                end: content_length - 1,
            }))
        }
        (Some(start), Some(end)) => {
            if content_length == 0 || start >= content_length || start > end {
                return Err(RangeError::Unsatisfiable);
            }

            Ok(RangeRequest::Partial(ByteRange {
                start,
                end: end.min(content_length - 1),
            }))
        }
    }
}

fn parse_side(side: &str) -> Result<Option<u64>, RangeError> {
    if side.is_empty() {
        Ok(None)
    } else {
        side.parse::<u64>()
            .map(Some)
            .map_err(|_| RangeError::Malformed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn partial(result: Result<RangeRequest, RangeError>) -> ByteRange {
        match result.unwrap() {
            RangeRequest::Partial(range) => range,
            RangeRequest::Full => panic!("expected a partial range"),
        }
    }

    #[test]
    fn missing_header_requests_the_full_content() {
        assert_eq!(parse_range_header(None, 1_000), Ok(RangeRequest::Full));
    }

    #[test]
    fn explicit_range_has_an_inclusive_end() {
        let range = partial(parse_range_header(Some("bytes=0-99"), 1_000));

        assert_eq!(range.start(), 0);
        assert_eq!(range.end(), 99);
        assert_eq!(range.len(), 100);
    }

    #[test]
    fn open_and_suffix_ranges_resolve_against_content_length() {
        let range = partial(parse_range_header(Some("bytes=100-"), 1_000));
        assert_eq!((range.start(), range.end()), (100, 999));

        let range = partial(parse_range_header(Some("bytes=-100"), 1_000));
        assert_eq!((range.start(), range.end()), (900, 999));
    }

    #[test]
    fn explicit_end_is_clamped_and_invalid_ranges_are_rejected() {
        let range = partial(parse_range_header(Some("bytes=950-1200"), 1_000));
        assert_eq!((range.start(), range.end()), (950, 999));

        assert_eq!(
            parse_range_header(Some("bytes=1000-"), 1_000),
            Err(RangeError::Unsatisfiable)
        );
        assert_eq!(
            parse_range_header(Some("bytes=100-50"), 1_000),
            Err(RangeError::Unsatisfiable)
        );
        assert_eq!(
            parse_range_header(Some("bytes=-0"), 1_000),
            Err(RangeError::Unsatisfiable)
        );
    }

    #[test]
    fn malformed_and_multiple_ranges_are_rejected() {
        assert_eq!(
            parse_range_header(Some("items=0-99"), 1_000),
            Err(RangeError::InvalidUnit)
        );
        assert_eq!(
            parse_range_header(Some("bytes=0-99,100-199"), 1_000),
            Err(RangeError::MultipleRanges)
        );
        assert_eq!(
            parse_range_header(Some("bytes=-"), 1_000),
            Err(RangeError::Malformed)
        );
        assert_eq!(
            parse_range_header(Some("bytes=0-99"), 0),
            Err(RangeError::Unsatisfiable)
        );
    }
}
