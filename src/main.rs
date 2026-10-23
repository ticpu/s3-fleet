//! `s3-fleet`: the command-line face of the library, for scripts and people.
//!
//! # Contract with scripts
//!
//! - stdout carries data only: host names one per line, or `--json`, whose
//!   shape (`"format": 1`) changes only with that number. Diagnostics go to
//!   stderr as `s3-fleet: <subject>: <error chain>`.
//! - The exit code says what happened; see `cli::exit` for the table. A
//!   caller can tell "someone else wrote, rerun" from "the key changed" from
//!   "S3 is down" without reading stderr.
//! - Nothing prompts except `setup`, and `setup` refuses to run without a TTY.
//!   No other command reads stdin.
//! - Logs go to stderr through tracing; `-v` raises the level, `RUST_LOG`
//!   overrides it, and `NO_COLOR` turns colour off.

mod cli;

fn main() {}
