//! A tiny STRUCTURAL JSON validator for the test suites: balanced braces,
//! balanced brackets, quoted strings with escapes, and a value at the root.
//!
//! Why test-local: this crate's only dependency is the contract (the
//! architecture law), so the golden tests cannot `use serde_json`. What the
//! tests need is not a parser but a proof — "this line is one complete JSON
//! value" — and that is exactly what this checks.

/// Structural errors a line can have.
#[derive(Debug, PartialEq, Eq)]
pub enum JsonError {
    /// The line was empty.
    Empty,
    UnterminatedString,
    Unbalanced { detail: &'static str },
    Trailing,
}

impl core::fmt::Display for JsonError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            JsonError::Empty => write!(f, "empty"),
            JsonError::UnterminatedString => write!(f, "unterminated string"),
            JsonError::Unbalanced { detail } => write!(f, "{detail}"),
            JsonError::Trailing => write!(f, "trailing content after the root value"),
        }
    }
}

/// Validate that `text` is exactly one complete JSON value.
pub fn validate(text: &str) -> Result<(), JsonError> {
    let bytes: Vec<char> = text.trim().chars().collect();
    if bytes.is_empty() {
        return Err(JsonError::Empty);
    }
    let mut stack: Vec<char> = Vec::new();
    let mut in_string = false;
    let mut escaped = false;
    let mut index = 0usize;
    while index < bytes.len() {
        let ch = bytes[index];
        index += 1;
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
            '{' | '[' => stack.push(if ch == '{' { '}' } else { ']' }),
            '}' | ']' => match stack.pop() {
                Some(expected) if expected == ch => {}
                _ => return Err(JsonError::Unbalanced { detail: "mismatched closer" }),
            },
            ' ' | '\t' | '\n' | '\r' => {}
            _ => {
                // A bare token (number, true/false/null) runs to the next
                // delimiter; only the root token is allowed to be bare.
                if stack.is_empty() {
                    while index < bytes.len() && !matches!(bytes[index], ' ' | '\t' | '\n' | '\r') {
                        index += 1;
                    }
                    if index != bytes.len() {
                        return Err(JsonError::Trailing);
                    }
                }
            }
        }
    }
    if in_string {
        return Err(JsonError::UnterminatedString);
    }
    if !stack.is_empty() {
        return Err(JsonError::Unbalanced { detail: "unclosed object or array" });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_complete_object_validates() {
        assert_eq!(validate("{\"a\":1,\"b\":[1,2,{\"c\":true}]}"), Ok(()));
        assert_eq!(validate("{}"), Ok(()));
        assert_eq!(validate("[]"), Ok(()));
        assert_eq!(validate("42"), Ok(()));
        assert_eq!(validate("\"text\""), Ok(()));
        assert_eq!(validate("null"), Ok(()));
    }

    #[test]
    fn a_fragment_does_not() {
        assert_eq!(validate("{\"a\":1"), Err(JsonError::Unbalanced { detail: "unclosed object or array" }));
        assert_eq!(validate("{\"a\":1}}"), Err(JsonError::Unbalanced { detail: "mismatched closer" }));
        assert_eq!(validate("{\"a\":\"unterminated"), Err(JsonError::UnterminatedString));
        assert_eq!(validate(""), Err(JsonError::Empty));
        // A brace inside a string is not structure.
        assert_eq!(validate("{\"a\":\"{\"}"), Ok(()));
    }

    #[test]
    fn escaped_quotes_inside_strings_are_not_structure() {
        assert_eq!(validate("{\"a\":\"he said \\\"hi\\\"\"}"), Ok(()));
    }
}
