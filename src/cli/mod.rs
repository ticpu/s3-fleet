//! Command definitions, configuration layering, and the read and edit paths
//! every command goes through.
//!
//! Settings resolve per field, lowest to highest: DNS discovery,
//! `/etc/s3-fleet.yaml`, `$XDG_CONFIG_HOME/s3-fleet.yaml`, environment, flags.
//! `--config PATH` replaces both files with that one, so a run can be isolated
//! from the machine's. The endpoint and region resolve as a pair from the
//! highest layer that sets an endpoint, so a file's region is never signed
//! against DNS's endpoint.
//!
//! Credentials resolve first-wins by whole keypair: the AWS environment pair,
//! then `credentials.<fleet>` from any file, then `credentials.default`, each
//! searched from the highest file down, so a key for this fleet beats a
//! default key whichever file holds it. A file that holds credentials must not
//! be group-writable or readable by others; it is refused, not warned about.
//!
//! Every successful read from S3 refreshes the local cache. Reads fall back to
//! the cache only when S3 is unreachable and `--allow-stale` was given; the
//! output then says so (`source`, `age_secs`) and a warning names the age.

pub mod commands;
pub mod config;
pub mod exit;
pub mod man;
pub mod setup;
pub mod ssh;

use std::path::PathBuf;
use std::time::Duration;

use anyhow::Context as _;
use clap::{Parser, Subcommand};
use clap_complete::ArgValueCandidates;
use s3_fleet::cache::Cache;
use s3_fleet::discover::DiscoverError;
use s3_fleet::s3::{Endpoint, S3};
use s3_fleet::selector::Selector;
use s3_fleet::{Error, Fleet, FleetDoc};
use tracing::{debug, warn};

use config::{EndpointSource, Layer};
use exit::{Exit, Failure};

