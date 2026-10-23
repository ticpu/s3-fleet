//! `s3-fleet ssh`: run the system ssh against a fleet host, trusting only the
//! keys pinned in the fleet.
//!
//! ssh gets the keys from `KnownHostsCommand`, which calls this binary back as
//! `known-hosts --cached --host %H`, with `UserKnownHostsFile` and
//! `GlobalKnownHostsFile` set to `/dev/null` and `StrictHostKeyChecking=yes`.
//! A known_hosts file passed as a pipe does not work: ssh closes inherited
//! descriptors before reading it. `%H` is the bare name on port 22 and
//! `[name]:port` otherwise, which `known-hosts --host` accepts.
//!
//! `CheckHostIP` is forced off: with it on, ssh calls the command once more
//! with the server's address as `%H`, which the fleet does not know, and the
//! failing callback aborts the connection.
//!
//! The callback reads only the local cache. ssh may run it up to three times
//! per connection, and a network fetch each time would add seconds and three
//! chances to fail. This command refreshes the cache with one GET before
//! exec'ing ssh, and fails if S3 cannot be reached unless `--allow-stale`.
//!
//! ssh runs `KnownHostsCommand` through the user's shell, so the path to this
//! executable and to `--config` are shell-quoted. The fleet name needs no
//! quoting: the fleet-name rule in `s3_fleet::fleet` leaves only `[a-z0-9-]`.
//!
//! The destination ssh sees is always a host found in the fleet, so it cannot
//! start with `-`; a `user@` prefix is passed as `-l user`. Arguments after
//! the host go to ssh after the destination, where OpenSSH still parses
//! options and takes the rest as the remote command.
//!
//! The process is replaced with `exec`, so ssh owns the TTY and its exit
//! status is the caller's.

use std::ffi::OsString;
use std::os::unix::process::CommandExt as _;
use std::path::Path;
use std::process::Command;

use anyhow::Context as _;
use s3_fleet::DocError;
use s3_fleet::known_hosts::DEFAULT_PORT;

use super::Ctx;
use super::exit::Exit;

pub async fn run(
    ctx: &Ctx,
    config: Option<&Path>,
    destination: &str,
    args: &[OsString],
) -> anyhow::Result<Exit> {
    let (user, name) = match destination.rsplit_once('@') {
        Some((user, name)) => (Some(user), name),
        None => (None, destination),
    };
    let read = ctx
        .read()
        .await?;
    let host = read
        .doc
        .host(name)
        .ok_or_else(|| DocError::HostNotFound {
            name: name.to_string(),
        })?;

    let mut command = Command::new("ssh");
    command
        .arg("-o")
        .arg(format!(
            "KnownHostsCommand={}",
            callback(&ctx.fleet, config)?
        ))
        .args(["-o", "UserKnownHostsFile=/dev/null"])
        .args(["-o", "GlobalKnownHostsFile=/dev/null"])
        .args(["-o", "StrictHostKeyChecking=yes"])
        .args(["-o", "CheckHostIP=no"]);
    if host.port() != DEFAULT_PORT {
        command
            .arg("-p")
            .arg(
                host.port()
                    .to_string(),
            );
    }
    if let Some(user) = user {
        command
            .arg("-l")
            .arg(user);
    }
    command
        .arg(host.name())
        .args(args);
    let err = command.exec();
    Err(err).context("exec ssh")
}

/// The KnownHostsCommand line, quoted for the shell ssh runs it with.
fn callback(fleet: &str, config: Option<&Path>) -> anyhow::Result<String> {
    let exe = std::env::current_exe().context("find this executable")?;
    let mut line = quote(&exe)?;
    if let Some(config) = config {
        let config =
            std::path::absolute(config).with_context(|| format!("resolve {}", config.display()))?;
        line.push_str(" --config ");
        line.push_str(&quote(&config)?);
    }
    line.push_str(&format!(" --fleet {fleet} known-hosts --cached --host %H"));
    Ok(line)
}

/// A path quoted for the shell; `%` doubled, since ssh expands `%` tokens in
/// the command line before the shell sees it.
fn quote(path: &Path) -> anyhow::Result<String> {
    let text = path
        .to_str()
        .with_context(|| format!("{} is not UTF-8", path.display()))?;
    let quoted =
        shlex::try_quote(text).with_context(|| format!("cannot quote {text:?} for the shell"))?;
    Ok(quoted.replace('%', "%%"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What ssh then the shell make of a quoted path: `%%` becomes `%`, and
    /// the shell must split the result back into exactly the original path.
    #[test]
    fn quoted_paths_survive_ssh_and_the_shell() {
        for path in [
            "/usr/bin/s3-fleet",
            "/a b/s3-fleet",
            "/a'b/x",
            "/a\"b $HOME `x`/y",
            "/100%/x %h",
        ] {
            let quoted = quote(Path::new(path)).unwrap();
            let expanded = quoted.replace("%%", "%");
            assert_eq!(
                shlex::split(&expanded),
                Some(vec![path.to_string()]),
                "{path:?} quoted as {quoted:?}"
            );
            assert!(
                !quoted
                    .replace("%%", "")
                    .contains('%'),
                "{path:?}: a lone % would be an ssh token: {quoted:?}"
            );
        }
    }
}
