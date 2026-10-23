//! Label selectors: which hosts a command applies to.
//!
//! A selector is a comma-separated list of terms, all of which must hold:
//!
//! - `key=value`: the label is set to exactly that value
//! - `key!=value`: the label is absent, or set to another value
//! - `key`: the label is set, to anything
//! - `!key`: the label is absent
//!
//! Several selectors (repeated `-l`) are ANDed as well. Whitespace is not
//! trimmed: a value is compared byte for byte, as stored.

use std::collections::BTreeMap;
use std::fmt;
use std::str::FromStr;

use crate::doc::validate_key_name;

/// A parsed selector: terms that must all hold.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Selector {
    terms: Vec<Term>,
    text: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Term {
    Equals(String, String),
    NotEquals(String, String),
    Exists(String),
    Absent(String),
}

/// A selector that does not parse.
#[derive(Debug, thiserror::Error)]
#[error("invalid selector term {term:?} in {selector:?}: {reason}")]
pub struct SelectorError {
    selector: String,
    term: String,
    reason: &'static str,
}

impl FromStr for Selector {
    type Err = SelectorError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let terms = s
            .split(',')
            .map(|term| parse_term(s, term))
            .collect::<Result<_, _>>()?;
        Ok(Self {
            terms,
            text: s.to_string(),
        })
    }
}

impl fmt::Display for Selector {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.text)
    }
}

fn parse_term(selector: &str, term: &str) -> Result<Term, SelectorError> {
    let fail = |reason| SelectorError {
        selector: selector.to_string(),
        term: term.to_string(),
        reason,
    };
    let key = |k: &str| -> Result<String, SelectorError> {
        validate_key_name(k).map_err(|_| fail("label keys use only A-Z a-z 0-9 . _ / -"))?;
        Ok(k.to_string())
    };
    if term.is_empty() {
        return Err(fail("empty term"));
    }
    if let Some((k, v)) = term.split_once("!=") {
        return Ok(Term::NotEquals(key(k)?, v.to_string()));
    }
    if let Some((k, v)) = term.split_once('=') {
        return Ok(Term::Equals(key(k)?, v.to_string()));
    }
    if let Some(k) = term.strip_prefix('!') {
        return Ok(Term::Absent(key(k)?));
    }
    Ok(Term::Exists(key(term)?))
}

impl Selector {
    /// Whether a host with these labels matches.
    pub fn matches(&self, labels: &BTreeMap<String, String>) -> bool {
        self.terms
            .iter()
            .all(|term| match term {
                Term::Equals(k, v) => labels.get(k) == Some(v),
                Term::NotEquals(k, v) => labels.get(k) != Some(v),
                Term::Exists(k) => labels.contains_key(k),
                Term::Absent(k) => !labels.contains_key(k),
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn labels(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    fn sel(s: &str) -> Selector {
        s.parse()
            .unwrap()
    }

    #[test]
    fn terms() {
        let prod_sbc = labels(&[("env", "prod"), ("role", "sbc")]);
        let test = labels(&[("env", "test")]);
        assert!(sel("env=prod").matches(&prod_sbc));
        assert!(!sel("env=prod").matches(&test));
        assert!(sel("env!=prod").matches(&test));
        assert!(sel("role!=sbc").matches(&test), "absent label is not equal");
        assert!(sel("role").matches(&prod_sbc));
        assert!(!sel("role").matches(&test));
        assert!(sel("!role").matches(&test));
        assert!(sel("env=prod,role=sbc").matches(&prod_sbc));
        assert!(!sel("env=prod,role=sbc").matches(&test));
        assert!(sel("env=").matches(&labels(&[("env", "")])));
        assert!(!sel("env=").matches(&test));
    }

    #[test]
    fn values_compare_exactly() {
        assert!(!sel("env= prod").matches(&labels(&[("env", "prod")])));
    }

    #[test]
    fn rejects_malformed() {
        for bad in ["", "a,,b", "=x", "!=x", "!", "a b=c", "a=b,", "!a=b"] {
            assert!(
                bad.parse::<Selector>()
                    .is_err(),
                "{bad:?} accepted"
            );
        }
    }
}
