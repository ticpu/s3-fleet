//! The subcommands that read or edit the fleet document.
//!
//! An edit loads the document, applies every change in memory, and commits
//! once with `If-Match`: a run never leaves half its changes behind. When some
//! hosts fail, nothing is written unless `--keep-going` was given; then the
//! rest is committed and the exit code is 3. Each failure is printed on its
//! own stderr line naming the host, before the summary.
//!
//! `add` scans every host before loading the document, so the window between
//! load and commit, where another writer causes a conflict, stays short.

use std::collections::BTreeMap;
use std::io::Write as _;
use std::path::Path;

use anyhow::Context as _;
use clap_complete::CompletionCandidate;
use s3_fleet::cache::Cache;
use s3_fleet::doc::{Pinned, validate_host_name, validate_key_name, validate_label};
use s3_fleet::keyscan::{KeyscanOptions, keyscan};
use s3_fleet::known_hosts;
use s3_fleet::selector::Selector;
use s3_fleet::{DocError, FleetDoc, Host, Init};
use serde::Serialize;
use tokio::task::JoinSet;

use super::exit::{Exit, Failure, exit_code};
use super::{AttrCommand, Ctx, Read, Source};

/// The `--json` format version.
const FORMAT: u32 = 1;

#[derive(Serialize)]
struct HostJson<'a> {
    name: &'a str,
    port: u16,
    keys: &'a [String],
    labels: &'a BTreeMap<String, String>,
}

#[derive(Serialize)]
struct FleetJson<'a> {
    format: u32,
    fleet: &'a str,
    endpoint: Option<&'a str>,
    endpoint_source: Option<String>,
    bucket: Option<&'a str>,
    source: &'static str,
    age_secs: Option<u64>,
    attributes: &'a BTreeMap<String, String>,
    hosts: Vec<HostJson<'a>>,
}

fn print_json<'a>(
    ctx: &'a Ctx,
    read: &'a Read,
    hosts: impl Iterator<Item = &'a Host>,
) -> anyhow::Result<()> {
    let (source, age_secs) = match read.source {
        Source::S3 => ("s3", None),
        Source::Cache { age } => ("cache", Some(age.as_secs())),
    };
    let target = read
        .target
        .as_ref();
    let out = FleetJson {
        format: FORMAT,
        fleet: &ctx.fleet,
        endpoint: target.map(|t| {
            t.endpoint
                .authority
                .as_str()
        }),
        endpoint_source: target.map(|t| {
            t.source
                .to_string()
        }),
        bucket: target.map(|t| {
            t.bucket
                .as_str()
        }),
        source,
        age_secs,
        attributes: read
            .doc
            .attributes(),
        hosts: hosts
            .map(|h| HostJson {
                name: h.name(),
                port: h.port(),
                keys: h.keys(),
                labels: h.labels(),
            })
            .collect(),
    };
    println!("{}", serde_json::to_string(&out)?);
    Ok(())
}

pub async fn init(ctx: &Ctx) -> anyhow::Result<Exit> {
    let target = ctx
        .target()
        .await?;
    let fleet = ctx.writer(&target)?;
    let what = match fleet
        .init()
        .await?
    {
        Init::Created => "created",
        Init::AlreadyExists => "already exists",
    };
    eprintln!(
        "s3-fleet: fleet {} {what} in bucket {} on {}",
        ctx.fleet,
        target.bucket,
        target
            .endpoint
            .authority
    );
    Ok(Exit::Ok)
}

pub async fn list(ctx: &Ctx, selectors: &[Selector], json: bool) -> anyhow::Result<Exit> {
    let read = ctx
        .read()
        .await?;
    let hosts = read
        .doc
        .select(selectors);
    if json {
        print_json(ctx, &read, hosts)?;
    } else {
        let mut out = std::io::stdout().lock();
        for host in hosts {
            writeln!(out, "{}", host.name())?;
        }
    }
    Ok(Exit::Ok)
}

pub async fn show(ctx: &Ctx, name: &str, json: bool) -> anyhow::Result<Exit> {
    let read = ctx
        .read()
        .await?;
    let host = read
        .doc
        .host(name)
        .ok_or_else(|| DocError::HostNotFound {
            name: name.to_string(),
        })?;
    if json {
        print_json(ctx, &read, std::iter::once(host))?;
    } else {
        print!("{}", serde_norway::to_string(host)?);
    }
    Ok(Exit::Ok)
}

