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
