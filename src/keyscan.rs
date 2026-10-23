//! Fetching a host's public keys with OpenSSH's `ssh-keyscan`.
//!
//! Runs `ssh-keyscan -T <secs> -t <types> [-p <port>] <host>` under a
//! wall-clock timeout of its own: `-T` bounds each connection, not the run,
//! and a host that resolves slowly can hold ssh-keyscan well past it. The
//! child is killed when the timeout fires or the future is dropped.
//!
//! Only ed25519 is scanned unless the caller asks for more. Each key type
//! pinned is one more key an attacker could try to match, and every OpenSSH
//! that s3-fleet targets offers ed25519.
//!
//! A failure is classified, so a caller can act on it without parsing text: a
//! name that does not exist usually means a decommissioned host, which is not
//! the same as a resolver that could not answer, a timeout, or a host that
//! returned no key of the requested type. The stderr text is kept in every
//! error. Resolution failures are read from ssh-keyscan's stderr by
//! [`crate::lookup`], which says why that is the only signal and how it is
//! classified.

use std::process::Stdio;
use std::time::Duration;

use tokio::process::Command;

use crate::doc::{DocError, validate_host_name};
use crate::known_hosts::{self, DEFAULT_PORT};
use crate::lookup::{LookupFailure, failed_lookup};

/// How to scan.
#[derive(Clone, Debug)]
pub struct KeyscanOptions {
    /// SSH port.
    pub port: u16,
    /// Key types, as ssh-keyscan's `-t` names them (`ed25519`, `rsa`, ...).
    pub key_types: Vec<String>,
    /// Per-connection timeout, ssh-keyscan's `-T`; at least one second.
    pub connect_timeout: Duration,
    /// Wall-clock limit for the whole run, after which the child is killed.
    pub timeout: Duration,
}

impl Default for KeyscanOptions {
    fn default() -> Self {
        Self {
            port: DEFAULT_PORT,
            key_types: vec!["ed25519".to_string()],
            connect_timeout: Duration::from_secs(5),
            timeout: Duration::from_secs(15),
        }
    }
}

/// Why a host's keys could not be fetched.
#[derive(Debug, thiserror::Error)]
pub enum KeyscanError {
    /// The host name would not be safe to pass to ssh-keyscan.
    #[error(transparent)]
    InvalidHost(#[from] DocError),
    /// ssh-keyscan could not be run (not installed?).
    #[error("run ssh-keyscan")]
    Spawn(#[source] std::io::Error),
    /// The wall-clock limit passed; the child was killed.
    #[error("{host}: ssh-keyscan did not finish within {after:?}")]
    Timeout {
        /// The host.
        host: String,
        /// The limit.
        after: Duration,
    },
    /// The resolver answered that the name has no address; often a
    /// decommissioned host.
    #[error("{host}: does not resolve: {stderr}")]
    Resolution {
        /// The host.
        host: String,
        /// ssh-keyscan's stderr.
        stderr: String,
    },
    /// The lookup failed without saying the name is absent: a resolver that
    /// did not answer (EAI_AGAIN), a hard resolver error, or a reason not
    /// recognised. Says nothing about the host.
    #[error("{host}: name lookup failed (resolver; says nothing about the host): {stderr}")]
    ResolverFailure {
        /// The host.
        host: String,
        /// ssh-keyscan's stderr.
        stderr: String,
    },
    /// No key of the requested types came back. ssh-keyscan reports a
    /// refused or unreachable port with no message at all.
    #[error(
        "{host}: no {types} key returned (unreachable, refused, or no such key type){}",
        stderr_suffix(stderr)
    )]
    NoKeys {
        /// The host.
        host: String,
        /// The requested types, comma-separated.
        types: String,
        /// ssh-keyscan's stderr, often empty.
        stderr: String,
    },
    /// A line of output that is not `<host> <type> <base64>`.
    #[error("{host}: unexpected ssh-keyscan output {line:?}")]
    Malformed {
        /// The host.
        host: String,
        /// The line.
        line: String,
    },
}

fn stderr_suffix(stderr: &str) -> String {
    if stderr.is_empty() {
        String::new()
    } else {
        format!(": {stderr}")
    }
}

