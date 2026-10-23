//! Reading the config files and resolving the endpoint and credentials from
//! them; the layering rules are in the notes of the parent module.

use std::collections::BTreeMap;
use std::fmt;
use std::io;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use etcetera::BaseStrategy;
use s3_fleet::discover::{self, DiscoverError};
use s3_fleet::s3::{Credentials, DEFAULT_REGION, Endpoint};
use serde::Deserialize;
use tracing::debug;

pub const SYSTEM_CONFIG: &str = "/etc/s3-fleet.yaml";
pub const CONFIG_NAME: &str = "s3-fleet.yaml";
const DEFAULT_CREDENTIALS: &str = "default";

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("{0}")]
    Usage(String),
    #[error("find the XDG config directory")]
    Dir(#[source] etcetera::HomeDirError),
    #[error("read {}", path.display())]
    Read {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("parse {}", path.display())]
    Parse {
        path: PathBuf,
        #[source]
        source: serde_norway::Error,
    },
    #[error(
        "{} holds credentials but has mode {mode:04o}; run: chmod 600 {}",
        path.display(),
        path.display()
    )]
    Permissions { path: PathBuf, mode: u32 },
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileConfig {
    endpoint: Option<String>,
    region: Option<String>,
    #[serde(default)]
    credentials: BTreeMap<String, FileCredentials>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileCredentials {
    access_key: String,
    secret_key: String,
}

/// One config file, in the order read: lowest precedence first.
pub struct Layer {
    pub path: PathBuf,
    config: FileConfig,
}

/// Where the endpoint came from, for `-v` and `--json`.
#[derive(Clone, Debug)]
pub enum EndpointSource {
    CommandLine,
    File(PathBuf),
    Dns(String),
}

impl fmt::Display for EndpointSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::CommandLine => f.write_str("command line or environment"),
            Self::File(path) => write!(f, "config {}", path.display()),
            Self::Dns(record) => write!(f, "dns {record}"),
        }
    }
}

pub fn user_config_path() -> Result<PathBuf, ConfigError> {
    let base = etcetera::choose_base_strategy().map_err(ConfigError::Dir)?;
    Ok(base
        .config_dir()
        .join(CONFIG_NAME))
}

/// The config files in effect: `explicit` alone, or the system file then the
/// user's, each skipped when absent.
pub fn load_layers(explicit: Option<&Path>) -> Result<Vec<Layer>, ConfigError> {
    if let Some(path) = explicit {
        return Ok(vec![read_layer(path)?]);
    }
    let mut layers = Vec::new();
    for path in [PathBuf::from(SYSTEM_CONFIG), user_config_path()?] {
        match read_layer(&path) {
            Ok(layer) => layers.push(layer),
            Err(ConfigError::Read { source, .. }) if source.kind() == io::ErrorKind::NotFound => {
                debug!("no config at {}", path.display());
            }
            Err(e) => return Err(e),
        }
    }
    Ok(layers)
}

fn read_layer(path: &Path) -> Result<Layer, ConfigError> {
    let read_error = |source| ConfigError::Read {
        path: path.to_path_buf(),
        source,
    };
    let text = std::fs::read_to_string(path).map_err(read_error)?;
    let config: FileConfig =
        serde_norway::from_str(&text).map_err(|source| ConfigError::Parse {
            path: path.to_path_buf(),
            source,
        })?;
    if !config
        .credentials
        .is_empty()
    {
        let mode = std::fs::metadata(path)
            .map_err(read_error)?
            .mode()
            & 0o7777;
        check_mode(path, mode)?;
    }
    Ok(Layer {
        path: path.to_path_buf(),
        config,
    })
}

/// A file with keys may be readable by its group (a root-owned /etc file
/// shared with an admin group), never writable by it nor open to others.
pub fn check_mode(path: &Path, mode: u32) -> Result<(), ConfigError> {
    if mode & 0o027 != 0 {
        return Err(ConfigError::Permissions {
            path: path.to_path_buf(),
            mode,
        });
    }
    Ok(())
}