#[derive(Parser)]
#[command(
    version,
    about = "A fleet of SSH hosts kept in S3: pinned host keys, labels, discovery over DNS"
)]
pub struct Cli {
    /// Fleet name, which is also its bucket
    #[arg(long, short = 'f', env = "S3_FLEET", global = true)]
    pub fleet: Option<String>,
    /// S3 endpoint as host[:port]; skips DNS discovery
    #[arg(long, env = "S3_FLEET_ENDPOINT", global = true)]
    pub endpoint: Option<String>,
    /// S3 signing region, with --endpoint [default: us-east-1]
    #[arg(long, env = "S3_FLEET_REGION", global = true)]
    pub region: Option<String>,
    /// Use only this config file instead of /etc and XDG ones
    #[arg(long, global = true)]
    pub config: Option<PathBuf>,
    /// When S3 is unreachable, read the local cache and say how old it is
    #[arg(long, global = true)]
    pub allow_stale: bool,
    /// Per-request timeout in seconds, for S3 and ssh-keyscan
    #[arg(long, global = true, default_value_t = 15, value_parser = clap::value_parser!(u64).range(1..))]
    pub timeout: u64,
    /// More logging on stderr: -v info, -vv debug, -vvv trace (RUST_LOG overrides)
    #[arg(short, long, action = clap::ArgAction::Count, global = true)]
    pub verbose: u8,
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand)]
pub enum Command {
    /// Store S3 credentials for writing (asks on the terminal)
    Setup,
    /// Create the fleet's bucket and an empty fleet document
    Init,
    /// List hosts, one per line, or as JSON
    List {
        /// Label selector: k=v, k!=v, k, !k, comma-separated; repeat to AND
        #[arg(short = 'l', long = "selector")]
        selectors: Vec<Selector>,
        /// JSON output (format 1)
        #[arg(long)]
        json: bool,
    },
    /// Show one host
    Show {
        #[arg(add = ArgValueCandidates::new(commands::cached_hosts))]
        host: String,
        /// JSON output (format 1)
        #[arg(long)]
        json: bool,
    },
    /// Keyscan hosts and pin their keys (all or nothing unless --keep-going)
    Add {
        #[arg(required = true)]
        hosts: Vec<String>,
        /// Label to set on every added host, k=v; repeatable
        #[arg(short = 'l', long = "label")]
        labels: Vec<String>,
        /// SSH port
        #[arg(long, default_value_t = 22)]
        port: u16,
        /// Key type to pin, as ssh-keyscan -t names it; repeatable
        #[arg(long = "key-type", default_value = "ed25519")]
        key_types: Vec<String>,
        /// Accept keys that differ from the pinned ones
        #[arg(long)]
        rekey: bool,
        /// Commit the hosts that succeeded even if others failed (exit 3)
        #[arg(long)]
        keep_going: bool,
    },
    /// Remove hosts (all or nothing unless --keep-going)
    Rm {
        #[arg(required = true, add = ArgValueCandidates::new(commands::cached_hosts))]
        hosts: Vec<String>,
        /// Commit the removals that succeeded even if others failed (exit 3)
        #[arg(long)]
        keep_going: bool,
    },
    /// Set (k=v) or remove (k-) labels: label HOST... CHANGE...
    Label {
        #[arg(required = true, num_args = 2.., add = ArgValueCandidates::new(commands::cached_hosts))]
        args: Vec<String>,
    },
    /// Fleet-wide attributes
    Attr {
        #[command(subcommand)]
        command: AttrCommand,
    },
    /// Print known_hosts lines for ssh -o UserKnownHostsFile or KnownHostsCommand
    KnownHosts {
        /// Label selector, as for list
        #[arg(short = 'l', long = "selector")]
        selectors: Vec<Selector>,
        /// Only this host; accepts name or [name]:port, as ssh's %H gives
        #[arg(long)]
        host: Option<String>,
        /// Read only the local cache, never the network
        #[arg(long)]
        cached: bool,
        /// Write to this file atomically instead of stdout
        #[arg(short, long)]
        output: Option<PathBuf>,
    },
    /// Write the man pages into DIR (packaging)
    #[command(hide = true)]
    Manpages { dir: PathBuf },
    /// Run ssh to a fleet host, trusting only its pinned keys
    Ssh {
        /// [USER@]HOST, a host in the fleet
        #[arg(add = ArgValueCandidates::new(commands::cached_hosts))]
        destination: String,
        /// Passed to ssh after the destination: options, then the remote command
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<std::ffi::OsString>,
    },
}

#[derive(Subcommand)]
pub enum AttrCommand {
    /// Set attributes, k=v
    Set {
        #[arg(required = true)]
        pairs: Vec<String>,
    },
    /// Remove attributes
    Rm {
        #[arg(required = true)]
        keys: Vec<String>,
    },
    /// Print one attribute's value; exit 1 if unset
    Get { key: String },
}

/// Where a document was read from.
pub enum Source {
    S3,
    Cache { age: Duration },
}

/// A document as read, with what the output needs to say about it.
pub struct Read {
    pub doc: FleetDoc,
    pub source: Source,
    pub endpoint: Option<(Endpoint, EndpointSource)>,
}

/// The resolved global options, shared by every command.
pub struct Ctx {
    pub fleet: String,
    endpoint: Option<String>,
    region: Option<String>,
    layers: Vec<Layer>,
    allow_stale: bool,
    timeout: Duration,
}

impl Ctx {
    pub fn new(cli: &Cli) -> anyhow::Result<Self> {
        let fleet = cli
            .fleet
            .clone()
            .ok_or_else(|| Failure::new(Exit::Usage, "no fleet: pass --fleet or set S3_FLEET"))?;
        s3_fleet::fleet::validate_fleet_name(&fleet)?;
        Ok(Self {
            fleet,
            endpoint: cli
                .endpoint
                .clone(),
            region: cli
                .region
                .clone(),
            layers: config::load_layers(
                cli.config
                    .as_deref(),
            )?,
            allow_stale: cli.allow_stale,
            timeout: Duration::from_secs(cli.timeout),
        })
    }