/// Load, change, commit once; nothing is written if `change` fails or
/// leaves the document as it was.
async fn edit<T>(
    ctx: &Ctx,
    change: impl FnOnce(&mut FleetDoc) -> anyhow::Result<T>,
) -> anyhow::Result<T> {
    let target = ctx
        .target()
        .await?;
    let fleet = ctx.writer(&target)?;
    let snapshot = fleet
        .load()
        .await?;
    let mut doc = snapshot
        .doc
        .clone();
    let out = change(&mut doc)?;
    if doc != snapshot.doc {
        fleet
            .commit(snapshot, &doc)
            .await?;
    }
    ctx.store(&doc);
    Ok(out)
}

/// Reports per-host failures and decides between committing and aborting.
fn settle(
    failures: Vec<(String, anyhow::Error)>,
    total: usize,
    keep_going: bool,
) -> anyhow::Result<Exit> {
    if failures.is_empty() {
        return Ok(Exit::Ok);
    }
    let mut codes: Vec<Exit> = Vec::new();
    for (host, err) in &failures {
        eprintln!("s3-fleet: {host}: {err:#}");
        codes.push(exit_code(err));
    }
    if keep_going && failures.len() < total {
        return Ok(Exit::Partial);
    }
    codes.dedup();
    let code = match codes[..] {
        [single] => single,
        _ => Exit::Error,
    };
    Err(Failure::new(
        code,
        format!(
            "nothing written: {} of {total} host(s) failed",
            failures.len()
        ),
    )
    .into())
}

fn parse_pairs(pairs: &[String], what: &str) -> anyhow::Result<Vec<(String, String)>> {
    pairs
        .iter()
        .map(|p| {
            p.split_once('=')
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .ok_or_else(|| {
                    Failure::new(Exit::Usage, format!("{what} {p:?} is not key=value")).into()
                })
        })
        .collect()
}

#[allow(clippy::too_many_arguments)]
pub async fn add(
    ctx: &Ctx,
    hosts: &[String],
    labels: &[String],
    port: u16,
    key_types: &[String],
    rekey: bool,
    keep_going: bool,
) -> anyhow::Result<Exit> {
    let labels = parse_pairs(labels, "label")?;
    for (k, v) in &labels {
        validate_label(k, v)?;
    }
    for host in hosts {
        validate_host_name(host)?;
    }
    let target = ctx
        .target()
        .await?;
    ctx.writer(&target)?;

    let opts = KeyscanOptions {
        port,
        key_types: key_types.to_vec(),
        connect_timeout: ctx.timeout(),
        timeout: ctx.timeout() * 2,
    };
    let mut scans = JoinSet::new();
    for (i, host) in hosts
        .iter()
        .enumerate()
    {
        let host = host.clone();
        let opts = opts.clone();
        scans.spawn(async move {
            let keys = keyscan(&host, &opts).await;
            (i, host, keys)
        });
    }
    let mut scanned = scans
        .join_all()
        .await;
    scanned.sort_by_key(|(i, _, _)| *i);

    let total = hosts.len();
    let (exit, report) = edit(ctx, |doc| {
        let mut failures = Vec::new();
        let mut report: BTreeMap<&'static str, Vec<String>> = BTreeMap::new();
        for (_, host, keys) in scanned {
            let applied = keys
                .map_err(anyhow::Error::from)
                .and_then(|keys| {
                    let pinned = doc.pin_host(&host, port, keys, rekey)?;
                    for (k, v) in &labels {
                        doc.set_label(&host, k, v)?;
                    }
                    Ok(pinned)
                });
            match applied {
                Ok(pinned) => report
                    .entry(match pinned {
                        Pinned::Added => "added",
                        Pinned::Unchanged => "unchanged",
                        Pinned::Replaced => "rekeyed",
                    })
                    .or_default()
                    .push(host),
                Err(e) => failures.push((host, e)),
            }
        }
        Ok((settle(failures, total, keep_going)?, report))
    })
    .await?;
    for (what, names) in report {
        eprintln!("s3-fleet: {what}: {}", names.join(" "));
    }
    Ok(exit)
}

