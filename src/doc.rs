//! The fleet document: the one object, `fleet.yaml`, stored in a fleet's bucket.
//!
//! ```yaml
//! version: 1
//! attributes:
//!   ssh_timeout_secs: "3"
//! hosts:
//! - name: c1pic-sbc1.example.net
//!   port: 2222
//!   keys:
//!   - ssh-ed25519 AAAA...
//!   labels:
//!     env: prod
//!     role: sbc
//! ```
//!
//! This is a wire format: every s3-fleet version and every other reader parse
//! it. Changing it means a new `version`.
//!
//! # Invariants
//!
//! - Unknown fields and unknown versions are parse errors. A writer that
//!   tolerated them would drop them on its next commit and destroy data that
//!   a newer writer put there.
//! - Hosts are sorted by name, unique, and attribute and label maps are
//!   BTreeMaps, so the serialised document is byte-stable and a diff of two
//!   versions shows only what changed.
//! - `port` is omitted when it is 22. `keys` hold known_hosts key fields
//!   (`<type> <base64>`), with no host pattern: the pattern is derived from
//!   `name` and `port` when rendering, so it cannot disagree with them.
//! - Label and attribute keys match `[A-Za-z0-9._/-]+`. Label values may hold
//!   any UTF-8 except `,` and line breaks, which keeps the selector grammar in
//!   `crate::selector` unambiguous.
//!
//! Mutations are pure methods on `FleetDoc` and return typed errors, so
//! every rule above is tested without S3.
