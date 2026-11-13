//! The query language shared by the UI, REST and MCP (spec §8).
//!
//! ```text
//! free text   "exact phrase"   -negation   OR   ( … )
//! type:passport  person:testov  from:maib  tag:warranty  has:attention
//! date:>=2024-01  added:<2025  field.expiry_date<2027-01-01  local:yes
//! has:expiring  due:<2027-06  due:>=2026-01-01
//! ```
//!
//! Precedence: `-` binds tightest, then implicit AND, then `OR`; parentheses
//! override.

use serde::{Deserialize, Serialize};

use crate::error::{Result, ServiceError};

/// Comparison of a field filter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum Op {
    Eq,
    Lt,
    Le,
    Gt,
    Ge,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Query {
    Term(String),
    Phrase(String),
    Not(Box<Query>),
    And(Vec<Query>),
    Or(Vec<Query>),
    Filter { key: String, op: Op, value: String },
    Has(String),
}

/// Filter keys of the language; `field.<key>` is handled separately.
const KEYS: [&str; 9] = [
    "type", "from", "person", "tag", "has", "date", "added", "due", "local",
];

/// Parses a query string. Errors are `Invalid` with code `query_syntax` and
/// `details.position`, the character offset of the problem.
pub fn parse(input: &str) -> Result<Query> {
    let mut parser = Parser {
        chars: input.chars().collect(),
        pos: 0,
    };
    parser.skip_ws();
    if parser.eof() {
        return Err(syntax(0, "the query is empty"));
    }
    let query = parser.or()?;
    parser.skip_ws();
    if !parser.eof() {
        return Err(syntax(parser.pos, "unexpected closing parenthesis"));
    }
    Ok(query)
}

fn syntax(position: usize, message: &str) -> ServiceError {
    ServiceError::invalid("query_syntax", message)
        .with_details(serde_json::json!({ "position": position }))
}

struct Parser {
    chars: Vec<char>,
    pos: usize,
}

impl Parser {
    fn eof(&self) -> bool {
        self.pos >= self.chars.len()
    }

    fn peek(&self) -> Option<char> {
        self.chars.get(self.pos).copied()
    }

    fn skip_ws(&mut self) {
        while self.peek().is_some_and(char::is_whitespace) {
            self.pos += 1;
        }
    }

    /// `OR` as a standalone upper-case word.
    fn at_or_keyword(&self) -> bool {
        self.peek() == Some('O')
            && self.chars.get(self.pos + 1) == Some(&'R')
            && self
                .chars
                .get(self.pos + 2)
                .is_none_or(|c| c.is_whitespace() || *c == '(')
    }

    fn or(&mut self) -> Result<Query> {
        let mut parts = vec![self.and()?];
        loop {
            self.skip_ws();
            if !self.at_or_keyword() {
                break;
            }
            self.pos += 2;
            parts.push(self.and()?);
        }
        Ok(collapse(parts, Query::Or))
    }

    fn and(&mut self) -> Result<Query> {
        let mut items = Vec::new();
        loop {
            self.skip_ws();
            if self.eof() || self.peek() == Some(')') || self.at_or_keyword() {
                break;
            }
            items.push(self.unary()?);
        }
        if items.is_empty() {
            return Err(syntax(self.pos, "expected a search term"));
        }
        Ok(collapse(items, Query::And))
    }

    fn unary(&mut self) -> Result<Query> {
        if self.peek() == Some('-')
            && self
                .chars
                .get(self.pos + 1)
                .is_some_and(|c| !c.is_whitespace())
        {
            self.pos += 1;
            return Ok(Query::Not(Box::new(self.unary()?)));
        }
        self.primary()
    }

    fn primary(&mut self) -> Result<Query> {
        let start = self.pos;
        match self.peek() {
            Some('(') => {
                self.pos += 1;
                let inner = self.or()?;
                self.skip_ws();
                if self.peek() != Some(')') {
                    return Err(syntax(start, "unclosed parenthesis"));
                }
                self.pos += 1;
                Ok(inner)
            }
            Some('"') => Ok(Query::Phrase(self.quoted()?)),
            _ => self.word(),
        }
    }

    /// Reads `"…"` starting at the opening quote.
    fn quoted(&mut self) -> Result<String> {
        let start = self.pos;
        self.pos += 1;
        let mut text = String::new();
        while let Some(c) = self.peek() {
            self.pos += 1;
            if c == '"' {
                return Ok(text);
            }
            text.push(c);
        }
        Err(syntax(start, "unterminated quoted text"))
    }

    fn word(&mut self) -> Result<Query> {
        let start = self.pos;
        let mut word = String::new();
        let mut quoted = None;
        while let Some(c) = self.peek() {
            if c.is_whitespace() || c == '(' || c == ')' {
                break;
            }
            if c == '"' && word.ends_with([':', '<', '>', '=']) {
                quoted = Some(self.quoted()?);
                break;
            }
            word.push(c);
            self.pos += 1;
        }
        if word.is_empty() {
            return Err(syntax(start, "expected a search term"));
        }
        let Some(split) = word.find([':', '<', '>', '=']) else {
            return Ok(Query::Term(word));
        };
        let key = word[..split].to_lowercase();
        let known = KEYS.contains(&key.as_str()) || (key.starts_with("field.") && key.len() > 6);
        if !known {
            return Ok(Query::Term(word));
        }

        let mut rest = &word[split..];
        if let Some(r) = rest.strip_prefix(':') {
            rest = r;
        }
        let (op, value) = [
            (">=", Op::Ge),
            ("<=", Op::Le),
            (">", Op::Gt),
            ("<", Op::Lt),
            ("=", Op::Eq),
        ]
        .iter()
        .find_map(|(prefix, op)| rest.strip_prefix(prefix).map(|v| (*op, v)))
        .unwrap_or((Op::Eq, rest));
        let value = quoted.unwrap_or_else(|| value.to_string());
        if value.is_empty() {
            return Err(syntax(start, "missing value after the filter key"));
        }
        if key == "has" {
            if op != Op::Eq {
                return Err(syntax(start, "has: takes a plain name"));
            }
            return Ok(Query::Has(value.to_lowercase()));
        }
        Ok(Query::Filter { key, op, value })
    }
}

