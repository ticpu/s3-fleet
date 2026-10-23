//! A handle on one fleet: its bucket and the S3 client that reaches it.
//!
//! `Fleet::load` reads `fleet.yaml` and returns it with its ETag;
//! `Fleet::commit` writes a changed document back with `If-Match` on that
//! ETag, and `Fleet::init` creates the bucket and writes an empty document
//! with `If-None-Match: *`. Every write is conditional: an unconditional PUT
//! would let two concurrent editors each overwrite the other's change without
//! a trace.
//!
//! A `Snapshot` can be committed once. After a commit, or a conflict, load
//! again: the ETag it carries is stale.
//!
//! The fleet name is the bucket name, restricted to 3 to 63 characters of
//! `[a-z0-9-]`, starting and ending with a letter or digit: a valid bucket name
//! on every S3 implementation, with no dot to break TLS on virtual-hosted
//! endpoints, and safe to put in a shell command unquoted (the ssh wrapper
//! relies on that). It is checked before any request, so a typo fails here and
//! not as an S3 error about some bucket.
