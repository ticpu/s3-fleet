//! The lookup classifier on what the real ssh prints: a name that cannot
//! exist, and any name with the network gone (common::netns). Skipped where
//! ssh is missing, or where names still resolve without the network.

mod common;

use std::process::Command;

use s3_fleet::lookup::{LookupFailure, failed_lookup};

/// ssh's stderr for a connection to `host`, or `None` where ssh is missing.
fn ssh_stderr(host: &str) -> Option<String> {
    landlock_test_confine::to_scratch_only(&landlock_test_confine::target_dir());
    let output = match Command::new("ssh")
        .args(["-F", "/dev/null", "-o", "BatchMode=yes", host, "true"])
        .output()
    {
        Ok(output) => output,
        Err(e) => {
            eprintln!("skipped: run ssh: {e}");
            return None;
        }
    };
    Some(String::from_utf8_lossy(&output.stderr).into_owned())
}

#[test]
fn ssh_on_an_invalid_name_is_name_absent() {
    let Some(stderr) = ssh_stderr("nonexistent.invalid") else {
        return;
    };
    let failed = failed_lookup(&stderr).unwrap_or_else(|| panic!("{stderr}"));
    assert_eq!(failed.name, "nonexistent.invalid");
    assert_eq!(failed.failure, LookupFailure::NameAbsent, "{stderr}");
}

#[test]
fn ssh_with_no_resolver_is_not_name_absent() {
    if !common::netns::inside() {
        common::netns::rerun("ssh_with_no_resolver_is_not_name_absent");
        return;
    }
    let Some(stderr) = ssh_stderr("example.com") else {
        return;
    };
    match failed_lookup(&stderr) {
        Some(failed) => assert_eq!(failed.failure, LookupFailure::Other, "{stderr}"),
        None => eprintln!("skipped: names resolve without the network here: {stderr}"),
    }
}
