//! The CLI-local state file: a **TOML subset**, written and read by hand.
//!
//! Why hand-written: this crate's only dependency is the CONTRACT (the
//! architecture law — Part 4.3 of the plan names no toml crate), so the
//! client-side state format is ours, not a dependency's. The subset is exactly
//! what `net export` / `net import` / `net profile` need and nothing more:
//!
//! ```toml
//! [section]
//! key = "text"
//! count = 42
//! flag = true
//! list = ["a", "b"]
//! ```
//!
//! Doc 04 §5 is explicit that these workflows are CLI-local: the profile files,
//! export/import, and history retention have no contract route, and this module
//! is where that honesty is implemented. The CLI itself never WRITES a file —
//! `net export` writes stdout and the user redirects (law #8: the host is
//! read-only).

#![cfg(any(
    feature = "cmd-net",
    feature = "cmd-dns",
    feature = "cmd-bt",
    feature = "cmd-tunnel",
    feature = "cmd-web",
    feature = "cmd-socket",
    feature = "cmd-discovery"
))]

use alloc::string::{String, ToString};
use alloc::vec::Vec;

/// A scalar or list value in the state file.
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Str(String),
    Int(i64),
    Bool(bool),
    List(Vec<Value>),
}

impl Value {
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::Str(s) => Some(s),
            _ => None,
        }
    }
    pub fn as_int(&self) -> Option<i64> {
        match self {
            Value::Int(i) => Some(*i),
            _ => None,
        }
    }
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Value::Bool(b) => Some(*b),
            _ => None,
        }
    }
    /// The values of a list, as a borrowed slice.
    pub fn as_list(&self) -> Option<&[Value]> {
        match self {
            Value::List(items) => Some(items),
            _ => None,
        }
    }
    /// A list of text values (empty when the key holds anything else).
    pub fn text_list(&self) -> Vec<String> {
        match self {
            Value::List(items) => items.iter().filter_map(|v| v.as_str().map(String::from)).collect(),
            _ => Vec::new(),
        }
    }
    /// Serialize this value back to the state-file syntax.
    fn write(&self, out: &mut String) {
        match self {
            Value::Str(s) => write_quoted(out, s),
            Value::Int(i) => out.push_str(&i.to_string()),
            Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
            Value::List(items) => {
                out.push('[');
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        out.push_str(", ");
                    }
                    item.write(out);
                }
                out.push(']');
            }
        }
    }
}

fn write_quoted(out: &mut String, s: &str) {
    out.push('"');
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            c => out.push(c),
        }
    }
    out.push('"');
}

/// One key/value line inside a section.
#[derive(Clone, Debug)]
pub struct Entry {
    pub key: String,
    pub value: Value,
}

/// A `[name]` section of the state file.
#[derive(Clone, Debug)]
pub struct Section {
    pub name: String,
    pub entries: Vec<Entry>,
}

impl Section {
    /// Set (or replace) a key in this section.
    pub fn set(&mut self, key: &str, value: Value) {
        if let Some(entry) = self.entries.iter_mut().find(|e| e.key == key) {
            entry.value = value;
        } else {
            self.entries.push(Entry {
                key: key.to_string(),
                value,
            });
        }
    }
}

/// A parsed state file: an ordered list of sections.
#[derive(Clone, Debug, Default)]
pub struct Doc {
    pub sections: Vec<Section>,
}

impl Doc {
    pub fn new() -> Self {
        Doc::default()
    }

    /// The section named `name`, creating it when absent.
    pub fn section(&mut self, name: &str) -> &mut Section {
        if let Some(index) = self.sections.iter().position(|s| s.name == name) {
            return &mut self.sections[index];
        }
        self.sections.push(Section {
            name: name.to_string(),
            entries: Vec::new(),
        });
        let last = self.sections.len() - 1;
        &mut self.sections[last]
    }

    /// A key's value from a section, when both exist.
    pub fn get(&self, section: &str, key: &str) -> Option<&Value> {
        self.sections
            .iter()
            .find(|s| s.name == section)
            .and_then(|s| s.entries.iter().find(|e| e.key == key))
            .map(|e| &e.value)
    }

    /// The section names, in file order.
    pub fn section_names(&self) -> Vec<String> {
        self.sections.iter().map(|s| s.name.clone()).collect()
    }

    /// Serialize to the state-file syntax.
    pub fn write(&self, out: &mut String) {
        for section in &self.sections {
            out.push_str(&format!("[{}]\n", section.name));
            for entry in &section.entries {
                out.push_str(&format!("{} = ", entry.key));
                entry.value.write(out);
                out.push('\n');
            }
            out.push('\n');
        }
    }

