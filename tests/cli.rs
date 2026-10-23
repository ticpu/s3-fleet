//! The s3-fleet binary end to end: exit codes, stdout and stderr, the cache
//! fallback. Each run gets its own XDG directories and an empty `--config`,
//! so the machine's files are never read. The cases that need S3 are skipped
//! unless tests/common's environment is set; the add/rm flow also needs an
//! sshd on localhost.

mod common;

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use s3_fleet::Fleet;

const BIN: &str = env!("CARGO_BIN_EXE_s3-fleet");

struct Sandbox {
    dir: tempfile::TempDir,
}

impl Sandbox {
    fn new() -> Self {
        let dir = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap();
        std::fs::write(
            dir.path()
                .join("config.yaml"),
            "{}\n",
        )
        .unwrap();
        Self { dir }
    }

    fn path(&self) -> &Path {
        self.dir
            .path()
    }

    /// The binary with isolated XDG dirs and no AWS keys unless added.
    fn cmd(&self, args: &[&str]) -> Command {
        let mut cmd = Command::new(BIN);
        cmd.args(["--config"])
            .arg(
                self.path()
                    .join("config.yaml"),
            )
            .args(args)
            .env(
                "XDG_CACHE_HOME",
                self.path()
                    .join("cache"),
            )
            .env(
                "XDG_CONFIG_HOME",
                self.path()
                    .join("config"),
            )
            .env_remove("AWS_ACCESS_KEY_ID")
            .env_remove("AWS_SECRET_ACCESS_KEY")
            .env_remove("AWS_SESSION_TOKEN")
            .env_remove("S3_FLEET")
            .env_remove("S3_FLEET_ENDPOINT")
            .env_remove("S3_FLEET_REGION")
            .env_remove("RUST_LOG")
            .stdin(Stdio::null());
        cmd
    }

    fn cache_file(&self, fleet: &str) -> PathBuf {
        self.path()
            .join("cache/s3-fleet")
            .join(format!("{fleet}.yaml"))
    }
}

fn run(cmd: &mut Command) -> (i32, String, String) {
    let Output {
        status,
        stdout,
        stderr,
    } = cmd
        .output()
        .unwrap();
    (
        status
            .code()
            .expect("exited, not signalled"),
        String::from_utf8(stdout).unwrap(),
        String::from_utf8(stderr).unwrap(),
    )
}

#[test]
fn usage_errors_exit_2_without_touching_the_network() {
    let sb = Sandbox::new();
    let (code, out, err) = run(&mut sb.cmd(&["list"]));
    assert_eq!(code, 2, "{err}");
    assert!(out.is_empty());
    assert!(err.contains("no fleet"), "{err}");

    let (code, _, err) = run(&mut sb.cmd(&["-f", "Bad_Name", "list"]));
    assert_eq!(code, 2, "{err}");

    let (code, _, err) = run(&mut sb.cmd(&["-f", "fleet", "list", "-l", "a b"]));
    assert_eq!(code, 2, "{err}");

    let (code, _, err) = run(&mut sb.cmd(&["-f", "fleet", "--endpoint", "x", "list"]));
    assert_eq!(code, 2, "an endpoint without a region: {err}");
}

#[test]
fn setup_never_prompts_without_a_tty() {
    let sb = Sandbox::new();
    let (code, out, err) = run(&mut sb.cmd(&["-f", "fleet", "setup"]));
    assert_eq!(code, 2, "{err}");
    assert!(out.is_empty());
    assert!(err.contains("not one"), "{err}");
}

#[test]
fn cached_known_hosts_without_a_cache_fails_offline() {
    let sb = Sandbox::new();
    let (code, out, err) =
        run(&mut sb.cmd(&["-f", "fleet", "known-hosts", "--cached", "--host", "h"]));
    assert_eq!(code, 1, "{err}");
    assert!(out.is_empty());
}

fn live_args(live: &common::Live) -> Vec<String> {
    vec![
        "-f".into(),
        live.bucket
            .clone(),
        "--endpoint".into(),
        live.endpoint
            .authority
            .clone(),
        "--region".into(),
        live.endpoint
            .region
            .clone(),
    ]
}

fn with_keys(cmd: &mut Command, live: &common::Live) {
    cmd.env(
        "AWS_ACCESS_KEY_ID",
        &live
            .credentials
            .access_key,
    )
    .env(
        "AWS_SECRET_ACCESS_KEY",
        &live
            .credentials
            .secret_key,
    );
}

