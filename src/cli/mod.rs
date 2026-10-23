//! Command definitions, configuration layering, and the read and edit paths
//! every command goes through.
//!
//! Settings resolve per field, lowest to highest: DNS discovery,
//! `/etc/s3-fleet.yaml`, `$XDG_CONFIG_HOME/s3-fleet.yaml`, environment, flags.
//! `--config PATH` replaces both files with that one, so a run can be isolated
//! from the machine's. The endpoint and region resolve as a pair from the
//! highest layer that sets an endpoint, so a file's region is never signed
//! against DNS's endpoint. The bucket is `--bucket`, else the per-fleet
//! record's `bucket=` when the endpoint was discovered, else the fleet name;
//! the local cache is keyed by fleet name either way.
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
use s3_fleet::s3::S3;
use s3_fleet::selector::Selector;
use s3_fleet::{Error, Fleet, FleetDoc};
use tracing::{debug, warn};

use config::{Layer, Target};
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
    /// Bucket holding the fleet, over a DNS record's bucket= [default: the fleet name]
    #[arg(long, env = "S3_FLEET_BUCKET", global = true)]
    pub bucket: Option<String>,
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
    ///
    /// Prompts for the access key and secret, checks them against the
    /// endpoint, and saves them mode 0600 in $XDG_CONFIG_HOME/s3-fleet.yaml,
    /// under the fleet's name, or as the default without --fleet. Refuses
    /// without a terminal; scripts use AWS_ACCESS_KEY_ID and
    /// AWS_SECRET_ACCESS_KEY instead.
    Setup,
    /// Create the fleet's bucket and an empty fleet document
    ///
    /// Needs credentials. A bucket the same credentials already own is
    /// reused, and an existing document is left untouched.
    Init,
    /// List hosts, one per line, or as JSON
    ///
    /// Reads the fleet from S3 and refreshes the local cache.
    List {
        /// Label selector: k=v, k!=v, k, !k, comma-separated; repeat to AND
        #[arg(short = 'l', long = "selector")]
        selectors: Vec<Selector>,
        /// JSON output (format 1)
        #[arg(long)]
        json: bool,
    },
    /// Show one host
    ///
    /// Reads the fleet from S3 and refreshes the local cache; exits 1 when
    /// the host is not in the fleet.
    Show {
        #[arg(add = ArgValueCandidates::new(commands::cached_hosts))]
        host: String,
        /// JSON output (format 1)
        #[arg(long)]
        json: bool,
    },
    /// Keyscan hosts and pin their keys (all or nothing unless --keep-going)
    ///
    /// Runs ssh-keyscan on each host and commits them in one conditional
    /// write. A host already in the fleet whose key changed is refused with
    /// exit 7 unless --rekey.
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
    ///
    /// The removals are committed in one conditional write.
    Rm {
        #[arg(required = true, add = ArgValueCandidates::new(commands::cached_hosts))]
        hosts: Vec<String>,
        /// Commit the removals that succeeded even if others failed (exit 3)
        #[arg(long)]
        keep_going: bool,
    },
    /// Set (k=v) or remove (k-) labels: label HOST... CHANGE...
    ///
    /// The arguments that hold = or end in - are changes, the others hosts;
    /// every change applies to every host, in one conditional write.
    Label {
        #[arg(required = true, num_args = 2.., add = ArgValueCandidates::new(commands::cached_hosts))]
        args: Vec<String>,
    },
    /// Fleet-wide attributes
    ///
    /// Key-value pairs stored with the fleet rather than with a host, such
    /// as settings shared by every tool reading it.
    Attr {
        #[command(subcommand)]
        command: AttrCommand,
    },
    /// Print known_hosts lines for ssh -o UserKnownHostsFile or KnownHostsCommand
    ///
    /// Reads the fleet from S3 and refreshes the local cache, unless
    /// --cached: then only the local copy is read and the network never is,
    /// which is what a KnownHostsCommand needs, since ssh may run it three
    /// times per connection.
    ///
    /// With --cached, a missing cache or an unknown --host exits 1, and ssh
    /// then reports "KnownHostsCommand failed" and exits 255. Any command
    /// that reads the fleet from S3 fills the cache: list, show, ssh,
    /// known-hosts without --cached, add, rm, label and attr. Run
    /// s3-fleet -f FLEET list >/dev/null once on a new machine, and again
    /// after the fleet changes.
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
    ///
    /// Reads the fleet from S3 once, which refreshes the local cache, then
    /// replaces itself with the system ssh (exec), adding: -o
    /// KnownHostsCommand calling s3-fleet known-hosts --cached --host %H,
    /// -o UserKnownHostsFile=/dev/null, -o GlobalKnownHostsFile=/dev/null, -o
    /// StrictHostKeyChecking=yes, -o CheckHostIP=no, -p when the fleet gives
    /// the host a port, and -l USER for USER@HOST. With S3 unreachable it
    /// fails, unless --allow-stale.
    ///
    /// Exit status: s3-fleet's own codes only when it fails before ssh
    /// starts. Once ssh runs, the status is ssh's: the remote command's, or
    /// 255 for an ssh error, so a remote exit of 1 to 8 looks like one of
    /// s3-fleet's. A caller that must tell them apart runs s3-fleet -f
    /// FLEET show HOST >/dev/null first, then plain ssh with the
    /// KnownHostsCommand line shown in s3-fleet(1).
    ///
    /// Arguments after HOST go to ssh after the destination. OpenSSH still
    /// reads options there, then takes the rest as the remote command, as in
    /// s3-fleet -f FLEET ssh HOST -o ControlMaster=auto -o
    /// ControlPath=~/.ssh/cm-%C -o ControlPersist=60 uptime
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
    /// Where it was read; unknown for a cached copy.
    pub target: Option<Target>,
}

