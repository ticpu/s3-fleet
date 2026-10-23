//! Finding a fleet's S3 endpoint and region from DNS.
//!
//! The record is a TXT record:
//!
//! ```text
//! _s3-fleet.prod-sbc  IN TXT "v=s3-fleet1 endpoint=s3.example.net region=default"
//! _s3-fleet           IN TXT "v=s3-fleet1 endpoint=s3.example.net region=default"
//! ```
//!
//! For fleet `<f>`, `_s3-fleet.<f>` is queried first, then `_s3-fleet`. The
//! per-fleet name lets fleets live on different clusters; the bare one serves
//! every other fleet. Both names are relative: the system resolver applies
//! its search list, so a box in `example.net` finds `_s3-fleet.example.net`
//! with no configuration, and the binary carries no site's domain.
//!
//! The text is space-separated `key=value` pairs; `v=s3-fleet1` must come
//! first. An unknown version, a missing or repeated key, or two records
//! answering the same name is an error, never a guess between them. Unknown
//! keys are ignored so that a later version can add some without breaking
//! readers, which is the opposite of the fleet document's rule: a TXT record
//! is never written back.
//!
//! Discovery is the lowest layer. A flag, an environment variable or a config
//! file that names an endpoint wins, and is not combined with a discovered
//! region.
