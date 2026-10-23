//! ssh-keyscan against real endpoints: this machine's sshd, a name that cannot
//! resolve, a closed port and a blackholed address. Skipped where ssh-keyscan
//! is missing; the localhost case is skipped when no sshd answers.
//!
//! The resolver-down case re-runs this test binary under `unshare -rn`, a
//! network namespace with no interface up, where the DNS server cannot be
//! reached. It is skipped where unprivileged user namespaces are refused, and
//! where names still resolve without the network (systemd-resolved reached
//! through nss-resolve's unix socket).

use std::process::Command;
use std::time::Duration;

use s3_fleet::keyscan::{KeyscanError, KeyscanOptions, keyscan};

fn have_keyscan() -> bool {
    landlock_test_confine::to_scratch_only(&landlock_test_confine::target_dir());
    let found = std::process::Command::new("ssh-keyscan")
        .arg("-V")
        .output()
        .is_ok();
    if !found {
        eprintln!("skipped: ssh-keyscan not installed");
    }
    found
}

#[tokio::test]
async fn localhost_returns_an_ed25519_key() {
    if !have_keyscan() {
        return;
    }
    match keyscan("localhost", &KeyscanOptions::default()).await {
        Ok(keys) => {
            assert!(!keys.is_empty());
            assert!(
                keys.iter()
                    .all(|k| k.starts_with("ssh-ed25519 ")),
                "{keys:?}"
            );
        }
        Err(KeyscanError::NoKeys { .. }) => eprintln!("skipped: no sshd on localhost:22"),
        Err(e) => panic!("{e}"),
    }
}

#[tokio::test]
async fn invalid_tld_is_a_resolution_failure() {
    if !have_keyscan() {
        return;
    }
    let err = keyscan("nonexistent.invalid", &KeyscanOptions::default()).await;
    assert!(
        matches!(err, Err(KeyscanError::Resolution { .. })),
        "{err:?}"
    );
}

const IN_NETNS: &str = "S3_FLEET_TEST_IN_NETNS";

#[tokio::test]
async fn unreachable_resolver_is_not_a_missing_name() {
    if std::env::var_os(IN_NETNS).is_some() {
        if !have_keyscan() {
            return;
        }
        match keyscan("example.com", &KeyscanOptions::default()).await {
            Err(KeyscanError::ResolverFailure { .. }) => {}
            Err(KeyscanError::NoKeys { .. }) => {
                eprintln!("skipped: names resolve without the network here")
            }
            other => panic!("{other:?}"),
        }
        return;
    }
    // Not confined yet: landlock would refuse unshare its write to
    // /proc/self/uid_map. The child confines itself.
    let exe = std::env::current_exe().unwrap();
    let output = Command::new("unshare")
        .arg("-rn")
        .arg(exe)
        .args([
            "--exact",
            "unreachable_resolver_is_not_a_missing_name",
            "--nocapture",
        ])
        .env(IN_NETNS, "1")
        .output()
        .expect("run unshare");
    let stderr = String::from_utf8_lossy(&output.stderr);
    if !output
        .status
        .success()
        && stderr.starts_with("unshare:")
    {
        eprintln!("skipped: no user namespace: {stderr}");
        return;
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output
            .status
            .success(),
        "in the namespace:\n{stdout}\n{stderr}"
    );
    assert!(stdout.contains("1 passed"), "{stdout}");
    eprint!("{stderr}");
}

#[tokio::test]
async fn closed_port_is_no_keys() {
    if !have_keyscan() {
        return;
    }
    let opts = KeyscanOptions {
        port: 1,
        ..KeyscanOptions::default()
    };
    let err = keyscan("localhost", &opts).await;
    assert!(matches!(err, Err(KeyscanError::NoKeys { .. })), "{err:?}");
}

#[tokio::test]
async fn wall_clock_limit_kills_the_scan() {
    if !have_keyscan() {
        return;
    }
    // TEST-NET-1 is never routed, so the connection hangs until -T fires.
    let opts = KeyscanOptions {
        connect_timeout: Duration::from_secs(30),
        timeout: Duration::from_millis(500),
        ..KeyscanOptions::default()
    };
    let started = std::time::Instant::now();
    let err = keyscan("192.0.2.1", &opts).await;
    assert!(matches!(err, Err(KeyscanError::Timeout { .. })), "{err:?}");
    assert!(started.elapsed() < Duration::from_secs(5));
}