pub async fn rm(ctx: &Ctx, hosts: &[String], keep_going: bool) -> anyhow::Result<Exit> {
    let total = hosts.len();
    edit(ctx, |doc| {
        let mut failures = Vec::new();
        for host in hosts {
            if !doc.remove_host(host) {
                failures.push((
                    host.clone(),
                    DocError::HostNotFound { name: host.clone() }.into(),
                ));
            }
        }
        settle(failures, total, keep_going)
    })
    .await
}

enum LabelChange {
    Set(String, String),
    Unset(String),
}

/// Splits `label` arguments: `k=v` sets, `k-` removes, anything else is a
/// host. Host names cannot hold `=`; one ending in `-` cannot be labelled
/// this way.
fn split_label_args(args: &[String]) -> anyhow::Result<(Vec<&str>, Vec<LabelChange>)> {
    let mut hosts = Vec::new();
    let mut changes = Vec::new();
    for arg in args {
        if let Some((k, v)) = arg.split_once('=') {
            validate_label(k, v)?;
            changes.push(LabelChange::Set(k.to_string(), v.to_string()));
        } else if let Some(k) = arg.strip_suffix('-') {
            validate_key_name(k)?;
            changes.push(LabelChange::Unset(k.to_string()));
        } else {
            hosts.push(arg.as_str());
        }
    }
    if hosts.is_empty() || changes.is_empty() {
        return Err(Failure::new(Exit::Usage, "usage: label HOST... k=v|k-...").into());
    }
    Ok((hosts, changes))
}

pub async fn label(ctx: &Ctx, args: &[String]) -> anyhow::Result<Exit> {
    let (hosts, changes) = split_label_args(args)?;
    edit(ctx, |doc| {
        for host in &hosts {
            for change in &changes {
                match change {
                    LabelChange::Set(k, v) => doc.set_label(host, k, v)?,
                    LabelChange::Unset(k) => {
                        doc.unset_label(host, k)?;
                    }
                }
            }
        }
        Ok(Exit::Ok)
    })
    .await
}

pub async fn attr(ctx: &Ctx, command: &AttrCommand) -> anyhow::Result<Exit> {
    match command {
        AttrCommand::Set { pairs } => {
            let pairs = parse_pairs(pairs, "attribute")?;
            edit(ctx, |doc| {
                for (k, v) in &pairs {
                    doc.set_attribute(k, v)?;
                }
                Ok(Exit::Ok)
            })
            .await
        }
        AttrCommand::Rm { keys } => {
            edit(ctx, |doc| {
                for k in keys {
                    doc.unset_attribute(k);
                }
                Ok(Exit::Ok)
            })
            .await
        }
        AttrCommand::Get { key } => {
            let read = ctx
                .read()
                .await?;
            match read
                .doc
                .attributes()
                .get(key)
            {
                Some(value) => {
                    println!("{value}");
                    Ok(Exit::Ok)
                }
                None => {
                    Err(Failure::new(Exit::Error, format!("attribute {key} is not set")).into())
                }
            }
        }
    }
}

/// `name`, or `[name]:port` as ssh writes a non-default port in `%H`.
fn parse_host_arg(arg: &str) -> (&str, Option<u16>) {
    if let Some(rest) = arg.strip_prefix('[')
        && let Some((name, port)) = rest.split_once("]:")
        && let Ok(port) = port.parse()
    {
        return (name, Some(port));
    }
    (arg, None)
}

pub async fn known_hosts(
    ctx: &Ctx,
    selectors: &[Selector],
    host: Option<&str>,
    cached: bool,
    output: Option<&Path>,
) -> anyhow::Result<Exit> {
    let doc = if cached {
        ctx.cache()?
            .load(&ctx.fleet)
            .with_context(|| format!("read the cached fleet {}", ctx.fleet))?
            .doc
    } else {
        ctx.read()
            .await?
            .doc
    };
    let wanted = host.map(parse_host_arg);
    let hosts: Vec<&Host> = doc
        .select(selectors)
        .filter(|h| match wanted {
            None => true,
            Some((name, port)) => h.name() == name && port.is_none_or(|p| p == h.port()),
        })
        .collect();
    if let Some(arg) = host
        && hosts.is_empty()
    {
        return Err(DocError::HostNotFound {
            name: arg.to_string(),
        }
        .into());
    }
    let text = known_hosts::render(hosts);
    match output {
        None => print!("{text}"),
        Some(path) => write_atomic(path, text.as_bytes())?,
    }
    Ok(Exit::Ok)
}

