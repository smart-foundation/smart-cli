//! The SHAPE of a JSON value: the key paths a document carries, without the
//! values.
//!
//! Why test-local: the crate's only dependency is the contract, so the tests
//! cannot pull in a JSON library. What the parity proof needs is not a
//! parser but a comparison — "these two documents have the same shape" — and
//! that is what this gives: the list of key paths, in document order.
//!
//! A list contributes ONE path (`name[]`) whatever its length: the shape is
//! the shape, not the count of items. The FIRST element carries it (the
//! elements of a contract-shaped list are the same struct).

/// The key paths of a JSON document, in document order.
pub fn shape(text: &str) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    let mut index = 0usize;
    let mut out = Vec::new();
    parse_value(&chars, &mut index, "", &mut out);
    out
}

/// Skip whitespace.
fn skip_spaces(chars: &[char], index: &mut usize) {
    while *index < chars.len() && chars[*index].is_whitespace() {
        *index += 1;
    }
}

/// Parse one value; record the key paths it carries.
fn parse_value(chars: &[char], index: &mut usize, path: &str, out: &mut Vec<String>) {
    skip_spaces(chars, index);
    if *index >= chars.len() {
        return;
    }
    match chars[*index] {
        '{' => {
            *index += 1;
            skip_spaces(chars, index);
            if *index < chars.len() && chars[*index] == '}' {
                *index += 1;
                return;
            }
            loop {
                skip_spaces(chars, index);
                let Some(key) = read_string(chars, index) else {
                    return;
                };
                skip_spaces(chars, index);
                if *index < chars.len() && chars[*index] == ':' {
                    *index += 1;
                }
                let child = if path.is_empty() {
                    key.clone()
                } else {
                    format!("{path}.{key}")
                };
                out.push(child.clone());
                parse_value(chars, index, &child, out);
                skip_spaces(chars, index);
                if *index < chars.len() && chars[*index] == ',' {
                    *index += 1;
                    continue;
                }
                if *index < chars.len() && chars[*index] == '}' {
                    *index += 1;
                }
                return;
            }
        }
        '[' => {
            *index += 1;
            let child = format!("{path}[]");
            skip_spaces(chars, index);
            if *index < chars.len() && chars[*index] == ']' {
                *index += 1;
                return;
            }
            // The FIRST element carries the shape; the rest repeat it.
            parse_value(chars, index, &child, out);
            // The remaining elements are skipped (their shape is the first
            // element's), so the walk returns to the parent at the ']'.
            loop {
                skip_spaces(chars, index);
                if *index >= chars.len() {
                    return;
                }
                match chars[*index] {
                    ']' => {
                        *index += 1;
                        return;
                    }
                    ',' => {
                        *index += 1;
                        skip_value(chars, index);
                    }
                    _ => {
                        skip_value(chars, index);
                    }
                }
            }
        }
        '"' => {
            read_string(chars, index);
        }
        _ => {
            while *index < chars.len()
                && !matches!(chars[*index], ',' | '}' | ']' | ' ' | '\t' | '\n' | '\r')
            {
                *index += 1;
            }
        }
    }
}

/// Skip one value (no shape recorded): the other elements of a list.
fn skip_value(chars: &[char], index: &mut usize) {
    skip_spaces(chars, index);
    if *index >= chars.len() {
        return;
    }
    match chars[*index] {
        '{' | '[' => {
            let mut depth = 0usize;
            let mut in_string = false;
            let mut escaped = false;
            while *index < chars.len() {
                let ch = chars[*index];
                *index += 1;
                if in_string {
                    if escaped {
                        escaped = false;
                    } else if ch == '\\' {
                        escaped = true;
                    } else if ch == '"' {
                        in_string = false;
                    }
                    continue;
                }
                match ch {
                    '"' => in_string = true,
                    '{' | '[' => depth += 1,
                    '}' | ']' => {
                        depth -= 1;
                        if depth == 0 {
                            return;
                        }
                    }
                    _ => {}
                }
            }
        }
        '"' => {
            read_string(chars, index);
        }
        _ => {
            while *index < chars.len()
                && !matches!(chars[*index], ',' | '}' | ']' | ' ' | '\t' | '\n' | '\r')
            {
                *index += 1;
            }
        }
    }
}

/// Read a JSON string, returning its key (the escapes are decoded: the shape
/// needs the key's spelling).
fn read_string(chars: &[char], index: &mut usize) -> Option<String> {
    skip_spaces(chars, index);
    if *index >= chars.len() || chars[*index] != '"' {
        return None;
    }
    *index += 1;
    let mut out = String::new();
    while *index < chars.len() {
        let ch = chars[*index];
        *index += 1;
        match ch {
            '"' => return Some(out),
            '\\' => {
                if *index < chars.len() {
                    let escaped = chars[*index];
                    *index += 1;
                    out.push(match escaped {
                        'n' => '\n',
                        't' => '\t',
                        'r' => '\r',
                        other => other,
                    });
                }
            }
            other => out.push(other),
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_shape_is_the_key_paths() {
        assert_eq!(
            shape("{\"a\":1,\"b\":[{\"c\":true}]}"),
            vec!["a".to_string(), "b".to_string(), "b[].c".to_string()]
        );
    }

    #[test]
    fn a_list_of_several_elements_has_the_shape_of_the_first() {
        assert_eq!(
            shape("{\"b\":[{\"c\":1},{\"c\":2},{\"c\":3}],\"d\":true}"),
            vec![
                "b".to_string(),
                "b[].c".to_string(),
                "d".to_string()
            ]
        );
    }

    #[test]
    fn an_empty_object_has_no_keys() {
        assert!(shape("{}").is_empty());
    }
}
