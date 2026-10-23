//! Rendering pinned keys as OpenSSH known_hosts lines, and fingerprints.
//!
//! A host on port 22 renders as `name keytype base64`; any other port as
//! `[name]:port keytype base64`, the only form ssh matches for a non-default
//! port. Names are never hashed: the file is generated, and a hashed name
//! cannot be matched back to the fleet.
//!
//! An IPv6 literal is a name like any other. It is written bare on port 22
//! and inside the brackets otherwise, and nothing here splits on `:`.
//!
//! Fingerprints use OpenSSH's format (`SHA256:` and unpadded base64 of the
//! decoded key blob), so a key-mismatch error can be compared with what
//! `ssh-keygen -lf` or an ssh warning prints.
