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
//!   [`crate::selector`] unambiguous.
//! - Host names use `[A-Za-z0-9._:-]` and never start with `-`: they reach
//!   ssh and ssh-keyscan as arguments, where a leading dash is an option, and
//!   known_hosts as a pattern, where `*?!,` are syntax.
//! - Re-pinning a host with any different set of keys, an added key type
//!   included, is a [`DocError::KeyMismatch`] unless the caller passes
//!   `rekey`. A refused pin changes nothing, not even the port.
//!
//! Mutations are pure methods on [`FleetDoc`] and return typed errors, so
//! every rule above is tested without S3.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::known_hosts::{self, DEFAULT_PORT, KeyError};
use crate::selector::Selector;

/// The only document version this crate reads and writes.
pub const VERSION: u32 = 1;

/// A rule of the fleet document broken by a parse or a mutation.
#[derive(Debug, thiserror::Error)]
pub enum DocError {
    /// Not YAML, or not the document's shape (unknown fields included).
    #[error("parse fleet document")]
    Yaml(#[source] serde_norway::Error),
    /// Serialising failed; not expected for a valid document.
    #[error("serialise fleet document")]
    Serialize(#[source] serde_norway::Error),
    /// A version this crate does not know.
    #[error("fleet document version {found} is not supported (this s3-fleet reads {VERSION})")]
    UnsupportedVersion {
        /// The version found.
        found: u32,
    },
    /// Two hosts with the same name.
    #[error("host {name} appears twice")]
    DuplicateHost {
        /// The repeated name.
        name: String,
    },
    /// A host name ssh or ssh-keyscan could misread.
    #[error("invalid host name {name:?}: {reason}")]
    InvalidHostName {
        /// The name.
        name: String,
        /// What is wrong with it.
        reason: &'static str,
    },
    /// A host with no keys.
    #[error("host {name} has no keys")]
    NoKeys {
        /// The host.
        name: String,
    },
    /// A pinned key that is not `<type> <base64>`.
    #[error("host {host}: invalid key {key:?}")]
    InvalidKey {
        /// The host.
        host: String,
        /// The key field.
        key: String,
        /// Why it was refused.
        #[source]
        source: KeyError,
    },
    /// A label or attribute key outside `[A-Za-z0-9._/-]+`.
    #[error("invalid key {key:?}: only A-Z a-z 0-9 . _ / - are allowed")]
    InvalidKeyName {
        /// The key.
        key: String,
    },
    /// A label value holding `,` or a line break.
    #[error("invalid value for label {key}: commas and line breaks are not allowed")]
    InvalidLabelValue {
        /// The label key.
        key: String,
    },
    /// A host that is not in the fleet.
    #[error("host {name} is not in the fleet")]
    HostNotFound {
        /// The host.
        name: String,
    },
    /// Scanned keys differ from the pinned ones.
    #[error(
        "host {host}: scanned keys [{}] differ from pinned keys [{}]",
        scanned.join(", "),
        pinned.join(", ")
    )]
    KeyMismatch {
        /// The host.
        host: String,
        /// Fingerprints of the pinned keys.
        pinned: Vec<String>,
        /// Fingerprints of the scanned keys.
        scanned: Vec<String>,
    },
}

/// The fleet: hosts sorted by name, and fleet-wide attributes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FleetDoc {
    version: u32,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    attributes: BTreeMap<String, String>,
    #[serde(default)]
    hosts: Vec<Host>,
}

/// One host: where to reach it, its pinned keys, its labels.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Host {
    name: String,
    #[serde(default = "default_port", skip_serializing_if = "is_default_port")]
    port: u16,
    keys: Vec<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    labels: BTreeMap<String, String>,
}

fn default_port() -> u16 {
    DEFAULT_PORT
}

fn is_default_port(port: &u16) -> bool {
    *port == DEFAULT_PORT
}

/// What [`FleetDoc::pin_host`] did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pinned {
    /// The host was not in the fleet.
    Added,
    /// The host was there with the same keys; its port may have changed.
    Unchanged,
    /// The host's keys were replaced (`rekey`).
    Replaced,
}

impl Host {
    /// The host name, as given to ssh.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The SSH port.
    pub fn port(&self) -> u16 {
        self.port
    }

    /// Pinned keys, each `<type> <base64>`.
    pub fn keys(&self) -> &[String] {
        &self.keys
    }

    /// Labels.
    pub fn labels(&self) -> &BTreeMap<String, String> {
        &self.labels
    }

    fn fingerprints(&self) -> Vec<String> {
        fingerprints(&self.keys)
    }
}

impl Default for FleetDoc {
    fn default() -> Self {
        Self::new()
    }
}