    /// Parse the state-file syntax. A `#` line is a comment; every non-empty,
    /// non-section line must be `key = value`; a value is a quoted string, an
    /// integer, a bool, or a list of those.
    pub fn parse(text: &str) -> Result<Doc, String> {
        let mut doc = Doc::new();
        let mut current = String::new();
        for (number, raw) in text.lines().enumerate() {
            let line = strip_comment(raw).trim();
            if line.is_empty() {
                continue;
            }
            if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
                let name = name.trim();
                if name.is_empty() {
                    return Err(format!("line {}: an empty section name", number + 1));
                }
                current = name.to_string();
                doc.section(&current);
                continue;
            }
            if current.is_empty() {
                return Err(format!("line {}: a key before any [section]", number + 1));
            }
            let Some((key, value)) = line.split_once('=') else {
                return Err(format!("line {}: expected `key = value`, got `{line}`", number + 1));
            };
            let key = key.trim();
            if key.is_empty() {
                return Err(format!("line {}: an empty key", number + 1));
            }
            let value = parse_value(value.trim())
                .map_err(|e| format!("line {}: {e}", number + 1))?;
            doc.section(&current).set(key, value);
        }
        Ok(doc)
    }
}

/// Strip a `#` comment, but never one inside a quoted string (a value may
/// legitimately contain a `#`).
fn strip_comment(line: &str) -> &str {
    let mut quoted = false;
    for (index, ch) in line.char_indices() {
        match ch {
            '"' => quoted = !quoted,
            '#' if !quoted => return &line[..index],
            _ => {}
        }
    }
    line
}

/// Parse one right-hand-side value.
fn parse_value(text: &str) -> Result<Value, String> {
    if text.is_empty() {
        return Err("a value is missing".to_string());
    }
    if let Some(inner) = text.strip_prefix('"') {
        return Ok(Value::Str(unquote(inner)?));
    }
    if text == "true" {
        return Ok(Value::Bool(true));
    }
    if text == "false" {
        return Ok(Value::Bool(false));
    }
    if let Some(inner) = text.strip_prefix('[') {
        let inner = inner
            .strip_suffix(']')
            .ok_or_else(|| format!("an unterminated list: `{text}`"))?;
        let mut items = Vec::new();
        for part in split_list(inner) {
            items.push(parse_value(part)?);
        }
        return Ok(Value::List(items));
    }
    text.parse::<i64>()
        .map(Value::Int)
        .map_err(|_| format!("unrecognized value `{text}` (use \"text\", 123, true, or [..])"))
}

/// Split a list body on commas that are not inside quotes.
fn split_list(inner: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut quoted = false;
    let mut start = 0usize;
    for (index, ch) in inner.char_indices() {
        match ch {
            '"' => quoted = !quoted,
            ',' if !quoted => {
                parts.push(&inner[start..index]);
                start = index + 1;
            }
            _ => {}
        }
    }
    let tail = inner[start..].trim();
    if !tail.is_empty() {
        parts.push(&inner[start..]);
    }
    parts
        .into_iter()
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .collect()
}

fn unquote(inner: &str) -> Result<String, String> {
    let mut out = String::new();
    let mut escaped = false;
    for ch in inner.chars() {
        if escaped {
            match ch {
                'n' => out.push('\n'),
                't' => out.push('\t'),
                other => out.push(other),
            }
            escaped = false;
            continue;
        }
        match ch {
            '\\' => escaped = true,
            '"' => return Ok(out),
            other => out.push(other),
        }
    }
    Err("an unterminated string".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_a_document() {
        let mut doc = Doc::new();
        doc.section("networking").set("enabled", Value::Bool(true));
        doc.section("dns")
            .set("servers", Value::List(vec![Value::Str("1.1.1.1".into()), Value::Str("8.8.8.8".into())]));
        doc.section("routes").set("entries", Value::List(vec![Value::Str("0.0.0.0/0|192.168.1.1|100".into())]));
        let mut text = String::new();
        doc.write(&mut text);
        let parsed = Doc::parse(&text).expect("our own output must parse");
        assert_eq!(parsed.get("networking", "enabled"), Some(&Value::Bool(true)));
        assert_eq!(parsed.get("dns", "servers").map(Value::text_list).unwrap_or_default().len(), 2);
        assert_eq!(
            parsed.get("routes", "entries").map(Value::text_list),
            Some(vec!["0.0.0.0/0|192.168.1.1|100".to_string()])
        );
    }

    #[test]
    fn comments_blank_lines_and_escapes_survive() {
        let text = "# a comment\n\n[dns]\nnote = \"a # not a comment\"\n";
        let doc = Doc::parse(text).unwrap();
        assert_eq!(doc.get("dns", "note").and_then(Value::as_str), Some("a # not a comment"));
    }

    #[test]
    fn malformed_input_is_named_not_guessed() {
        assert!(Doc::parse("key = 1\n").is_err(), "a key before a section is an error");
        assert!(Doc::parse("[a]\nkey\n").is_err(), "a key with no value is an error");
        assert!(Doc::parse("[a]\nkey = \"open\n").is_err(), "an unterminated string is an error");
    }

    #[test]
    fn list_values_keep_text_and_order() {
        let doc = Doc::parse("[wifi]\nknown = [\"Home\", \"Cafe\"]\n").unwrap();
        assert_eq!(doc.get("wifi", "known").map(Value::text_list), Some(vec!["Home".to_string(), "Cafe".to_string()]));
    }
}
