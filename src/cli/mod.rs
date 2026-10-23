//! Command definitions, configuration layering, and dispatch.
//!
//! Settings resolve per field, lowest to highest: DNS discovery,
//! `/etc/s3-fleet.yaml`, `$XDG_CONFIG_HOME/s3-fleet.yaml`, environment, flags.
//! `--config PATH` replaces both files with that one, so a run can be isolated
//! from the machine's. The endpoint and region resolve as a pair from the
//! highest layer that sets an endpoint, so a file's region is never signed
//! against DNS's endpoint.
//!
//! Credentials resolve first-wins by whole keypair: the AWS environment pair,
//! then `credentials.<fleet>`, then `credentials.default`, from the highest
//! file down. A file that holds credentials must not be group-writable or
//! readable by others; it is refused, not warned about.
