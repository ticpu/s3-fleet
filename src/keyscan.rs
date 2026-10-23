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
//! name that does not resolve usually means a decommissioned host, which is
//! not the same as a timeout or a host that returned no key of the requested
//! type. Resolution failures are recognised by ssh-keyscan's stderr, which is
//! the only signal it gives; the stderr text is kept in every error.
