//! The s3-fleet binary end to end: exit codes, stdout and stderr, the cache
//! fallback. Each run gets its own XDG directories and an empty `--config`,
//! so the machine's files are never read. The cases that need S3 are skipped
//! unless tests/common's environment is set; the add/rm flow also needs an
//! sshd on localhost.

mod common;

use std::process::Command;

use common::cli::{Sandbox, args, live_args, run, with_keys};
use s3_fleet::Fleet;

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

#[test]
fn completion_offers_cached_hosts_offline() {
    let sb = Sandbox::new();
    let mut doc = s3_fleet::FleetDoc::new();
    let key = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIBmcY94uJ8nfRLVFBp/kPsyfZq54fZCLKJk8GCwvUlCa";
    for host in ["web1.example", "web2.example", "db1.example"] {
        doc.pin_host(host, 22, vec![key.to_string()], false)
            .unwrap();
    }
    s3_fleet::cache::Cache::new(
        sb.path()
            .join("cache/s3-fleet"),
    )
    .store("compfleet", &doc)
    .unwrap();

    // What the script from `COMPLETE=bash s3-fleet` runs for `s3-fleet ssh web<TAB>`.
    let (code, out, err) = run(Command::new(common::cli::BIN)
        .args(["--", "s3-fleet", "ssh", "web"])
        .env("COMPLETE", "bash")
        .env("_CLAP_IFS", "\n")
        .env("_CLAP_COMPLETE_INDEX", "2")
        .env("_CLAP_COMPLETE_COMP_TYPE", "9")
        .env("_CLAP_COMPLETE_SPACE", "true")
        .env("S3_FLEET", "compfleet")
        .env(
            "XDG_CACHE_HOME",
            sb.path()
                .join("cache"),
        )
        .env("S3_FLEET_ENDPOINT", "127.0.0.1:1")
        .env("S3_FLEET_REGION", "x"));
    assert_eq!(code, 0, "{err}");
    let mut offered: Vec<&str> = out
        .lines()
        .collect();
    offered.sort();
    assert_eq!(offered, ["web1.example", "web2.example"], "{out:?}");
}