impl FleetDoc {
    /// An empty fleet.
    pub fn new() -> Self {
        Self {
            version: VERSION,
            attributes: BTreeMap::new(),
            hosts: Vec::new(),
        }
    }

    /// Parse and validate a document. Hosts out of order are sorted; every
    /// other broken rule is an error.
    pub fn parse(bytes: &[u8]) -> Result<Self, DocError> {
        let mut doc: Self = serde_norway::from_slice(bytes).map_err(DocError::Yaml)?;
        if doc.version != VERSION {
            return Err(DocError::UnsupportedVersion { found: doc.version });
        }
        doc.hosts
            .sort_by(|a, b| {
                a.name
                    .cmp(&b.name)
            });
        for pair in doc
            .hosts
            .windows(2)
        {
            if pair[0].name == pair[1].name {
                return Err(DocError::DuplicateHost {
                    name: pair[0]
                        .name
                        .clone(),
                });
            }
        }
        for host in &doc.hosts {
            validate_host_name(&host.name)?;
            validate_keys(&host.name, &host.keys)?;
            for (key, value) in &host.labels {
                validate_label(key, value)?;
            }
        }
        for key in doc
            .attributes
            .keys()
        {
            validate_key_name(key)?;
        }
        Ok(doc)
    }

    /// The document as stored.
    pub fn to_yaml(&self) -> Result<String, DocError> {
        serde_norway::to_string(self).map_err(DocError::Serialize)
    }

    /// Hosts, sorted by name.
    pub fn hosts(&self) -> &[Host] {
        &self.hosts
    }

    /// One host by name.
    pub fn host(&self, name: &str) -> Option<&Host> {
        self.index(name)
            .ok()
            .map(|i| &self.hosts[i])
    }

    /// Hosts matching every selector; all hosts for none.
    pub fn select<'a>(&'a self, selectors: &'a [Selector]) -> impl Iterator<Item = &'a Host> {
        self.hosts
            .iter()
            .filter(|h| {
                selectors
                    .iter()
                    .all(|s| s.matches(&h.labels))
            })
    }

    /// Fleet-wide attributes.
    pub fn attributes(&self) -> &BTreeMap<String, String> {
        &self.attributes
    }

    /// Pin `keys` for a host, adding it if absent. A host already pinned
    /// with a different set of keys is refused with
    /// [`DocError::KeyMismatch`] unless `rekey` is set.
    pub fn pin_host(
        &mut self,
        name: &str,
        port: u16,
        keys: Vec<String>,
        rekey: bool,
    ) -> Result<Pinned, DocError> {
        validate_host_name(name)?;
        validate_keys(name, &keys)?;
        let mut keys = keys;
        keys.sort();
        keys.dedup();
        match self.index(name) {
            Err(at) => {
                self.hosts
                    .insert(
                        at,
                        Host {
                            name: name.to_string(),
                            port,
                            keys,
                            labels: BTreeMap::new(),
                        },
                    );
                Ok(Pinned::Added)
            }
            Ok(i) => {
                let host = &mut self.hosts[i];
                let mut pinned = host
                    .keys
                    .clone();
                pinned.sort();
                let outcome = if pinned == keys {
                    Pinned::Unchanged
                } else if rekey {
                    Pinned::Replaced
                } else {
                    return Err(DocError::KeyMismatch {
                        host: name.to_string(),
                        pinned: host.fingerprints(),
                        scanned: fingerprints(&keys),
                    });
                };
                host.port = port;
                host.keys = keys;
                Ok(outcome)
            }
        }
    }

    /// Remove a host; `false` if it was not there.
    pub fn remove_host(&mut self, name: &str) -> bool {
        match self.index(name) {
            Ok(i) => {
                self.hosts
                    .remove(i);
                true
            }
            Err(_) => false,
        }
    }

    /// Set a label on a host.
    pub fn set_label(&mut self, host: &str, key: &str, value: &str) -> Result<(), DocError> {
        validate_label(key, value)?;
        self.host_mut(host)?
            .labels
            .insert(key.to_string(), value.to_string());
        Ok(())
    }

    /// Remove a label from a host; `Ok(false)` if it was not set.
    pub fn unset_label(&mut self, host: &str, key: &str) -> Result<bool, DocError> {
        Ok(self
            .host_mut(host)?
            .labels
            .remove(key)
            .is_some())
    }

    /// Set a fleet-wide attribute.
    pub fn set_attribute(&mut self, key: &str, value: &str) -> Result<(), DocError> {
        validate_key_name(key)?;
        self.attributes
            .insert(key.to_string(), value.to_string());
        Ok(())
    }

