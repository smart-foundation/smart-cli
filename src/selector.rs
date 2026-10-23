//! The selector: `--where <field> <op> <value>` over a listed contract type
//! (grammar law #4). Ops `==, !=, >, <, ~=`; combinators `and` / `or`;
//! parentheses optional but supported. A field that does not exist in the
//! contract type is a USAGE error naming the VALID fields — generated from the
//! schema ([`smart_protocol::contract::fields_of`]), never a hand-list.
//!
//! The selector is schema-blind at runtime: the eval hands back a field's
//! [`Cell`] (a borrowed value from the reader), and the predicate evaluates
//! against it. This keeps the selector testable without a schema and lets a
//! command supply its own field accessor for whatever contract type it lists.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;

/// Why a selector failed to parse (all map to exit code 2 — usage).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SelectorError {
    /// The named field is not in the contract type; `valid` lists the real ones.
    UnknownField {
        field: String,
        valid: Vec<String>,
    },
    /// An operator that is not one of `==, !=, >, <, ~=`.
    UnknownOp { op: String },
    /// A malformed expression (missing operand, unbalanced parens, ...).
    Syntax { detail: String },
}

impl SelectorError {
    /// The exact usage message (naming valid fields for the field error).
    pub fn message(&self) -> String {
        match self {
            SelectorError::UnknownField { field, valid } => {
                let names = if valid.is_empty() {
                    "(this type declares no filterable fields)".to_string()
                } else {
                    valid.join(", ")
                };
                format!("unknown field '{field}' in --where; valid fields are: {names}")
            }
            SelectorError::UnknownOp { op } => {
                format!("unknown operator '{op}' in --where; ops are ==, !=, >, <, ~=")
            }
            SelectorError::Syntax { detail } => {
                format!("bad --where selector: {detail}")
            }
        }
    }
}

/// A comparison operator.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Op {
    Eq,
    Ne,
    Gt,
    Lt,
    /// "~=" is a substring / set-membership test (contains, for text).
    Contains,
}

fn parse_op(op: &str) -> Result<Op, SelectorError> {
    match op {
        "==" | "=" => Ok(Op::Eq),
        "!=" => Ok(Op::Ne),
        ">" => Ok(Op::Gt),
        "<" => Ok(Op::Lt),
        "~=" => Ok(Op::Contains),
        other => Err(SelectorError::UnknownOp {
            op: other.to_string(),
        }),
    }
}

/// A leaf condition: `field op value`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Condition {
    pub field: String,
    pub op: Op,
    pub value: String,
}

/// A combinator between sub-predicates.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Comb {
    And,
    Or,
}

/// A parsed selector expression tree.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Pred {
    Leaf(Condition),
    Group(Comb, Vec<Pred>),
}

impl Pred {
    /// Parse selector tokens against a contract type name (fields validated
    /// against the schema). `type_name` is the schema struct name, e.g.
    /// `"NetworkInterface"`; pass `None` to skip field validation (tests).
    pub fn parse(tokens: &[String], type_name: Option<&str>) -> Result<Pred, SelectorError> {
        let valid: Option<Vec<String>> = type_name
            .and_then(smart_protocol::contract::fields_of)
            .map(|fields| fields.iter().map(|f| f.to_string()).collect());
        let mut parser = Parser { tokens, pos: 0, valid: valid.as_ref() };
        let pred = parser.parse_or()?;
        if parser.pos != parser.tokens.len() {
            return Err(SelectorError::Syntax {
                detail: format!(
                    "trailing tokens after the selector: {}",
                    parser.tokens[parser.pos..].join(" ")
                ),
            });
        }
        Ok(pred)
    }

    /// Evaluate against a field accessor. `field_of` returns the field's
    /// borrowed value, or `None` when the record does not carry it. The cell
    /// borrows the record's own data (an independent lifetime from the query).
    pub fn matches<'c, F>(&self, field_of: &F) -> bool
    where
        F: Fn(&str) -> Option<Cell<'c>>,
    {
        match self {
            Pred::Leaf(c) => eval_condition(c, field_of),
            Pred::Group(comb, preds) => match comb {
                Comb::And => preds.iter().all(|p| p.matches(field_of)),
                Comb::Or => preds.iter().any(|p| p.matches(field_of)),
            },
        }
    }
}

/// A field's value, borrowed from a contract reader.
#[derive(Clone, Copy, Debug)]
pub enum Cell<'a> {
    Int(i64),
    Float(f64),
    Bool(bool),
    Text(&'a str),
}

impl<'a> Cell<'a> {
    fn as_f64(&self) -> Option<f64> {
        match self {
            Cell::Int(i) => Some(*i as f64),
            Cell::Float(f) => Some(*f),
            Cell::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
            Cell::Text(_) => None,
        }
    }
    fn as_text(&self) -> Option<&str> {
        match self {
            Cell::Text(t) => Some(t),
            Cell::Bool(b) => Some(if *b { "true" } else { "false" }),
            Cell::Int(_) => None,
            Cell::Float(_) => None,
        }
    }
}

