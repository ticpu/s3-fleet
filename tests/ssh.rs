//! `s3-fleet ssh` against a private sshd started for the test, with a host key
//! made for it: the pinned key gets ssh past host key verification to the
//! authentication stage, and a wrongly pinned key stops it there. Needs the
//! live S3 environment (tests/common) and an sshd binary.

mod common;

use std::io::{BufRead as _, BufReader};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

use common::cli::{Sandbox, live_args, run};
use s3_fleet::Fleet;

const HOST: &str = "127.0.0.1";

struct Sshd(Child);

impl Drop for Sshd {
    fn drop(&mut self) {
        // Best effort: the child is ours and already doomed.
        let _ = self
            .0
            .kill();
        let _ = self
            .0
            .wait();
    }
}

fn sshd_binary() -> Option<PathBuf> {
    ["/usr/bin/sshd", "/usr/sbin/sshd"]
        .into_iter()
        .map(PathBuf::from)
        .find(|p| p.exists())
}

/// A host key in `dir`, returned as its `<type> <base64>` field.
fn host_key(dir: &Path, name: &str) -> (PathBuf, String) {
    let path = dir.join(name);
    let status = Command::new("ssh-keygen")
        .args(["-q", "-t", "ed25519", "-N", "", "-C", "", "-f"])
        .arg(&path)
        .status()
        .unwrap();
    assert!(status.success());
    let public = std::fs::read_to_string(path.with_extension("pub")).unwrap();
    let key = public
        .split_whitespace()
        .take(2)
        .collect::<Vec<_>>()
        .join(" ");
    (path, key)
}

fn start_sshd(binary: &Path, host_key: &Path) -> (Sshd, u16) {
    let port = TcpListener::bind((HOST, 0))
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let mut child = Command::new(binary)
        .args(["-D", "-e", "-f", "/dev/null"])
        .arg("-o")
        .arg(format!("HostKey={}", host_key.display()))
        .args(["-o", &format!("Port={port}"), "-o", "PidFile=none"])
        .args(["-o", &format!("ListenAddress={HOST}")])
        .stdin(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let stderr = child
        .stderr
        .take()
        .unwrap();
    let guard = Sshd(child);
    for line in BufReader::new(stderr).lines() {
        if line
            .unwrap()
            .contains("Server listening")
        {
            return (guard, port);
        }
    }
    panic!("sshd exited before listening");
}

async fn pin(fleet: &Fleet, port: u16, key: &str) {
    let snap = fleet
        .load()
        .await
        .unwrap();
    let mut doc = snap
        .doc
        .clone();
    doc.pin_host(HOST, port, vec![key.to_string()], true)
        .unwrap();
    fleet
        .commit(snap, &doc)
        .await
        .unwrap();
}

#[tokio::test]
async fn pinned_key_passes_and_wrong_key_is_refused() {
    let Some(live) = common::live("pinned_key_passes_and_wrong_key_is_refused") else {
        return;
    };
    let Some(binary) = sshd_binary() else {
        eprintln!("skipped: no sshd binary");
        return;
    };
    let sb = Sandbox::new();
    let (real_path, real_key) = host_key(sb.path(), "real");
    let (_, wrong_key) = host_key(sb.path(), "wrong");
    let (_sshd, port) = start_sshd(&binary, &real_path);

    let fleet = Fleet::new(&live.bucket, live.signed()).unwrap();
    fleet
        .init()
        .await
        .unwrap();
    let base = live_args(&live);
    // Authentication is switched off: reaching it proves the host key passed.
    let ssh = |user: &str| {
        let dest = format!("{user}{HOST}");
        let mut args: Vec<&str> = base
            .iter()
            .map(String::as_str)
            .collect();
        args.extend([
            "ssh",
            &dest,
            "-F",
            "/dev/null",
            "-o",
            "BatchMode=yes",
            "-o",
            "PubkeyAuthentication=no",
            "-o",
            "PasswordAuthentication=no",
            "-o",
            "KbdInteractiveAuthentication=no",
            "true",
        ]);
        run(&mut sb.cmd(&args))
    };

    pin(&fleet, port, &real_key).await;
    let (code, _, err) = ssh("");
    assert_eq!(code, 255, "{err}");
    assert!(
        err.contains("Permission denied"),
        "host key accepted: {err}"
    );
    assert!(!err.contains("Host key verification failed"), "{err}");

    let (_, _, err) = ssh("nobody@");
    assert!(
        err.contains("nobody@127.0.0.1: Permission denied"),
        "user@ goes to -l: {err}"
    );

    pin(&fleet, port, &wrong_key).await;
    let (code, _, err) = ssh("");
    assert_eq!(code, 255, "{err}");
    assert!(
        err.contains("Host key verification failed"),
        "wrong key refused: {err}"
    );

    let snap = fleet
        .load()
        .await
        .unwrap();
    let mut doc = snap
        .doc
        .clone();
    doc.remove_host(HOST);
    fleet
        .commit(snap, &doc)
        .await
        .unwrap();
}