    /// Remove a fleet-wide attribute; `false` if it was not set.
    pub fn unset_attribute(&mut self, key: &str) -> bool {
        self.attributes
            .remove(key)
            .is_some()
    }

    fn index(&self, name: &str) -> Result<usize, usize> {
        self.hosts
            .binary_search_by(|h| {
                h.name
                    .as_str()
                    .cmp(name)
            })
    }

    fn host_mut(&mut self, name: &str) -> Result<&mut Host, DocError> {
        match self.index(name) {
            Ok(i) => Ok(&mut self.hosts[i]),
            Err(_) => Err(DocError::HostNotFound {
                name: name.to_string(),
            }),
        }
    }
}

fn fingerprints(keys: &[String]) -> Vec<String> {
    keys.iter()
        .map(|k| known_hosts::fingerprint(k).unwrap_or_else(|_| k.clone()))
        .collect()
}

/// Host names reach ssh and ssh-keyscan as arguments and known_hosts as a
/// pattern: a leading `-` would be read as an option, and `*?!,[]` or
/// whitespace as pattern syntax.
pub fn validate_host_name(name: &str) -> Result<(), DocError> {
    let invalid = |reason| DocError::InvalidHostName {
        name: name.to_string(),
        reason,
    };
    if name.is_empty() {
        return Err(invalid("empty"));
    }
    if name.starts_with('-') {
        return Err(invalid("starts with '-'"));
    }
    if !name
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_' | b':'))
    {
        return Err(invalid("only A-Z a-z 0-9 . - _ : are allowed"));
    }
    Ok(())
}

fn validate_keys(host: &str, keys: &[String]) -> Result<(), DocError> {
    if keys.is_empty() {
        return Err(DocError::NoKeys {
            name: host.to_string(),
        });
    }
    for key in keys {
        known_hosts::fingerprint(key).map_err(|source| DocError::InvalidKey {
            host: host.to_string(),
            key: key.clone(),
            source,
        })?;
    }
    Ok(())
}

/// Label and attribute keys.
pub fn validate_key_name(key: &str) -> Result<(), DocError> {
    let valid = !key.is_empty()
        && key
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'/' | b'-'));
    if !valid {
        return Err(DocError::InvalidKeyName {
            key: key.to_string(),
        });
    }
    Ok(())
}