fn eval_condition<'c, F>(c: &Condition, field_of: &F) -> bool
where
    F: Fn(&str) -> Option<Cell<'c>>,
{
    let Some(cell) = field_of(&c.field) else {
        return false;
    };
    match c.op {
        Op::Eq => eq_cell(&cell, &c.value),
        Op::Ne => !eq_cell(&cell, &c.value),
        Op::Gt | Op::Lt => {
            let (Some(left), Ok(right)) = (cell.as_f64(), c.value.trim().parse::<f64>()) else {
                return false;
            };
            if c.op == Op::Gt {
                left > right
            } else {
                left < right
            }
        }
        Op::Contains => cell.as_text().is_some_and(|t| t.contains(&c.value)),
    }
}

fn eq_cell(cell: &Cell<'_>, value: &str) -> bool {
    match cell {
        Cell::Text(t) => *t == value,
        Cell::Bool(b) => {
            let want = matches!(value, "true" | "1" | "yes");
            let not = matches!(value, "false" | "0" | "no");
            (want && *b) || (not && !*b)
        }
        Cell::Int(i) => value.trim().parse::<i64>() == Ok(*i),
        Cell::Float(f) => value.trim().parse::<f64>() == Ok(*f),
    }
}

struct Parser<'a> {
    tokens: &'a [String],
    pos: usize,
    valid: Option<&'a Vec<String>>,
}

impl<'a> Parser<'a> {
    fn peek(&self) -> Option<&str> {
        self.tokens.get(self.pos).map(String::as_str)
    }

    fn parse_or(&mut self) -> Result<Pred, SelectorError> {
        let mut preds = vec![self.parse_and()?];
        while self.peek() == Some("or") {
            self.pos += 1;
            preds.push(self.parse_and()?);
        }
        if preds.len() == 1 {
            Ok(preds.pop().unwrap())
        } else {
            Ok(Pred::Group(Comb::Or, preds))
        }
    }

    fn parse_and(&mut self) -> Result<Pred, SelectorError> {
        let mut preds = vec![self.parse_atom()?];
        while self.peek() == Some("and") {
            self.pos += 1;
            preds.push(self.parse_atom()?);
        }
        if preds.len() == 1 {
            Ok(preds.pop().unwrap())
        } else {
            Ok(Pred::Group(Comb::And, preds))
        }
    }

    fn parse_atom(&mut self) -> Result<Pred, SelectorError> {
        if self.peek() == Some("(") {
            self.pos += 1;
            let inner = self.parse_or()?;
            if self.peek() != Some(")") {
                return Err(SelectorError::Syntax {
                    detail: "unbalanced '(' in selector".to_string(),
                });
            }
            self.pos += 1;
            return Ok(inner);
        }
        let field = self
            .peek()
            .ok_or(SelectorError::Syntax { detail: "expected a field name".to_string() })
            .map(String::from)?;
        self.pos += 1;
        if let Some(valid) = self.valid {
            if !valid.iter().any(|f| f.as_str() == field.as_str()) {
                return Err(SelectorError::UnknownField {
                    field,
                    valid: valid.clone(),
                });
            }
        }
        let op_tok = self
            .peek()
            .ok_or(SelectorError::Syntax { detail: format!("expected an operator after '{field}'") })
            .map(String::from)?;
        self.pos += 1;
        let op = parse_op(&op_tok)?;
        let value = self
            .peek()
            .ok_or(SelectorError::Syntax { detail: format!("expected a value after '{field} {op_tok}'") })
            .map(String::from)?;
        self.pos += 1;
        Ok(Pred::Leaf(Condition {
            field,
            op,
            value,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn toks(s: &str) -> Vec<String> {
        s.split_whitespace().map(String::from).collect()
    }

    #[test]
    fn parses_a_simple_equality() {
        let pred = Pred::parse(&toks("connected == true"), None).unwrap();
        assert!(matches!(pred, Pred::Leaf(_)));
    }

    #[test]
    fn parses_and_or_precedence() {
        let pred = Pred::parse(&toks("a == 1 or b == 2 and c == 3"), None).unwrap();
        assert!(matches!(pred, Pred::Group(Comb::Or, _)));
    }

    #[test]
    fn parses_parentheses() {
        let pred = Pred::parse(&toks("( a == 1 or b == 2 ) and c == 3"), None).unwrap();
        assert!(matches!(pred, Pred::Group(Comb::And, _)));
    }

    #[test]
    #[cfg(feature = "cmd-net")]
    fn unknown_field_names_valid_fields() {
        let err = Pred::parse(&toks("nope == 1"), Some("NetworkInterface")).unwrap_err();
        assert!(matches!(err, SelectorError::UnknownField { .. }));
        let message = err.message();
        assert!(message.contains("nope"), "{message}");
        assert!(message.contains("connected"), "{message}");
    }

    #[test]
    fn unknown_op_is_usage() {
        let err = Pred::parse(&toks("a === 1"), None).unwrap_err();
        assert!(matches!(err, SelectorError::UnknownOp { .. }));
    }

    #[test]
    fn numeric_and_text_and_contains_evaluate() {
        let pred = Pred::parse(&toks("strength > -50"), None).unwrap();
        let accessor = |name: &str| match name {
            "strength" => Some(Cell::Int(-40)),
            _ => None,
        };
        assert!(pred.matches(&accessor));

        let pred = Pred::parse(&toks("ssid ~= Home"), None).unwrap();
        let accessor = |name: &str| match name {
            "ssid" => Some(Cell::Text("Home Network")),
            _ => None,
        };
        assert!(pred.matches(&accessor));
    }
}
