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

use base64::Engine;
use base64::engine::general_purpose::{STANDARD, STANDARD_NO_PAD};
use sha2::{Digest, Sha256};

use crate::doc::Host;

/// The port ssh assumes when none is given.
pub const DEFAULT_PORT: u16 = 22;

/// A key field that is not `<type> <base64>`.
#[derive(Debug, thiserror::Error)]
pub enum KeyError {
    /// Not exactly two space-separated fields.
    #[error("expected `<type> <base64>`, got {fields} field(s)")]
    Shape {
        /// How many fields were found.
        fields: usize,
    },
    /// The second field is not base64.
    #[error("key blob is not base64")]
    Base64(#[source] base64::DecodeError),
}

/// The known_hosts host pattern for a host: `name`, or `[name]:port`.
pub fn pattern(name: &str, port: u16) -> String {
    if port == DEFAULT_PORT {
        name.to_string()
    } else {
        format!("[{name}]:{port}")
    }
}

/// known_hosts lines for these hosts, one per pinned key.
pub fn render<'a>(hosts: impl IntoIterator<Item = &'a Host>) -> String {
    let mut out = String::new();
    for host in hosts {
        let pattern = pattern(host.name(), host.port());
        for key in host.keys() {
            out.push_str(&pattern);
            out.push(' ');
            out.push_str(key);
            out.push('\n');
        }
    }
    out
}

/// OpenSSH's SHA256 fingerprint of a `<type> <base64>` key field.
pub fn fingerprint(key: &str) -> Result<String, KeyError> {
    let fields: Vec<&str> = key
        .split(' ')
        .collect();
    let [_, blob] = fields[..] else {
        return Err(KeyError::Shape {
            fields: fields.len(),
        });
    };
    let blob = STANDARD
        .decode(blob)
        .map_err(KeyError::Base64)?;
    Ok(format!(
        "SHA256:{}",
        STANDARD_NO_PAD.encode(Sha256::digest(&blob))
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::doc::FleetDoc;
    use crate::testkeys::{FINGERPRINT_A, FINGERPRINT_B, KEY_A, KEY_B};

    #[test]
    fn fingerprint_matches_ssh_keygen() {
        assert_eq!(fingerprint(KEY_A).unwrap(), FINGERPRINT_A);
        assert_eq!(fingerprint(KEY_B).unwrap(), FINGERPRINT_B);
    }

    #[test]
    fn fingerprint_rejects_malformed_keys() {
        assert!(matches!(
            fingerprint("ssh-ed25519"),
            Err(KeyError::Shape { fields: 1 })
        ));
        assert!(matches!(
            fingerprint("host ssh-ed25519 AAAA"),
            Err(KeyError::Shape { fields: 3 })
        ));
        assert!(matches!(
            fingerprint("ssh-ed25519 not*base64"),
            Err(KeyError::Base64(_))
        ));
    }

    #[test]
    fn pattern_brackets_only_non_default_ports() {
        assert_eq!(pattern("h.example", 22), "h.example");
        assert_eq!(pattern("h.example", 2222), "[h.example]:2222");
        assert_eq!(pattern("2001:db8::1", 22), "2001:db8::1");
        assert_eq!(pattern("2001:db8::1", 2222), "[2001:db8::1]:2222");
    }

    #[test]
    fn render_writes_one_line_per_key() {
        let mut doc = FleetDoc::new();
        doc.pin_host("b.example", 2222, vec![KEY_B.to_string()], false)
            .unwrap();
        doc.pin_host("a.example", 22, vec![KEY_A.to_string()], false)
            .unwrap();
        assert_eq!(
            render(doc.hosts()),
            format!("a.example {KEY_A}\n[b.example]:2222 {KEY_B}\n")
        );
    }
}