/// A label: a valid key, and a value with no comma or line break.
pub fn validate_label(key: &str, value: &str) -> Result<(), DocError> {
    validate_key_name(key)?;
    if value.contains([',', '\n', '\r']) {
        return Err(DocError::InvalidLabelValue {
            key: key.to_string(),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkeys::{FINGERPRINT_A, FINGERPRINT_B, KEY_A, KEY_B};

    fn sample() -> FleetDoc {
        let mut doc = FleetDoc::new();
        doc.pin_host("sbc1.example", 22, vec![KEY_A.to_string()], false)
            .unwrap();
        doc.pin_host("2001:db8::1", 2222, vec![KEY_B.to_string()], false)
            .unwrap();
        doc.set_label("sbc1.example", "env", "prod")
            .unwrap();
        doc.set_label("sbc1.example", "role", "sbc")
            .unwrap();
        doc.set_attribute("ssh_timeout_secs", "3")
            .unwrap();
        doc
    }

    #[test]
    fn round_trip_keeps_every_field() {
        let doc = sample();
        let yaml = doc
            .to_yaml()
            .unwrap();
        assert_eq!(FleetDoc::parse(yaml.as_bytes()).unwrap(), doc);
        assert_eq!(
            yaml,
            format!(
                "version: 1\n\
                 attributes:\n  ssh_timeout_secs: '3'\n\
                 hosts:\n\
                 - name: 2001:db8::1\n  port: 2222\n  keys:\n  - {KEY_B}\n\
                 - name: sbc1.example\n  keys:\n  - {KEY_A}\n  labels:\n    env: prod\n    role: sbc\n"
            )
        );
    }

    #[test]
    fn unknown_fields_and_versions_are_refused() {
        let base = format!("version: 1\nhosts:\n- name: h\n  keys:\n  - {KEY_A}\n");
        assert!(FleetDoc::parse(base.as_bytes()).is_ok());
        let top = format!("{base}future: x\n");
        assert!(matches!(
            FleetDoc::parse(top.as_bytes()),
            Err(DocError::Yaml(_))
        ));
        let in_host = format!("version: 1\nhosts:\n- name: h\n  owner: x\n  keys:\n  - {KEY_A}\n");
        assert!(matches!(
            FleetDoc::parse(in_host.as_bytes()),
            Err(DocError::Yaml(_))
        ));
        assert!(matches!(
            FleetDoc::parse(b"version: 2\n"),
            Err(DocError::UnsupportedVersion { found: 2 })
        ));
        assert!(matches!(
            FleetDoc::parse(b"hosts: []\n"),
            Err(DocError::Yaml(_))
        ));
    }

    #[test]
    fn parse_sorts_hosts_and_refuses_duplicates() {
        let unsorted = format!(
            "version: 1\nhosts:\n- name: b\n  keys:\n  - {KEY_A}\n- name: a\n  keys:\n  - {KEY_B}\n"
        );
        let doc = FleetDoc::parse(unsorted.as_bytes()).unwrap();
        let names: Vec<&str> = doc
            .hosts()
            .iter()
            .map(Host::name)
            .collect();
        assert_eq!(names, ["a", "b"]);
        let dup = format!(
            "version: 1\nhosts:\n- name: a\n  keys:\n  - {KEY_A}\n- name: a\n  keys:\n  - {KEY_B}\n"
        );
        assert!(matches!(
            FleetDoc::parse(dup.as_bytes()),
            Err(DocError::DuplicateHost { name }) if name == "a"
        ));
    }

    #[test]
    fn parse_validates_names_keys_and_labels() {
        for (yaml, what) in [
            (
                format!("version: 1\nhosts:\n- name: -oProxyCommand=x\n  keys:\n  - {KEY_A}\n"),
                "dash",
            ),
            (
                format!("version: 1\nhosts:\n- name: 'h*'\n  keys:\n  - {KEY_A}\n"),
                "glob",
            ),
            (
                "version: 1\nhosts:\n- name: h\n  keys: []\n".to_string(),
                "no keys",
            ),
            (
                "version: 1\nhosts:\n- name: h\n  keys:\n  - garbage\n".to_string(),
                "bad key",
            ),
            (
                format!(
                    "version: 1\nhosts:\n- name: h\n  keys:\n  - {KEY_A}\n  labels:\n    'a b': x\n"
                ),
                "label key",
            ),
            (
                format!(
                    "version: 1\nhosts:\n- name: h\n  keys:\n  - {KEY_A}\n  labels:\n    a: x,y\n"
                ),
                "label value",
            ),
            (
                "version: 1\nattributes:\n  'a=b': x\n".to_string(),
                "attribute key",
            ),
        ] {
            assert!(FleetDoc::parse(yaml.as_bytes()).is_err(), "{what} accepted");
        }
    }

    #[test]
    fn changed_key_needs_rekey() {
        let mut doc = sample();
        assert_eq!(
            doc.pin_host("sbc1.example", 22, vec![KEY_A.to_string()], false)
                .unwrap(),
            Pinned::Unchanged
        );
        match doc.pin_host("sbc1.example", 2200, vec![KEY_B.to_string()], false) {
            Err(DocError::KeyMismatch {
                pinned, scanned, ..
            }) => {
                assert_eq!(pinned, [FINGERPRINT_A]);
                assert_eq!(scanned, [FINGERPRINT_B]);
            }
            other => panic!("expected a mismatch, got {other:?}"),
        }
        assert_eq!(
            doc.host("sbc1.example")
                .unwrap()
                .keys(),
            [KEY_A],
            "a refused pin must not change the key"
        );
        assert_eq!(
            doc.host("sbc1.example")
                .unwrap()
                .port(),
            22,
            "a refused pin must not change the port"
        );
        let extra = vec![KEY_A.to_string(), KEY_B.to_string()];
        assert!(
            doc.pin_host("sbc1.example", 22, extra.clone(), false)
                .is_err(),
            "an added key type is a change too"
        );
        assert_eq!(
            doc.pin_host("sbc1.example", 22, extra, true)
                .unwrap(),
            Pinned::Replaced
        );
        assert_eq!(
            doc.host("sbc1.example")
                .unwrap()
                .labels()["env"],
            "prod",
            "rekey keeps labels"
        );
    }

    #[test]
    fn labels_and_attributes() {
        let mut doc = sample();
        assert!(matches!(
            doc.set_label("nope", "a", "b"),
            Err(DocError::HostNotFound { .. })
        ));
        assert!(
            doc.unset_label("sbc1.example", "env")
                .unwrap()
        );
        assert!(
            !doc.unset_label("sbc1.example", "env")
                .unwrap()
        );
        assert!(doc.unset_attribute("ssh_timeout_secs"));
        assert!(!doc.unset_attribute("ssh_timeout_secs"));
        assert!(doc.remove_host("sbc1.example"));
        assert!(!doc.remove_host("sbc1.example"));
    }
}
