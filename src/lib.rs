//! A fleet of SSH hosts kept as one YAML document in an S3 bucket: each host's
//! pinned host keys and free-form labels, plus fleet-wide attributes.
//!
//! One bucket holds one fleet, named after it unless a per-fleet DNS record
//! or the caller names another. A reader needs nothing but the fleet name:
//! [`discover`] finds the endpoint (and bucket) through a DNS TXT record, and
//! the document is world-readable, so listing hosts or exporting their keys
//! works anonymously. Writing takes S3 credentials.
//!
//! # Model
//!
//! [`Fleet::load`] returns a [`Snapshot`]: the parsed [`FleetDoc`] and the
//! ETag it was read at. The caller changes the document with its pure methods
//! and hands both back to [`Fleet::commit`], which writes with `If-Match` on
//! that ETag. When someone else wrote in between, the commit fails with
//! [`Error::Conflict`] and nothing is written; the caller reloads and decides.
//! Hosts, keys and labels live in the same object, so a reader can never see
//! a host without its key.
//!
//! # What the library does not do
//!
//! It never retries, never falls back to a cached copy and never prompts: each
//! of those hides a signal its caller may need. The [`cache`] module is a
//! separate, opt-in layer whose results say where they came from and how old
//! they are. Every fallible call returns a typed error with its source chain.
//!
//! The API is async (tokio): ssh-keyscan runs under `tokio::process`, and HTTP
//! goes through reqwest's async client.

pub mod cache;
pub mod discover;
pub mod doc;
pub mod fleet;
pub mod keyscan;
pub mod known_hosts;
pub mod s3;
pub mod selector;

#[cfg(test)]
mod testkeys;

pub use doc::{DocError, FleetDoc, Host};
pub use fleet::{Fleet, Init, Snapshot};
pub use s3::S3Error;

/// A failed fleet operation.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The name is not a valid fleet (bucket) name.
    #[error(
        "invalid fleet name {name:?}: 3 to 63 of a-z 0-9 -, starting and ending with a letter or digit"
    )]
    InvalidFleetName {
        /// The name.
        name: String,
    },
    /// The bucket name breaks the fleet-name rule.
    #[error(
        "invalid bucket name {name:?}: 3 to 63 of a-z 0-9 -, starting and ending with a letter or digit"
    )]
    InvalidBucketName {
        /// The name.
        name: String,
    },
    /// The bucket or its document does not exist; [`Fleet::init`] creates
    /// both.
    #[error("fleet {fleet} does not exist in bucket {bucket}")]
    NotFound {
        /// The fleet.
        fleet: String,
        /// The bucket it was looked for in.
        bucket: String,
        /// The S3 response.
        #[source]
        source: Box<S3Error>,
    },
    /// Access denied. Anonymously, the fleet may also not exist.
    #[error("fleet {fleet}: access denied")]
    Forbidden {
        /// The fleet.
        fleet: String,
        /// The S3 response, which says whether the request was anonymous.
        #[source]
        source: Box<S3Error>,
    },
    /// Someone else changed the document since it was read; nothing was
    /// written.
    #[error("fleet {fleet} changed since it was read; nothing was written, reload and retry")]
    Conflict {
        /// The fleet.
        fleet: String,
    },
    /// A write was attempted without credentials.
    #[error("fleet {fleet}: writing needs S3 credentials")]
    CredentialsRequired {
        /// The fleet.
        fleet: String,
    },
    /// S3 could not be reached at all.
    #[error("fleet {fleet}: S3 unreachable")]
    Unreachable {
        /// The fleet.
        fleet: String,
        /// The transport failure.
        #[source]
        source: Box<S3Error>,
    },
    /// Any other S3 failure.
    #[error("fleet {fleet}: S3 request failed")]
    S3 {
        /// The fleet.
        fleet: String,
        /// The S3 failure.
        #[source]
        source: Box<S3Error>,
    },
    /// The stored document did not parse or broke a rule.
    #[error("fleet {fleet}: stored document is invalid")]
    Document {
        /// The fleet.
        fleet: String,
        /// What is wrong with it.
        #[source]
        source: DocError,
    },
    /// A change to the document was refused.
    #[error(transparent)]
    Change(#[from] DocError),
    /// The store returned the document without an ETag, so it cannot be
    /// updated safely.
    #[error("fleet {fleet}: S3 returned no ETag; conditional writes are impossible")]
    MissingEtag {
        /// The fleet.
        fleet: String,
    },
}