/// Write through a sibling temp file and a rename, so a reader never sees a
/// partial file; the umask decides the mode.
fn write_atomic(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    let name = path
        .file_name()
        .with_context(|| format!("{} names no file", path.display()))?;
    let mut tmp_name = name.to_os_string();
    tmp_name.push(format!(".tmp.{}", std::process::id()));
    let tmp = path.with_file_name(tmp_name);
    std::fs::write(&tmp, bytes).with_context(|| format!("write {}", tmp.display()))?;
    std::fs::rename(&tmp, path)
        .with_context(|| format!("rename {} to {}", tmp.display(), path.display()))
}

/// Completion candidates: host names from the local cache of `$S3_FLEET`, or
/// of every cached fleet when it is unset. The completer sees no other
/// argument, so `--fleet` on the line being completed cannot be read. Never
/// touches the network; a cache that cannot be read offers nothing, since
/// completion has nowhere to report an error.
pub fn cached_hosts() -> Vec<CompletionCandidate> {
    let Ok(cache) = Cache::xdg() else {
        return Vec::new();
    };
    let fleets: Vec<String> = match std::env::var("S3_FLEET") {
        Ok(fleet) if !fleet.is_empty() => vec![fleet],
        _ => cached_fleets(&cache),
    };
    let mut names: Vec<String> = fleets
        .iter()
        .filter_map(|f| {
            cache
                .load(f)
                .ok()
        })
        .flat_map(|c| {
            c.doc
                .hosts()
                .iter()
                .map(|h| {
                    h.name()
                        .to_string()
                })
                .collect::<Vec<_>>()
        })
        .collect();
    names.sort();
    names.dedup();
    names
        .into_iter()
        .map(CompletionCandidate::new)
        .collect()
}

fn cached_fleets(cache: &Cache) -> Vec<String> {
    let Some(dir) = cache
        .path("x")
        .parent()
        .map(Path::to_path_buf)
    else {
        return Vec::new();
    };
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .filter_map(Result::ok)
        .filter_map(|e| {
            e.file_name()
                .to_str()
                .and_then(|n| n.strip_suffix(".yaml"))
                .map(str::to_string)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_arg_forms() {
        assert_eq!(parse_host_arg("h.example"), ("h.example", None));
        assert_eq!(
            parse_host_arg("[h.example]:2222"),
            ("h.example", Some(2222))
        );
        assert_eq!(
            parse_host_arg("[2001:db8::1]:22"),
            ("2001:db8::1", Some(22))
        );
        assert_eq!(parse_host_arg("2001:db8::1"), ("2001:db8::1", None));
        assert_eq!(parse_host_arg("[broken"), ("[broken", None));
    }

    #[test]
    fn label_args_split() {
        let args: Vec<String> = ["a", "b", "env=prod", "old-"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let (hosts, changes) = split_label_args(&args).unwrap();
        assert_eq!(hosts, ["a", "b"]);
        assert!(matches!(&changes[0], LabelChange::Set(k, v) if k == "env" && v == "prod"));
        assert!(matches!(&changes[1], LabelChange::Unset(k) if k == "old"));
        for bad in [&["a"][..], &["env=prod"][..], &["a", "bad key=x"][..]] {
            let bad: Vec<String> = bad
                .iter()
                .map(|s| s.to_string())
                .collect();
            assert!(split_label_args(&bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn settle_codes() {
        let mismatch = || {
            anyhow::Error::new(DocError::KeyMismatch {
                host: "h".into(),
                pinned: vec![],
                scanned: vec![],
            })
        };
        assert_eq!(settle(vec![], 2, false).unwrap(), Exit::Ok);
        let err = settle(vec![("h".into(), mismatch())], 2, false).unwrap_err();
        assert_eq!(
            exit_code(&err),
            Exit::KeyMismatch,
            "one kind of failure keeps its code"
        );
        assert_eq!(
            settle(vec![("h".into(), mismatch())], 2, true).unwrap(),
            Exit::Partial
        );
        let err = settle(vec![("h".into(), mismatch())], 1, true).unwrap_err();
        assert_eq!(
            exit_code(&err),
            Exit::KeyMismatch,
            "keep-going with nothing done still fails"
        );
        let mixed = vec![
            ("a".into(), mismatch()),
            ("b".into(), anyhow::anyhow!("other")),
        ];
        let err = settle(mixed, 3, false).unwrap_err();
        assert_eq!(exit_code(&err), Exit::Error);
    }
}