    pub fn timeout(&self) -> Duration {
        self.timeout
    }

    pub async fn endpoint(&self) -> anyhow::Result<(Endpoint, EndpointSource)> {
        let (endpoint, source) = config::resolve_endpoint(
            self.endpoint
                .as_deref(),
            self.region
                .as_deref(),
            &self.layers,
            &self.fleet,
        )
        .await?;
        debug!(
            "endpoint {} region {} from {source}",
            endpoint.authority, endpoint.region
        );
        Ok((endpoint, source))
    }

    /// A fleet handle; signed when credentials resolve, else anonymous.
    pub fn handle(&self, endpoint: &Endpoint) -> anyhow::Result<Fleet> {
        let credentials = config::resolve_credentials(&self.layers, &self.fleet)?;
        let s3 = S3::new(endpoint.clone(), credentials, self.timeout)?;
        Ok(Fleet::new(&self.fleet, s3)?)
    }

    /// Like `handle`, but fails before any work when there are no credentials.
    pub fn writer(&self, endpoint: &Endpoint) -> anyhow::Result<Fleet> {
        let fleet = self.handle(endpoint)?;
        if !fleet
            .s3()
            .has_credentials()
        {
            return Err(Error::CredentialsRequired {
                fleet: self
                    .fleet
                    .clone(),
            })
            .context("set AWS_ACCESS_KEY_ID/AWS_SECRET_ACCESS_KEY or run s3-fleet setup");
        }
        Ok(fleet)
    }

    /// Read the document from S3, refreshing the cache; with --allow-stale,
    /// fall back to the cache when S3 cannot be reached.
    pub async fn read(&self) -> anyhow::Result<Read> {
        let err = match self
            .read_live()
            .await
        {
            Ok((doc, endpoint)) => {
                self.store(&doc);
                return Ok(Read {
                    doc,
                    source: Source::S3,
                    endpoint: Some(endpoint),
                });
            }
            Err(err) if self.allow_stale && is_unreachable(&err) => err,
            Err(err) => return Err(err),
        };
        let cached = match self
            .cache()?
            .load(&self.fleet)
        {
            Ok(cached) => cached,
            Err(cache_err) => {
                return Err(Failure::new(
                    Exit::Unreachable,
                    format!("{err:#}; and no usable cache: {cache_err:#}"),
                )
                .into());
            }
        };
        warn!(
            "{err:#}; using the cached fleet {}, {}s old",
            self.fleet,
            cached
                .age
                .as_secs()
        );
        Ok(Read {
            doc: cached.doc,
            source: Source::Cache { age: cached.age },
            endpoint: None,
        })
    }

    async fn read_live(&self) -> anyhow::Result<(FleetDoc, (Endpoint, EndpointSource))> {
        let endpoint = self
            .endpoint()
            .await?;
        let snapshot = self
            .handle(&endpoint.0)?
            .load()
            .await?;
        Ok((snapshot.doc, endpoint))
    }

    pub fn cache(&self) -> anyhow::Result<Cache> {
        Ok(Cache::xdg()?)
    }

    /// Refresh the local copy; a failure costs only staleness, so it is
    /// reported and the command goes on.
    pub fn store(&self, doc: &FleetDoc) {
        let stored = self
            .cache()
            .and_then(|cache| Ok(cache.store(&self.fleet, doc)?));
        if let Err(e) = stored {
            warn!("updating the local cache of fleet {}: {e:#}", self.fleet);
        }
    }
}

/// Whether a read failed for want of a network, the one case --allow-stale
/// covers: S3 unreachable, or the DNS query for discovery failing. A missing
/// record or a refused request is not covered: the cache would hide it.
fn is_unreachable(err: &anyhow::Error) -> bool {
    exit::exit_code(err) == Exit::Unreachable
        || err
            .chain()
            .any(|c| {
                matches!(
                    c.downcast_ref::<DiscoverError>(),
                    Some(DiscoverError::Lookup { .. } | DiscoverError::Resolver(_))
                )
            })
}
