use serde_json::Value;

use crate::features::metadata::refresh::DocumentError;

pub(crate) fn append_items(
    buffer: &mut Vec<u8>,
    cursor: &mut usize,
    opened: &mut bool,
    items: &mut Vec<Value>,
    limit: usize,
    chunk: &[u8],
) -> Result<bool, DocumentError> {
    buffer.extend_from_slice(chunk);
    loop {
        skip_separators(buffer, cursor);
        if !*opened {
            if *cursor == buffer.len() {
                return Ok(false);
            }
            if buffer[*cursor] != b'[' {
                return Err(DocumentError::ResolutionFailed);
            }
            *opened = true;
            *cursor += 1;
            continue;
        }
        skip_separators(buffer, cursor);
        if *cursor >= buffer.len() {
            return Ok(false);
        }
        if buffer[*cursor] == b']' {
            return Ok(true);
        }
        let Some(end) = value_end(buffer, *cursor) else {
            return Ok(false);
        };
        let value = serde_json::from_slice(&buffer[*cursor..end])
            .map_err(|_| DocumentError::ResolutionFailed)?;
        items.push(value);
        *cursor = end;
        if items.len() >= limit {
            return Ok(true);
        }
    }
}

pub(crate) fn finish(buffer: &[u8], cursor: usize, opened: bool) -> Result<(), DocumentError> {
    if !opened {
        return Err(DocumentError::ResolutionFailed);
    }
    let mut cursor = cursor;
    skip_separators(buffer, &mut cursor);
    if cursor < buffer.len() && buffer[cursor] == b']' {
        Ok(())
    } else {
        Err(DocumentError::ResolutionFailed)
    }
}

fn skip_separators(buffer: &[u8], cursor: &mut usize) {
    while *cursor < buffer.len()
        && (buffer[*cursor].is_ascii_whitespace() || buffer[*cursor] == b',')
    {
        *cursor += 1;
    }
}

fn value_end(buffer: &[u8], start: usize) -> Option<usize> {
    let first = *buffer.get(start)?;
    if first == b'{' || first == b'[' {
        let mut stack = Vec::new();
        let mut quoted = false;
        let mut escaped = false;
        for (offset, byte) in buffer[start..].iter().copied().enumerate() {
            if quoted {
                if escaped {
                    escaped = false;
                } else if byte == b'\\' {
                    escaped = true;
                } else if byte == b'"' {
                    quoted = false;
                }
                continue;
            }
            match byte {
                b'"' => quoted = true,
                b'{' => stack.push(b'}'),
                b'[' => stack.push(b']'),
                b'}' | b']' => {
                    if stack.pop() != Some(byte) {
                        return None;
                    }
                    if stack.is_empty() {
                        return Some(start + offset + 1);
                    }
                }
                _ => {}
            }
        }
        return None;
    }
    if first == b'"' {
        let mut escaped = false;
        for (offset, byte) in buffer[start + 1..].iter().copied().enumerate() {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                return Some(start + offset + 2);
            }
        }
        return None;
    }
    buffer[start..]
        .iter()
        .position(|byte| byte.is_ascii_whitespace() || *byte == b',' || *byte == b']')
        .map(|offset| start + offset)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requires_array_and_accepts_whitespace_and_commas() {
        let mut buffer = Vec::new();
        let mut cursor = 0;
        let mut opened = false;
        let mut values = Vec::new();
        assert!(
            append_items(
                &mut buffer,
                &mut cursor,
                &mut opened,
                &mut values,
                50,
                b"  [ {\"a\":1}, 2 ]"
            )
            .unwrap()
        );
        assert_eq!(
            values,
            vec![serde_json::json!({"a": 1}), serde_json::json!(2)]
        );
        let mut invalid = Vec::new();
        let mut offset = 0;
        let mut opened = false;
        assert_eq!(
            append_items(
                &mut invalid,
                &mut offset,
                &mut opened,
                &mut Vec::new(),
                50,
                b"{}"
            ),
            Err(DocumentError::ResolutionFailed)
        );
    }

    #[test]
    fn waits_for_split_values_and_stops_at_the_limit() {
        let mut buffer = Vec::new();
        let mut cursor = 0;
        let mut opened = false;
        let mut values = Vec::new();
        assert!(
            !append_items(
                &mut buffer,
                &mut cursor,
                &mut opened,
                &mut values,
                2,
                b"[ {\"name\":"
            )
            .unwrap()
        );
        assert!(
            append_items(
                &mut buffer,
                &mut cursor,
                &mut opened,
                &mut values,
                2,
                b"\"a\"}, 3, 4]"
            )
            .unwrap()
        );
        assert_eq!(
            values,
            vec![serde_json::json!({"name": "a"}), serde_json::json!(3)]
        );
    }

    #[test]
    fn unfinished_array_is_a_resolution_failure() {
        let mut buffer = Vec::new();
        let mut cursor = 0;
        let mut opened = false;
        let mut values = Vec::new();
        assert!(
            !append_items(
                &mut buffer,
                &mut cursor,
                &mut opened,
                &mut values,
                50,
                b"[1"
            )
            .unwrap()
        );
        assert_eq!(
            finish(&buffer, cursor, opened),
            Err(DocumentError::ResolutionFailed)
        );
    }
}
