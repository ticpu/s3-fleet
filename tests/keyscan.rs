//! ssh-keyscan against real endpoints: this machine's sshd, a name that cannot
//! resolve, a closed port and a blackholed address. Skipped where ssh-keyscan
//! is missing; the localhost case is skipped when no sshd answers.

use std::time::Duration;

use s3_fleet::keyscan::{KeyscanError, KeyscanOptions, keyscan};

fn have_keyscan() -> bool {
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