/// The resolved global options, shared by every command.
pub struct Ctx {
    pub fleet: String,
    endpoint: Option<String>,
    region: Option<String>,
    bucket: Option<String>,
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
            bucket: cli
                .bucket
                .clone(),
            layers: config::load_layers(
                cli.config
                    .as_deref(),
                matches!(cli.command, Command::Setup),
            )?,
            allow_stale: cli.allow_stale,
            timeout: Duration::from_secs(cli.timeout),
        })
    }

    pub fn timeout(&self) -> Duration {
        self.timeout
    }

    pub async fn target(&self) -> anyhow::Result<Target> {
        let target = config::resolve_target(
            self.endpoint
                .as_deref(),
            self.region
                .as_deref(),
            self.bucket
                .as_deref(),
            &self.layers,
            &self.fleet,
        )
        .await?;
        debug!(
            "endpoint {} region {} bucket {} from {}",
            target
                .endpoint
                .authority,
            target
                .endpoint
                .region,
            target.bucket,
            target.source
        );
        Ok(target)
    }

    /// A fleet handle; signed when credentials resolve, else anonymous.
    pub fn handle(&self, target: &Target) -> anyhow::Result<Fleet> {
        let credentials = config::resolve_credentials(&self.layers, &self.fleet)?;
        let s3 = S3::new(
            target
                .endpoint
                .clone(),
            credentials,
            self.timeout,
        )?;
        Ok(Fleet::with_bucket(&self.fleet, &target.bucket, s3)?)
    }

    /// Like `handle`, but fails before any work when there are no credentials.
    pub fn writer(&self, target: &Target) -> anyhow::Result<Fleet> {
        let fleet = self.handle(target)?;
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
            Ok((doc, target)) => {
                self.store(&doc);
                return Ok(Read {
                    doc,
                    source: Source::S3,
                    target: Some(target),
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
            target: None,
        })
    }

    async fn read_live(&self) -> anyhow::Result<(FleetDoc, Target)> {
        let target = self
            .target()
            .await?;
        let snapshot = self
            .handle(&target)?
            .load()
            .await?;
        Ok((snapshot.doc, target))
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