fn args<'a>(base: &'a [String], more: &[&'a str]) -> Vec<&'a str> {
    base.iter()
        .map(String::as_str)
        .chain(
            more.iter()
                .copied(),
        )
        .collect()
}

#[test]
fn anonymous_reads_and_refused_writes() {
    let Some(live) = common::live("anonymous_reads_and_refused_writes") else {
        return;
    };
    let sb = Sandbox::new();
    let base = live_args(&live);

    let mut init = sb.cmd(&args(&base, &["init"]));
    with_keys(&mut init, &live);
    let (code, _, err) = run(&mut init);
    assert_eq!(code, 0, "{err}");

    let (code, _, err) = run(&mut sb.cmd(&args(&base, &["list"])));
    assert_eq!(code, 0, "{err}");
    assert!(
        sb.cache_file(&live.bucket)
            .exists(),
        "a read refreshes the cache"
    );

    let (code, _, err) = run(&mut sb.cmd(&args(&base, &["attr", "set", "x=y"])));
    assert_eq!(code, 6, "a write without keys: {err}");

    let (code, _, err) = run(&mut sb.cmd(&[
        "-f",
        "s3-fleet-never-created",
        "--endpoint",
        &live
            .endpoint
            .authority,
        "--region",
        &live
            .endpoint
            .region,
        "list",
    ]));
    assert_eq!(code, 5, "a missing fleet, anonymously: {err}");
}

#[test]
fn stale_cache_only_when_allowed() {
    let Some(live) = common::live("stale_cache_only_when_allowed") else {
        return;
    };
    let sb = Sandbox::new();
    let (code, _, err) = run(&mut sb.cmd(&args(&live_args(&live), &["list"])));
    assert_eq!(code, 0, "seed the cache: {err}");

    // Nothing listens on port 1: the endpoint is unreachable.
    let down = [
        "-f",
        &live.bucket,
        "--endpoint",
        "127.0.0.1:1",
        "--region",
        "x",
    ];
    let (code, out, err) = run(&mut sb.cmd(&[&down[..], &["list"]].concat()));
    assert_eq!(code, 8, "unreachable without --allow-stale: {err}");
    assert!(out.is_empty());

    let (code, out, err) =
        run(&mut sb.cmd(&[&down[..], &["--allow-stale", "list", "--json"]].concat()));
    assert_eq!(code, 0, "{err}");
    let json: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(json["format"], 1);
    assert_eq!(json["source"], "cache");
    assert!(json["age_secs"].is_u64(), "{json}");
    assert!(err.contains("using the cached fleet"), "{err}");

    std::fs::remove_file(sb.cache_file(&live.bucket)).unwrap();
    let (code, _, err) = run(&mut sb.cmd(&[&down[..], &["--allow-stale", "list"]].concat()));
    assert_eq!(code, 8, "unreachable and no cache: {err}");
}

#[tokio::test]
async fn add_label_known_hosts_rekey_rm() {
    let Some(live) = common::live("add_label_known_hosts_rekey_rm") else {
        return;
    };
    let probe = s3_fleet::keyscan::keyscan("localhost", &Default::default()).await;
    let Ok(real_keys) = probe else {
        eprintln!("skipped: no sshd on localhost: {probe:?}");
        return;
    };
    let sb = Sandbox::new();
    let base = live_args(&live);
    let signed = |more: &[&str]| {
        let mut cmd = sb.cmd(&args(&base, more));
        with_keys(&mut cmd, &live);
        run(&mut cmd)
    };

    let (code, _, err) = signed(&["init"]);
    assert_eq!(code, 0, "{err}");
    // A leftover from an interrupted run would turn the add into "unchanged".
    signed(&["rm", "localhost"]);

    let (code, _, err) = signed(&["add", "localhost", "-l", "env=cli-test"]);
    assert_eq!(code, 0, "{err}");
    assert!(err.contains("added: localhost"), "{err}");

    let (code, out, err) =
        run(&mut sb.cmd(&args(&base, &["list", "-l", "env=cli-test", "--json"])));
    assert_eq!(code, 0, "{err}");
    let json: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(json["source"], "s3");
    assert_eq!(json["endpoint_source"], "command line or environment");
    assert_eq!(json["hosts"][0]["name"], "localhost");
    assert_eq!(json["hosts"][0]["labels"]["env"], "cli-test");
    assert_eq!(json["hosts"][0]["keys"][0], real_keys[0].as_str());

    let (code, out, _) = run(&mut sb.cmd(&args(
        &base,
        &["known-hosts", "--cached", "--host", "localhost"],
    )));
    assert_eq!(code, 0);
    assert_eq!(out, format!("localhost {}\n", real_keys[0]));

    let (code, _, err) = signed(&["label", "localhost", "role=test", "env-"]);
    assert_eq!(code, 0, "{err}");
    let (_, out, _) = run(&mut sb.cmd(&args(&base, &["list", "-l", "role=test,!env"])));
    assert_eq!(out, "localhost\n");

    // Pin a different key behind the CLI's back: the next add must refuse it.
    let fleet = Fleet::new(&live.bucket, live.signed()).unwrap();
    let snap = fleet
        .load()
        .await
        .unwrap();
    let mut doc = snap
        .doc
        .clone();
    let other = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIBmcY94uJ8nfRLVFBp/kPsyfZq54fZCLKJk8GCwvUlCa";
    doc.pin_host("localhost", 22, vec![other.to_string()], true)
        .unwrap();
    fleet
        .commit(snap, &doc)
        .await
        .unwrap();

    let (code, _, err) = signed(&["add", "localhost"]);
    assert_eq!(code, 7, "{err}");
    assert!(
        err.contains("SHA256:D0PDqGgHPKMDnfCFygDhZKx/TLQYdS7cXB2WmQI9jRo"),
        "{err}"
    );
    assert!(err.contains("nothing written"), "{err}");

    let (code, _, err) = signed(&["add", "localhost", "--rekey"]);
    assert_eq!(code, 0, "{err}");
    assert!(err.contains("rekeyed: localhost"), "{err}");

    let (code, _, err) = signed(&["add", "localhost", "nonexistent.invalid"]);
    assert_eq!(code, 1, "one host fails, nothing written: {err}");
    let (code, _, err) = signed(&["add", "localhost", "nonexistent.invalid", "--keep-going"]);
    assert_eq!(code, 3, "partial: {err}");
    assert!(err.contains("nonexistent.invalid: "), "{err}");

    let (code, _, err) = signed(&["rm", "localhost"]);
    assert_eq!(code, 0, "{err}");
    let (code, _, err) = signed(&["rm", "localhost"]);
    assert_eq!(code, 1, "not in the fleet any more: {err}");
}
