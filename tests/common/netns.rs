//! Running a test with the network gone: the test binary re-runs itself,
//! filtered to one test, under `unshare -rn`, a network namespace with no
//! interface up, where no DNS server can be reached.
//!
//! The re-run happens before the test confines itself: landlock would refuse
//! unshare its write to /proc/self/uid_map. The run inside confines itself as
//! usual. Skipped where unprivileged user namespaces are refused.

use std::process::Command;

const INSIDE: &str = "S3_FLEET_TEST_IN_NETNS";

/// Whether this process is the re-run inside the namespace.
pub fn inside() -> bool {
    std::env::var_os(INSIDE).is_some()
}

/// Re-run `test` inside the namespace and fail if it fails there; a note on
/// stderr instead where no namespace can be made.
pub fn rerun(test: &str) {
    let exe = std::env::current_exe().unwrap();
    let output = Command::new("unshare")
        .arg("-rn")
        .arg(exe)
        .args(["--exact", test, "--nocapture"])
        .env(INSIDE, "1")
        .output()
        .expect("run unshare");
    let stderr = String::from_utf8_lossy(&output.stderr);
    if !output
        .status
        .success()
        && stderr.starts_with("unshare:")
    {
        eprintln!("{test}: skipped, no user namespace: {stderr}");
        return;
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output
            .status
            .success(),
        "{test} in the namespace:\n{stdout}\n{stderr}"
    );
    assert!(stdout.contains("1 passed"), "{stdout}");
    eprint!("{stderr}");
}