fn collapse(mut parts: Vec<Query>, wrap: fn(Vec<Query>) -> Query) -> Query {
    if parts.len() == 1 {
        parts.remove(0)
    } else {
        wrap(parts)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn term(s: &str) -> Query {
        Query::Term(s.to_string())
    }

    fn filter(key: &str, op: Op, value: &str) -> Query {
        Query::Filter {
            key: key.to_string(),
            op,
            value: value.to_string(),
        }
    }

    #[test]
    fn parse_precedence() {
        // a -b OR c  =>  (a AND NOT b) OR c
        assert_eq!(
            parse("a -b OR c").unwrap(),
            Query::Or(vec![
                Query::And(vec![term("a"), Query::Not(Box::new(term("b")))]),
                term("c"),
            ])
        );
    }

    #[test]
    fn parentheses_override_precedence() {
        assert_eq!(
            parse("a (b OR c)").unwrap(),
            Query::And(vec![term("a"), Query::Or(vec![term("b"), term("c")])])
        );
        assert_eq!(
            parse("-(a b)").unwrap(),
            Query::Not(Box::new(Query::And(vec![term("a"), term("b")])))
        );
    }

    #[test]
    fn single_term_is_not_wrapped() {
        assert_eq!(parse("договор").unwrap(), term("договор"));
        assert_eq!(parse("  a  ").unwrap(), term("a"));
    }

    #[test]
    fn phrases_are_quoted() {
        assert_eq!(
            parse("\"договор аренды\" -\"old one\"").unwrap(),
            Query::And(vec![
                Query::Phrase("договор аренды".to_string()),
                Query::Not(Box::new(Query::Phrase("old one".to_string()))),
            ])
        );
    }

    #[test]
    fn parse_field_filter() {
        assert_eq!(
            parse("field.expiry_date<2027-01-01").unwrap(),
            filter("field.expiry_date", Op::Lt, "2027-01-01")
        );
        assert_eq!(
            parse("field.total>5000").unwrap(),
            filter("field.total", Op::Gt, "5000")
        );
        assert_eq!(
            parse("field.number:AB1234567").unwrap(),
            filter("field.number", Op::Eq, "AB1234567")
        );
        assert_eq!(
            parse("field.total>=10").unwrap(),
            filter("field.total", Op::Ge, "10")
        );
    }

    #[test]
    fn colon_filters_take_an_optional_operator() {
        assert_eq!(
            parse("type:passport").unwrap(),
            filter("type", Op::Eq, "passport")
        );
        assert_eq!(
            parse("date:>=2024-01").unwrap(),
            filter("date", Op::Ge, "2024-01")
        );
        assert_eq!(
            parse("added:<2025").unwrap(),
            filter("added", Op::Lt, "2025")
        );
        assert_eq!(
            parse("date:<=2024").unwrap(),
            filter("date", Op::Le, "2024")
        );
        assert_eq!(parse("local:yes").unwrap(), filter("local", Op::Eq, "yes"));
    }

    #[test]
    fn quoted_filter_values_keep_spaces() {
        assert_eq!(
            parse("person:\"Ivan Testov\"").unwrap(),
            filter("person", Op::Eq, "Ivan Testov")
        );
    }

    #[test]
    fn has_and_negated_has() {
        assert_eq!(
            parse("has:attention").unwrap(),
            Query::Has("attention".to_string())
        );
        assert_eq!(
            parse("-has:tags").unwrap(),
            Query::Not(Box::new(Query::Has("tags".to_string())))
        );
    }

    #[test]
    fn unknown_key_is_plain_text() {
        assert_eq!(parse("12:30").unwrap(), term("12:30"));
        assert_eq!(parse("http://x").unwrap(), term("http://x"));
    }

    #[test]
    fn lowercase_or_is_a_term() {
        assert_eq!(
            parse("a or b").unwrap(),
            Query::And(vec![term("a"), term("or"), term("b")])
        );
    }

    #[test]
    fn parse_error_has_position() {
        let e = parse("a (b OR c").unwrap_err();
        assert_eq!(e.code, "query_syntax");
        assert_eq!(e.details.as_ref().unwrap()["position"], 2);

        let e = parse("type:").unwrap_err();
        assert_eq!(e.code, "query_syntax");
        assert_eq!(e.details.as_ref().unwrap()["position"], 0);

        let e = parse("\"unterminated").unwrap_err();
        assert_eq!(e.details.as_ref().unwrap()["position"], 0);

        let e = parse("a )").unwrap_err();
        assert_eq!(e.details.as_ref().unwrap()["position"], 2);
    }

    #[test]
    fn empty_query_is_an_error() {
        assert_eq!(parse("   ").unwrap_err().code, "query_syntax");
    }
}