/// Where a fleet lives: endpoint, where that came from, and bucket.
#[derive(Clone, Debug)]
pub struct Target {
    pub endpoint: Endpoint,
    pub source: EndpointSource,
    pub bucket: String,
}

/// The endpoint from the highest layer naming one, else from DNS. The
/// bucket is `bucket` if given, else a per-fleet record's `bucket=` when the
/// endpoint was discovered, else the fleet name.
pub async fn resolve_target(
    endpoint: Option<&str>,
    region: Option<&str>,
    bucket: Option<&str>,
    layers: &[Layer],
    fleet: &str,
) -> anyhow::Result<Target> {
    let (endpoint, source, discovered) = resolve_endpoint(endpoint, region, layers, fleet).await?;
    let bucket = bucket
        .map(str::to_string)
        .or(discovered)
        .unwrap_or_else(|| fleet.to_string());
    Ok(Target {
        endpoint,
        source,
        bucket,
    })
}

async fn resolve_endpoint(
    endpoint: Option<&str>,
    region: Option<&str>,
    layers: &[Layer],
    fleet: &str,
) -> anyhow::Result<(Endpoint, EndpointSource, Option<String>)> {
    if let Some(authority) = endpoint {
        return Ok((
            Endpoint {
                authority: authority.to_string(),
                region: region
                    .unwrap_or(DEFAULT_REGION)
                    .to_string(),
            },
            EndpointSource::CommandLine,
            None,
        ));
    }
    if region.is_some() {
        return Err(ConfigError::Usage(
            "--region (S3_FLEET_REGION) is only used with --endpoint (S3_FLEET_ENDPOINT)".into(),
        )
        .into());
    }
    for layer in layers
        .iter()
        .rev()
    {
        let config = &layer.config;
        let Some(authority) = &config.endpoint else {
            if config
                .region
                .is_some()
            {
                return Err(ConfigError::Usage(format!(
                    "{}: region is only used with endpoint",
                    layer
                        .path
                        .display()
                ))
                .into());
            }
            continue;
        };
        let region = config
            .region
            .clone()
            .unwrap_or_else(|| DEFAULT_REGION.to_string());
        return Ok((
            Endpoint {
                authority: authority.clone(),
                region,
            },
            EndpointSource::File(
                layer
                    .path
                    .clone(),
            ),
            None,
        ));
    }
    let found = discover::discover(fleet)
        .await
        .map_err(|e: DiscoverError| anyhow::Error::new(e).context("discover the S3 endpoint"))?;
    Ok((
        found.endpoint,
        EndpointSource::Dns(found.record),
        found.bucket,
    ))
}

/// The AWS environment pair, else the most specific entry in the files:
/// `credentials.<fleet>` from any file before `credentials.default`, each
/// searched from the highest file down.
pub fn resolve_credentials(
    layers: &[Layer],
    fleet: &str,
) -> Result<Option<Credentials>, ConfigError> {
    let var = |name: &str| {
        std::env::var(name)
            .ok()
            .filter(|v| !v.is_empty())
    };
    match (var("AWS_ACCESS_KEY_ID"), var("AWS_SECRET_ACCESS_KEY")) {
        (Some(access_key), Some(secret_key)) => Ok(Some(Credentials {
            access_key,
            secret_key,
            session_token: var("AWS_SESSION_TOKEN"),
        })),
        (None, None) => Ok(file_credentials(layers, fleet)),
        _ => Err(ConfigError::Usage(
            "set both AWS_ACCESS_KEY_ID and AWS_SECRET_ACCESS_KEY, or neither".into(),
        )),
    }
}

