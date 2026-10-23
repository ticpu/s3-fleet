//! A fleet of SSH hosts kept as one YAML document in an S3 bucket: each host's
//! pinned host keys and free-form labels, plus fleet-wide attributes.
//!
//! One bucket holds one fleet, and the bucket is named after the fleet. A
//! reader needs nothing but the fleet name: `discover` finds the endpoint
//! through a DNS TXT record, and the document is world-readable, so listing
//! hosts or exporting their keys works anonymously. Writing takes S3
//! credentials.
//!
//! # Model
//!
//! `Fleet::load` returns a `Snapshot`: the parsed `FleetDoc` and the ETag
//! it was read at. The caller changes the document with its pure methods and
//! hands both back to `Fleet::commit`, which writes with `If-Match` on that
//! ETag. When someone else wrote in between, the commit fails with
//! `Error::Conflict` and nothing is written; the caller reloads and decides.
//! Hosts, keys and labels live in the same object, so a reader can never see
//! a host without its key.
//!
//! # What the library does not do
//!
//! It never retries, never falls back to a cached copy and never prompts: each
//! of those hides a signal its caller may need. The `cache` module is a
//! separate, opt-in layer whose results say where they came from and how old
//! they are. Every fallible call returns a typed error with its source chain.
//!
//! The API is async (tokio): ssh-keyscan runs under `tokio::process`, and HTTP
//! goes through reqwest's async client.

pub mod doc;
pub mod known_hosts;
pub mod s3;
pub mod selector;

#[cfg(test)]
mod testkeys;
