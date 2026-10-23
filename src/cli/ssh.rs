//! `s3-fleet ssh`: run the system ssh against a fleet host, trusting only the
//! keys pinned in the fleet.
//!
//! ssh gets the keys from `KnownHostsCommand`, which calls this binary back as
//! `known-hosts --cached --host %H`, with `UserKnownHostsFile` and
//! `GlobalKnownHostsFile` set to `/dev/null` and `StrictHostKeyChecking=yes`.
//! A known_hosts file passed as a pipe does not work: ssh closes inherited
//! descriptors before reading it.
//!
//! The callback reads only the local cache. ssh may run it up to three times
//! per connection, and a network fetch each time would add seconds and three
//! chances to fail. This command refreshes the cache with one GET before
//! exec'ing ssh, and fails if S3 cannot be reached unless `--allow-stale`.
//!
//! ssh runs `KnownHostsCommand` through the user's shell, so the path to this
//! executable is shell-quoted. The fleet name needs no quoting: the fleet-name
//! rule in `s3_fleet::fleet` leaves only `[a-z0-9-]`.
//!
//! The process is replaced with `exec`, so ssh owns the TTY and its exit
//! status is the caller's.
