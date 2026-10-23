//! Exit codes, the part of the CLI contract scripts branch on.
//!
//! Each code's meaning is `Exit::meaning`, which the man page's EXIT STATUS
//! is rendered from; README.md repeats the table for readers without the
//! binary. Codes are stable once released. Every error maps to exactly one
//! code through `exit_code`, whose matches name every variant: a new library
//! error does not compile until someone decides its code.

use s3_fleet::discover::DiscoverError;
use s3_fleet::keyscan::KeyscanError;
use s3_fleet::s3::S3Error;
use s3_fleet::selector::SelectorError;
use s3_fleet::{DocError, Error};

use super::config::ConfigError;

/// The process exit status.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Exit {
    Ok = 0,
    Error = 1,
    Usage = 2,
    Partial = 3,
    Conflict = 4,
    NotFound = 5,
    Credentials = 6,
    KeyMismatch = 7,
    Unreachable = 8,
}

impl Exit {
    pub const ALL: [Exit; 9] = [
        Exit::Ok,
        Exit::Error,
        Exit::Usage,
        Exit::Partial,
        Exit::Conflict,
        Exit::NotFound,
        Exit::Credentials,
        Exit::KeyMismatch,
        Exit::Unreachable,
    ];

    pub fn meaning(self) -> &'static str {
        match self {
            Exit::Ok => "success",
            Exit::Error => "any other error: I/O, an HTTP status, an invalid document, discovery",
            Exit::Usage => "usage error, or setup without a terminal",
            Exit::Partial => "partial: --keep-going committed some hosts, others failed",
            Exit::Conflict => "conflict: someone else wrote first, nothing was written; rerun",
            Exit::NotFound => {
                "the fleet does not exist, or is not public and the request was anonymous"
            }
            Exit::Credentials => "credentials missing or refused",
            Exit::KeyMismatch => "a host's key differs from the pinned one; add --rekey accepts it",
            Exit::Unreachable => "S3 unreachable, and no usable cache (or no --allow-stale)",
        }
    }
}

/// A failure whose exit code was decided where it happened, for cases no
/// library error carries (a TTY missing, partial success).
#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub struct Failure {
    pub exit: Exit,
    pub message: String,
}

impl Failure {
    pub fn new(exit: Exit, message: impl Into<String>) -> Self {
        Self {
            exit,
            message: message.into(),
        }
    }
}

/// The exit code for an error: the first error in its chain that decides one.
pub fn exit_code(err: &anyhow::Error) -> Exit {
    for cause in err.chain() {
        if let Some(code) = decide(cause) {
            return code;
        }
    }
    Exit::Error
}