fn file_credentials(layers: &[Layer], fleet: &str) -> Option<Credentials> {
    for name in [fleet, DEFAULT_CREDENTIALS] {
        for layer in layers
            .iter()
            .rev()
        {
            if let Some(c) = layer
                .config
                .credentials
                .get(name)
            {
                debug!(
                    "credentials.{name} from {}",
                    layer
                        .path
                        .display()
                );
                return Some(Credentials {
                    access_key: c
                        .access_key
                        .clone(),
                    secret_key: c
                        .secret_key
                        .clone(),
                    session_token: None,
                });
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layer(path: &str, yaml: &str) -> Layer {
        Layer {
            path: PathBuf::from(path),
            config: serde_norway::from_str(yaml).unwrap(),
        }
    }

    const KEYS: &str = "credentials:\n  default:\n    access_key: D\n    secret_key: d\n";

    #[test]
    fn mode_check() {
        for (mode, ok) in [
            (0o600, true),
            (0o640, true),
            (0o400, true),
            (0o644, false),
            (0o660, false),
            (0o602, false),
        ] {
            assert_eq!(check_mode(Path::new("x"), mode).is_ok(), ok, "{mode:04o}");
        }
    }

    #[test]
    fn unknown_config_keys_are_refused() {
        assert!(serde_norway::from_str::<FileConfig>("endpont: x\n").is_err());
        assert!(
            serde_norway::from_str::<FileConfig>("credentials:\n  default:\n    access_key: a\n")
                .is_err()
        );
    }

    #[test]
    fn specific_credentials_beat_default_across_files() {
        let layers = [
            layer(
                "/etc",
                "credentials:\n  prod:\n    access_key: P\n    secret_key: p\n",
            ),
            layer("/home", KEYS),
        ];
        let creds = file_credentials(&layers, "prod").unwrap();
        assert_eq!(creds.access_key, "P");
        let creds = file_credentials(&layers, "test").unwrap();
        assert_eq!(creds.access_key, "D");
    }

    #[tokio::test]
    async fn bucket_is_the_flag_else_the_fleet() {
        let layers = [layer("/home", "endpoint: s3\n")];
        let t = resolve_target(None, None, None, &layers, "odf-prod")
            .await
            .unwrap();
        assert_eq!(t.bucket, "odf-prod");
        let t = resolve_target(None, None, Some("acme-odf-prod"), &layers, "odf-prod")
            .await
            .unwrap();
        assert_eq!(t.bucket, "acme-odf-prod");
    }

    #[tokio::test]
    async fn endpoint_layers() {
        let layers = [
            layer("/etc", "endpoint: etc.s3\nregion: e\n"),
            layer("/home", "endpoint: home.s3\nregion: h\n"),
        ];
        let (e, src, _) = resolve_endpoint(None, None, &layers, "f")
            .await
            .unwrap();
        assert_eq!(e.authority, "home.s3");
        assert!(matches!(src, EndpointSource::File(p) if p == Path::new("/home")));

        let (e, src, _) = resolve_endpoint(Some("[::1]:7480"), Some("r"), &layers, "f")
            .await
            .unwrap();
        assert_eq!(e.authority, "[::1]:7480");
        assert!(matches!(src, EndpointSource::CommandLine));

        let (e, _, _) = resolve_endpoint(Some("x"), None, &layers, "f")
            .await
            .unwrap();
        assert_eq!(e.region, DEFAULT_REGION, "flag endpoint, default region");
        let bare = [layer("/home", "endpoint: x\n")];
        let (e, _, _) = resolve_endpoint(None, None, &bare, "f")
            .await
            .unwrap();
        assert_eq!(e.region, DEFAULT_REGION, "file endpoint, default region");

        let stray = [layer("/etc", "region: r\n")];
        assert!(
            resolve_endpoint(None, None, &stray, "f")
                .await
                .is_err(),
            "a file region with no endpoint would be ignored"
        );
        assert!(
            resolve_endpoint(None, Some("r"), &layers, "f")
                .await
                .is_err(),
            "a flag region with no endpoint would be ignored"
        );
    }
}
