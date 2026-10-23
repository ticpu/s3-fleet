//! A handle on one fleet: its bucket and the S3 client that reaches it.
//!
//! [`Fleet::load`] reads `fleet.yaml` and returns it with its ETag;
//! [`Fleet::commit`] writes a changed document back with `If-Match` on that
//! ETag, and [`Fleet::init`] creates the bucket and writes an empty document
//! with `If-None-Match: *`. Every write is conditional: an unconditional PUT
//! would let two concurrent editors each overwrite the other's change without
//! a trace.
//!
//! A [`Snapshot`] is consumed by the commit: after a commit, or a conflict,
//! load again, because the ETag it carried is stale.
//!
//! S3 failures are sorted into the [`Error`] variants a caller acts on
//! differently: absent, forbidden, conflict, unreachable, no credentials.
//!
//! The fleet name is the bucket name, restricted to 3 to 63 characters of
//! `[a-z0-9-]`, starting and ending with a letter or digit: a valid bucket name
//! on every S3 implementation, with no dot to break TLS on virtual-hosted
//! endpoints, and safe to put in a shell command unquoted (the ssh wrapper
//! relies on that). It is checked before any request, so a typo fails here and
//! not as an S3 error about some bucket.

use crate::Error;
use crate::doc::FleetDoc;
use crate::s3::{Precondition, S3, S3Error};

/// The object holding the fleet document, at the bucket's root.
pub const DOCUMENT_KEY: &str = "fleet.yaml";

const CONTENT_TYPE: &str = "application/yaml";

/// A fleet document and the ETag it was read at.
#[derive(Clone, Debug)]
pub struct Snapshot {
    /// The document as read.
    pub doc: FleetDoc,
    etag: String,
}

/// What [`Fleet::init`] found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Init {
    /// An empty document was written.
    Created,
    /// A document was already there and was left untouched.
    AlreadyExists,
}

/// One fleet: a bucket named after it, reached through an [`S3`] client.
#[derive(Clone, Debug)]
pub struct Fleet {
    name: String,
    s3: S3,
}

impl Fleet {
    /// A handle on fleet `name`; the name is checked, nothing is fetched.
    pub fn new(name: &str, s3: S3) -> Result<Self, Error> {
        validate_fleet_name(name)?;
        Ok(Self {
            name: name.to_string(),
            s3,
        })
    }

    /// The fleet's name, which is also its bucket.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The S3 client, for callers keeping objects of their own in the bucket.
    pub fn s3(&self) -> &S3 {
        &self.s3
    }

    /// Create the bucket if needed, then an empty document if there is none.
    /// An existing document is never overwritten.
    pub async fn init(&self) -> Result<Init, Error> {
        self.s3
            .create_bucket(&self.name)
            .await
            .map_err(|e| self.classify(e))?;
        let body = FleetDoc::new()
            .to_yaml()?
            .into_bytes();
        match self
            .s3
            .put_object(
                &self.name,
                DOCUMENT_KEY,
                body,
                CONTENT_TYPE,
                Some(&Precondition::IfNoneMatch),
            )
            .await
        {
            Ok(_) => Ok(Init::Created),
            Err(S3Error::PreconditionFailed { .. }) => Ok(Init::AlreadyExists),
            Err(e) => Err(self.classify(e)),
        }
    }

    /// Read and parse the fleet document.
    pub async fn load(&self) -> Result<Snapshot, Error> {
        let object = self
            .s3
            .get_object(&self.name, DOCUMENT_KEY)
            .await
            .map_err(|e| self.classify(e))?;
        let etag = object
            .etag
            .ok_or_else(|| Error::MissingEtag {
                fleet: self
                    .name
                    .clone(),
            })?;
        let doc = FleetDoc::parse(&object.body).map_err(|source| Error::Document {
            fleet: self
                .name
                .clone(),
            source,
        })?;
        Ok(Snapshot { doc, etag })
    }

    /// Write `doc` if the stored document is still the one `snapshot` read.
    /// Otherwise nothing is written and the error is [`Error::Conflict`].
    pub async fn commit(&self, snapshot: Snapshot, doc: &FleetDoc) -> Result<(), Error> {
        let body = doc
            .to_yaml()?
            .into_bytes();
        match self
            .s3
            .put_object(
                &self.name,
                DOCUMENT_KEY,
                body,
                CONTENT_TYPE,
                Some(&Precondition::IfMatch(snapshot.etag)),
            )
            .await
        {
            Ok(_) => Ok(()),
            // Deleted since it was read: still someone else's write.
            Err(S3Error::NotFound { .. }) => Err(Error::Conflict {
                fleet: self
                    .name
                    .clone(),
            }),
            Err(e) => Err(self.classify(e)),
        }
    }

    fn classify(&self, e: S3Error) -> Error {
        let fleet = self
            .name
            .clone();
        match e {
            S3Error::NotFound { .. } => Error::NotFound {
                fleet,
                source: Box::new(e),
            },
            S3Error::Forbidden { .. } => Error::Forbidden {
                fleet,
                source: Box::new(e),
            },
            S3Error::PreconditionFailed { .. } => Error::Conflict { fleet },
            S3Error::CredentialsRequired { .. } => Error::CredentialsRequired { fleet },
            S3Error::Transport { .. } => Error::Unreachable {
                fleet,
                source: Box::new(e),
            },
            e => Error::S3 {
                fleet,
                source: Box::new(e),
            },
        }
    }
}

/// See the notes above: a bucket name, shell-safe, with no dot.
pub fn validate_fleet_name(name: &str) -> Result<(), Error> {
    let bytes = name.as_bytes();
    let edge = |b: u8| b.is_ascii_lowercase() || b.is_ascii_digit();
    let valid = (3..=63).contains(&bytes.len())
        && bytes
            .iter()
            .all(|&b| edge(b) || b == b'-')
        && edge(bytes[0])
        && edge(bytes[bytes.len() - 1]);
    if !valid {
        return Err(Error::InvalidFleetName {
            name: name.to_string(),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fleet_names() {
        for good in ["abc", "prod-sbc", "a1-b2", &"a".repeat(63)] {
            assert!(validate_fleet_name(good).is_ok(), "{good:?} refused");
        }
        for bad in [
            "ab",
            &"a".repeat(64),
            "-ab",
            "ab-",
            "Prod",
            "a.b",
            "a_b",
            "a b",
            "a;b",
        ] {
            assert!(validate_fleet_name(bad).is_err(), "{bad:?} accepted");
        }
    }
}