/// Fetch a host's keys, each as `<type> <base64>`, sorted.
pub async fn keyscan(host: &str, opts: &KeyscanOptions) -> Result<Vec<String>, KeyscanError> {
    validate_host_name(host)?;
    let types = opts
        .key_types
        .join(",");
    let connect = opts
        .connect_timeout
        .as_secs()
        .max(1);
    let child = Command::new("ssh-keyscan")
        .arg("-T")
        .arg(connect.to_string())
        .arg("-t")
        .arg(&types)
        .arg("-p")
        .arg(
            opts.port
                .to_string(),
        )
        .arg("--")
        .arg(host)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(KeyscanError::Spawn)?;
    let output = tokio::time::timeout(opts.timeout, child.wait_with_output())
        .await
        .map_err(|_| KeyscanError::Timeout {
            host: host.to_string(),
            after: opts.timeout,
        })?
        .map_err(KeyscanError::Spawn)?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr)
        .trim()
        .to_string();
    let keys = parse_output(host, &stdout)?;
    if keys.is_empty() {
        return Err(classify_failure(host, types, stderr));
    }
    Ok(keys)
}

/// The error for a scan that returned no key, from ssh-keyscan's stderr.
fn classify_failure(host: &str, types: String, stderr: String) -> KeyscanError {
    let host = host.to_string();
    match failed_lookup(&stderr).map(|f| f.failure) {
        Some(LookupFailure::NameAbsent) => KeyscanError::Resolution { host, stderr },
        Some(LookupFailure::Other) => KeyscanError::ResolverFailure { host, stderr },
        None => KeyscanError::NoKeys {
            host,
            types,
            stderr,
        },
    }
}

/// Keys from ssh-keyscan's stdout: the host pattern dropped, comments and
/// blank lines skipped, every key checked to decode.
fn parse_output(host: &str, stdout: &str) -> Result<Vec<String>, KeyscanError> {
    let mut keys = Vec::new();
    for line in stdout.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let malformed = || KeyscanError::Malformed {
            host: host.to_string(),
            line: line.to_string(),
        };
        let fields: Vec<&str> = line
            .split_whitespace()
            .collect();
        let [_, key_type, blob] = fields[..] else {
            return Err(malformed());
        };
        let key = format!("{key_type} {blob}");
        known_hosts::fingerprint(&key).map_err(|_| malformed())?;
        keys.push(key);
    }
    keys.sort();
    keys.dedup();
    Ok(keys)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkeys::{KEY_A, KEY_B};

    #[test]
    fn parse_drops_pattern_and_comments() {
        let out =
            format!("# h:22 SSH-2.0-OpenSSH_10.0\nh {KEY_B}\n\n[h]:2222 {KEY_A}\nh {KEY_B}\n");
        let mut expected = [KEY_A, KEY_B];
        expected.sort();
        assert_eq!(parse_output("h", &out).unwrap(), expected);
    }

    #[test]
    fn parse_rejects_garbage() {
        assert!(matches!(
            parse_output("h", "h ssh-ed25519\n"),
            Err(KeyscanError::Malformed { .. })
        ));
        assert!(matches!(
            parse_output("h", "h ssh-ed25519 ***\n"),
            Err(KeyscanError::Malformed { .. })
        ));
    }

    fn classify(stderr: &str) -> KeyscanError {
        classify_failure("h", "ed25519".to_string(), stderr.to_string())
    }

    #[test]
    fn lookup_verdicts_map_to_variants() {
        assert!(matches!(
            classify("getaddrinfo h: Name or service not known"),
            KeyscanError::Resolution { .. }
        ));
        assert!(matches!(
            classify("getaddrinfo h: Try again"),
            KeyscanError::ResolverFailure { .. }
        ));
        assert!(matches!(
            classify("write (h): Connection reset by peer"),
            KeyscanError::NoKeys { .. }
        ));
    }

    #[tokio::test]
    async fn option_like_host_is_refused_before_running() {
        let err = keyscan("-oProxyCommand=x", &KeyscanOptions::default()).await;
        assert!(matches!(err, Err(KeyscanError::InvalidHost(_))), "{err:?}");
    }
}
