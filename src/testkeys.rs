//! Throwaway ed25519 public keys for tests, with the fingerprints
//! `ssh-keygen -lf` printed for them. Their private halves were discarded.

pub const KEY_A: &str =
    "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIKnbPxkZs+chN1S99bZrRjz+EyixQKfaei4NKQCzWyaQ";
pub const FINGERPRINT_A: &str = "SHA256:97Bzb0cusN9vBlueyG+m2r6ttbDCkfiQY1hMWUSZJt8";
pub const KEY_B: &str =
    "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIBmcY94uJ8nfRLVFBp/kPsyfZq54fZCLKJk8GCwvUlCa";
pub const FINGERPRINT_B: &str = "SHA256:D0PDqGgHPKMDnfCFygDhZKx/TLQYdS7cXB2WmQI9jRo";