fn decide(cause: &(dyn std::error::Error + 'static)) -> Option<Exit> {
    if let Some(f) = cause.downcast_ref::<Failure>() {
        return Some(f.exit);
    }
    if let Some(e) = cause.downcast_ref::<Error>() {
        return Some(fleet_error(e));
    }
    if let Some(e) = cause.downcast_ref::<DocError>() {
        return Some(doc_error(e));
    }
    if let Some(e) = cause.downcast_ref::<S3Error>() {
        return Some(s3_error(e));
    }
    if let Some(e) = cause.downcast_ref::<KeyscanError>() {
        return Some(match e {
            KeyscanError::InvalidHost(_) => Exit::Usage,
            KeyscanError::Spawn(_)
            | KeyscanError::Timeout { .. }
            | KeyscanError::Resolution { .. }
            | KeyscanError::ResolverFailure { .. }
            | KeyscanError::NoKeys { .. }
            | KeyscanError::Malformed { .. } => Exit::Error,
        });
    }
    if cause
        .downcast_ref::<SelectorError>()
        .is_some()
    {
        return Some(Exit::Usage);
    }
    if cause
        .downcast_ref::<DiscoverError>()
        .is_some()
    {
        return Some(Exit::Error);
    }
    if let Some(e) = cause.downcast_ref::<ConfigError>() {
        return Some(match e {
            ConfigError::Usage(_) => Exit::Usage,
            _ => Exit::Error,
        });
    }
    None
}

fn fleet_error(e: &Error) -> Exit {
    match e {
        Error::InvalidFleetName { .. } | Error::InvalidBucketName { .. } => Exit::Usage,
        Error::NotFound { .. } => Exit::NotFound,
        Error::Forbidden { source, .. } => s3_error(source),
        Error::Conflict { .. } => Exit::Conflict,
        Error::CredentialsRequired { .. } => Exit::Credentials,
        Error::Unreachable { .. } => Exit::Unreachable,
        Error::Change(doc) => doc_error(doc),
        Error::S3 { .. } | Error::Document { .. } | Error::MissingEtag { .. } => Exit::Error,
    }
}

fn s3_error(e: &S3Error) -> Exit {
    match e {
        S3Error::Forbidden {
            anonymous: true, ..
        }
        | S3Error::NotFound { .. } => Exit::NotFound,
        S3Error::Forbidden { .. } | S3Error::CredentialsRequired { .. } => Exit::Credentials,
        S3Error::PreconditionFailed { .. } => Exit::Conflict,
        S3Error::Transport { .. } => Exit::Unreachable,
        S3Error::Client(_)
        | S3Error::Signing { .. }
        | S3Error::Malformed { .. }
        | S3Error::BucketOwnedByOther { .. }
        | S3Error::Status { .. } => Exit::Error,
    }
}

fn doc_error(e: &DocError) -> Exit {
    match e {
        DocError::KeyMismatch { .. } => Exit::KeyMismatch,
        DocError::InvalidHostName { .. }
        | DocError::InvalidKeyName { .. }
        | DocError::InvalidLabelValue { .. } => Exit::Usage,
        DocError::Yaml(_)
        | DocError::Serialize(_)
        | DocError::UnsupportedVersion { .. }
        | DocError::DuplicateHost { .. }
        | DocError::NoKeys { .. }
        | DocError::InvalidKey { .. }
        | DocError::HostNotFound { .. } => Exit::Error,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::collections::HashSet;

    #[test]
    fn codes_are_distinct_and_dense() {
        let codes: HashSet<i32> = Exit::ALL
            .iter()
            .map(|e| *e as i32)
            .collect();
        assert_eq!(codes.len(), Exit::ALL.len());
        assert_eq!(codes, (0..Exit::ALL.len() as i32).collect());
    }

    fn fleet(e: Error) -> Exit {
        exit_code(&anyhow::Error::new(e).context("outer context"))
    }

    fn forbidden(anonymous: bool) -> S3Error {
        S3Error::Forbidden {
            method: reqwest::Method::GET,
            url: "u".into(),
            anonymous,
            code: None,
            body: String::new(),
            request_id: None,
        }
    }

    #[test]
    fn fleet_errors_map_through_context() {
        let f = || "f".to_string();
        assert_eq!(fleet(Error::Conflict { fleet: f() }), Exit::Conflict);
        assert_eq!(
            fleet(Error::CredentialsRequired { fleet: f() }),
            Exit::Credentials
        );
        assert_eq!(fleet(Error::InvalidFleetName { name: f() }), Exit::Usage);
        assert_eq!(
            fleet(Error::Forbidden {
                fleet: f(),
                source: Box::new(forbidden(true)),
            }),
            Exit::NotFound,
            "anonymous 403 may be a missing fleet"
        );
        assert_eq!(
            fleet(Error::Forbidden {
                fleet: f(),
                source: Box::new(forbidden(false)),
            }),
            Exit::Credentials,
            "signed 403 means the credentials were refused"
        );
        assert_eq!(
            fleet(Error::Change(DocError::KeyMismatch {
                host: f(),
                pinned: vec![],
                scanned: vec![],
            })),
            Exit::KeyMismatch
        );
        assert_eq!(fleet(Error::MissingEtag { fleet: f() }), Exit::Error);
    }

    #[test]
    fn failure_carries_its_code() {
        let err = anyhow::Error::new(Failure::new(Exit::Partial, "x")).context("y");
        assert_eq!(exit_code(&err), Exit::Partial);
        assert_eq!(exit_code(&anyhow::anyhow!("plain")), Exit::Error);
    }
}
