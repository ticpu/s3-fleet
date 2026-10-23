//! `s3-fleet setup`: ask for an S3 keypair on the terminal, check it against
//! the fleet's endpoint, and store it as `credentials.<fleet>` in the user's
//! config file (or `--config`).
//!
//! It refuses to run when stdin is not a TTY (exit 2), so a script can never
//! hang on it. The secret is read with echo off and appears in no message.
//! The keypair is checked with a signed HEAD of the fleet document before
//! anything is written: 200 or 404 mean the keys were accepted, 403 means they
//! were refused (exit 6). The file is replaced through a 0600 temp file and a
//! rename, so a looser mode on the old file is not checked: it is gone after
//! the rename. Other settings in it are kept, its comments are not, since it
//! is re-serialised.

use std::fs;
use std::io::{self, BufRead as _, IsTerminal as _, Write as _};
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::Path;

use anyhow::Context as _;
use s3_fleet::fleet::DOCUMENT_KEY;
use s3_fleet::s3::{Credentials, S3};
use serde_norway::{Mapping, Value};

use super::Ctx;
use super::config;
use super::exit::{Exit, Failure};

pub async fn run(ctx: &Ctx, config_path: Option<&Path>) -> anyhow::Result<Exit> {
    if !io::stdin().is_terminal() {
        return Err(Failure::new(
            Exit::Usage,
            "setup asks on a terminal and stdin is not one; set AWS_ACCESS_KEY_ID and AWS_SECRET_ACCESS_KEY instead",
        )
        .into());
    }
    let target = match config_path {
        Some(path) => path.to_path_buf(),
        None => config::user_config_path()?,
    };
    let (endpoint, source) = ctx
        .endpoint()
        .await?;
    eprintln!(
        "s3-fleet: fleet {} on {} (from {source})",
        ctx.fleet, endpoint.authority
    );

    eprint!("access key: ");
    io::stderr().flush()?;
    let mut access_key = String::new();
    io::stdin()
        .lock()
        .read_line(&mut access_key)
        .context("read the access key")?;
    let access_key = access_key
        .trim()
        .to_string();
    let secret_key = rpassword::prompt_password("secret key: ").context("read the secret key")?;
    if access_key.is_empty() || secret_key.is_empty() {
        return Err(
            Failure::new(Exit::Usage, "the access key and secret key cannot be empty").into(),
        );
    }

    let s3 = S3::new(
        endpoint,
        Some(Credentials {
            access_key: access_key.clone(),
            secret_key: secret_key.clone(),
            session_token: None,
        }),
        ctx.timeout(),
    )?;
    s3.head_object(&ctx.fleet, DOCUMENT_KEY)
        .await
        .context("check the keys")?;

    store(&target, &ctx.fleet, &access_key, &secret_key)?;
    eprintln!(
        "s3-fleet: keys for fleet {} stored in {}",
        ctx.fleet,
        target.display()
    );
    Ok(Exit::Ok)
}

fn store(path: &Path, fleet: &str, access_key: &str, secret_key: &str) -> anyhow::Result<()> {
    let mut root = match fs::read_to_string(path) {
        Ok(text) => serde_norway::from_str::<Value>(&text)
            .with_context(|| format!("parse {}", path.display()))?,
        Err(e) if e.kind() == io::ErrorKind::NotFound => Value::Mapping(Mapping::new()),
        Err(e) => return Err(e).with_context(|| format!("read {}", path.display())),
    };
    let Value::Mapping(map) = &mut root else {
        anyhow::bail!("{} is not a YAML mapping", path.display());
    };
    let credentials = map
        .entry("credentials".into())
        .or_insert_with(|| Value::Mapping(Mapping::new()));
    let Value::Mapping(credentials) = credentials else {
        anyhow::bail!("{}: credentials is not a mapping", path.display());
    };
    let mut entry = Mapping::new();
    entry.insert("access_key".into(), access_key.into());
    entry.insert("secret_key".into(), secret_key.into());
    credentials.insert(fleet.into(), Value::Mapping(entry));
    let text = serde_norway::to_string(&root)?;

    if let Some(dir) = path.parent()
        && !dir
            .as_os_str()
            .is_empty()
    {
        fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
    }
    let mut tmp_name = path
        .file_name()
        .with_context(|| format!("{} names no file", path.display()))?
        .to_os_string();
    tmp_name.push(".tmp");
    let tmp = path.with_file_name(tmp_name);
    match fs::remove_file(&tmp) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e).with_context(|| format!("remove stale {}", tmp.display())),
    }
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&tmp)
        .with_context(|| format!("create {}", tmp.display()))?;
    file.write_all(text.as_bytes())
        .with_context(|| format!("write {}", tmp.display()))?;
    file.sync_all()
        .with_context(|| format!("sync {}", tmp.display()))?;
    fs::rename(&tmp, path)
        .with_context(|| format!("rename {} to {}", tmp.display(), path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};

    fn scratch_dir() -> tempfile::TempDir {
        landlock_test_confine::to_scratch_only(&landlock_test_confine::target_dir());
        let root = landlock_test_confine::scratch_dir("test-tmp");
        fs::create_dir_all(&root).unwrap();
        tempfile::tempdir_in(root).unwrap()
    }

    #[test]
    fn store_keeps_other_settings_and_tightens_the_mode() {
        let dir = scratch_dir();
        let path = dir
            .path()
            .join("s3-fleet.yaml");
        fs::write(&path, "endpoint: s3.example\nregion: r\n").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        store(&path, "prod", "AK", "SK").unwrap();
        store(&path, "test", "AK2", "SK2").unwrap();
        let text = fs::read_to_string(&path).unwrap();
        assert!(text.contains("endpoint: s3.example"), "{text}");
        let value: Value = serde_norway::from_str(&text).unwrap();
        assert_eq!(value["credentials"]["prod"]["access_key"], "AK");
        assert_eq!(value["credentials"]["test"]["secret_key"], "SK2");
        assert_eq!(
            fs::metadata(&path)
                .unwrap()
                .mode()
                & 0o777,
            0o600
        );
    }
}
