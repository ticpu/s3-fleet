//! A local copy of each fleet's document, for reads that must not touch the
//! network or that the caller allows to be stale.
//!
//! The copy lives at `$XDG_CACHE_HOME/s3-fleet/<fleet>.yaml`. Nothing in the
//! library reads it on its own: the CLI stores every document it loads, and
//! reads the copy only for `--allow-stale`, `known-hosts --cached` (which ssh
//! calls up to three times per connection) and shell completion. A
//! `Cached` result always carries its age.
//!
//! `store` rewrites the file only when the content changed, since the cache
//! directory may sit on a slow network home. When the content is unchanged it
//! still bumps the mtime,
//! because the age reported is "last confirmed against S3", not "last time the
//! fleet changed"; otherwise an untouched fleet makes a current copy look
//! months old. A changed file is written to a sibling temp file and renamed
//! into place, so a concurrent reader (ssh's KnownHostsCommand) never sees half
//! a document.
