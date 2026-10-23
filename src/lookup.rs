//! Whether an OpenSSH tool failed because a name does not exist, from what it
//! printed on stderr.
//!
//! "The name does not exist" usually means a decommissioned host, and a
//! caller may act on it by dropping the host; "the resolver did not answer"
//! says nothing about the host. Both ssh and ssh-keyscan print one line per
//! failed lookup, never localised (OpenSSH does not call setlocale):
//!
//! - ssh-keyscan: `getaddrinfo <name>: <reason>`;
//! - ssh: `ssh: Could not resolve hostname <name>: <reason>`, with the name
//!   cut to 100 characters, so [`FailedLookup::name`] is for messages, not for
//!   matching back to a longer host name.
//!
//! `<reason>` is `gai_strerror` of the libc that tool was linked against, and
//! glibc, musl and the BSDs each word it differently, musl's EAI_AGAIN being a
//! bare "Try again". Only the EAI_NONAME and EAI_NODATA wordings listed here
//! give [`LookupFailure::NameAbsent`]; any other reason, an unknown wording
//! included, gives [`LookupFailure::Other`]. Taking a resolver outage for a
//! missing name could get a live host dropped; the reverse costs a retry.
//!
//! The verdict is the tool's, not ours, on purpose: ssh resolves through NSS,
//! so /etc/hosts, sssd and systemd-resolved's split DNS all count. A resolver
//! in this crate (hickory, or musl's in the static binary) would see only
//! resolv.conf, and may take the first answer from any listed server, letting
//! a public server's NXDOMAIN for an internal name win.
//!
//! With ProxyJump or ProxyCommand ssh does not resolve the target itself; a
//! failure on the far side prints a different line, and the result is `None`.

/// ssh-keyscan's prefix on a failed lookup.
const KEYSCAN_PREFIX: &str = "getaddrinfo ";

/// ssh's marker on a failed lookup, after its program name.
const SSH_MARKER: &str = ": Could not resolve hostname ";

/// `gai_strerror` texts meaning the name has no address: EAI_NONAME and
/// EAI_NODATA of glibc and musl, EAI_NONAME of the BSDs.
const NAME_ABSENT: &[&str] = &[
    "Name or service not known",
    "No address associated with hostname",
    "Name does not resolve",
    "Name has no usable address",
    "nodename nor servname provided, or not known",
];

/// A name lookup that an OpenSSH tool reported as failed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FailedLookup {
    /// The name looked up, as the tool printed it.
    pub name: String,
    /// What the resolver said.
    pub failure: LookupFailure,
}

/// What the resolver said about a name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LookupFailure {
    /// The name has no address; often a decommissioned host.
    NameAbsent,
    /// The lookup failed without saying the name is absent: a resolver that
    /// did not answer, a hard resolver error, or a reason not recognised.
    Other,
}

/// The failed lookup reported in an OpenSSH tool's stderr, or `None` when it
/// reports none. Several lookup lines are `NameAbsent` only if all are.
pub fn failed_lookup(stderr: &str) -> Option<FailedLookup> {
    let mut found: Option<FailedLookup> = None;
    for (name, reason) in stderr
        .lines()
        .filter_map(lookup_line)
    {
        let failure = if NAME_ABSENT.contains(&reason) {
            LookupFailure::NameAbsent
        } else {
            LookupFailure::Other
        };
        match &mut found {
            None => {
                found = Some(FailedLookup {
                    name: name.to_string(),
                    failure,
                })
            }
            Some(first) if failure == LookupFailure::Other => first.failure = failure,
            Some(_) => {}
        }
    }
    found
}

/// The name and reason of one lookup-failure line.
fn lookup_line(line: &str) -> Option<(&str, &str)> {
    let rest = match line.strip_prefix(KEYSCAN_PREFIX) {
        Some(rest) => rest,
        None => {
            line.split_once(SSH_MARKER)?
                .1
        }
    };
    let (name, reason) = rest.split_once(": ")?;
    Some((name, reason.trim()))
}

#[cfg(test)]
mod tests {
    use super::*;

    const ABSENT: [&str; 5] = [
        "Name or service not known",
        "No address associated with hostname",
        "Name does not resolve",
        "Name has no usable address",
        "nodename nor servname provided, or not known",
    ];

    fn both_tools(name: &str, reason: &str) -> [String; 2] {
        [
            format!("getaddrinfo {name}: {reason}\ngetaddrinfo {name}: {reason}"),
            format!("ssh: Could not resolve hostname {name}: {reason}"),
        ]
    }

    #[test]
    fn absent_name_in_every_libc() {
        for reason in ABSENT {
            for stderr in both_tools("h.example", reason) {
                assert_eq!(
                    failed_lookup(&stderr),
                    Some(FailedLookup {
                        name: "h.example".to_string(),
                        failure: LookupFailure::NameAbsent,
                    }),
                    "{stderr}"
                );
            }
        }
    }

    #[test]
    fn other_failures_say_nothing_about_the_host() {
        for reason in [
            "Temporary failure in name resolution",
            "Try again",
            "Non-recoverable failure in name resolution",
            "Non-recoverable error",
            "Something new",
        ] {
            for stderr in both_tools("h", reason) {
                assert_eq!(
                    failed_lookup(&stderr).map(|f| f.failure),
                    Some(LookupFailure::Other),
                    "{stderr}"
                );
            }
        }
        let mixed = "getaddrinfo h: Name or service not known\ngetaddrinfo h: Try again";
        assert_eq!(
            failed_lookup(mixed).map(|f| f.failure),
            Some(LookupFailure::Other)
        );
    }

    #[test]
    fn no_lookup_line_is_no_verdict() {
        for stderr in [
            "",
            "write (h): Connection reset by peer",
            "ssh: connect to host h port 22: Connection refused",
            "channel 0: open failed: connect failed: Name or service not known",
        ] {
            assert_eq!(failed_lookup(stderr), None, "{stderr}");
        }
    }
}
