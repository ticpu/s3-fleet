//! Running the s3-fleet binary in a sandbox: its own XDG directories, an
//! empty `--config`, and none of the caller's AWS or S3_FLEET variables, so
//! the machine's files and keys are never used.

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use super::Live;

pub const BIN: &str = env!("CARGO_BIN_EXE_s3-fleet");

pub struct Sandbox {
    dir: tempfile::TempDir,
}

impl Sandbox {
    /// Also confines the calling test thread, and every process it starts,
    /// to writing under the build directory.
    pub fn new() -> Self {
        landlock_test_confine::to_scratch_only(&landlock_test_confine::target_dir());
        let dir = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap();
        std::fs::write(
            dir.path()
                .join("config.yaml"),
            "{}\n",
        )
        .unwrap();
        Self { dir }
    }

    pub fn path(&self) -> &Path {
        self.dir
            .path()
    }

    /// The binary with isolated XDG dirs and no AWS keys unless added.
    pub fn cmd(&self, args: &[&str]) -> Command {
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
            .env_remove("S3_FLEET_BUCKET")
            .env_remove("RUST_LOG")
            .stdin(Stdio::null());
        cmd
    }

    pub fn cache_file(&self, fleet: &str) -> PathBuf {
        self.path()
            .join("cache/s3-fleet")
            .join(format!("{fleet}.yaml"))
    }
}

pub fn run(cmd: &mut Command) -> (i32, String, String) {
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

pub fn live_args(live: &Live) -> Vec<String> {
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

pub fn with_keys(cmd: &mut Command, live: &Live) {
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

pub fn args<'a>(base: &'a [String], more: &[&'a str]) -> Vec<&'a str> {
    base.iter()
        .map(String::as_str)
        .chain(
            more.iter()
                .copied(),
        )
        .collect()
}
