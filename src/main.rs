//! `s3-fleet`: the command-line face of the library, for scripts and people.
//!
//! # Contract with scripts
//!
//! - stdout carries data only: host names one per line, or `--json`
//!   (`"format": 1`), to which fields may be added but whose existing fields
//!   change only with that number. Diagnostics go to stderr as
//!   `s3-fleet: <subject>: <error chain>`.
//! - The exit code says what happened; see `cli::exit` for the table. A
//!   caller can tell "someone else wrote, rerun" from "the key changed" from
//!   "S3 is down" without reading stderr.
//! - Nothing prompts except `setup`, and `setup` refuses to run without a TTY.
//!   No other command reads stdin.
//! - Logs go to stderr through tracing; `-v` raises the level, `RUST_LOG`
//!   overrides it, and `NO_COLOR` turns colour off.
//! - Shell completion: `source <(COMPLETE=bash s3-fleet)` (or zsh, fish);
//!   host names come from the local cache only.

mod cli;

use std::io::IsTerminal as _;
use std::process::ExitCode;

use clap::{CommandFactory as _, Parser as _};
use clap_complete::CompleteEnv;
use tracing_subscriber::EnvFilter;

use cli::exit::{Exit, exit_code};
use cli::{Cli, Command, Ctx, commands, man, setup, ssh};

fn main() -> ExitCode {
    CompleteEnv::with_factory(Cli::command).complete();
    let cli = Cli::parse();
    init_logging(cli.verbose);

    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("s3-fleet: start the async runtime: {e}");
            return ExitCode::from(Exit::Error as u8);
        }
    };
    let exit = match runtime.block_on(run(&cli)) {
        Ok(exit) => exit,
        Err(err) => {
            eprintln!("s3-fleet: {err:#}");
            if let Some(hint) = hint(&cli, &err) {
                eprintln!("s3-fleet: {hint}");
            }
            exit_code(&err)
        }
    };
    ExitCode::from(exit as u8)
}

async fn run(cli: &Cli) -> anyhow::Result<Exit> {
    if let Command::Manpages { dir } = &cli.command {
        man::generate(dir)?;
        return Ok(Exit::Ok);
    }
    let ctx = Ctx::new(cli)?;
    match &cli.command {
        Command::Setup => {
            setup::run(
                &ctx,
                cli.config
                    .as_deref(),
            )
            .await
        }
        Command::Init => commands::init(&ctx).await,
        Command::List { selectors, json } => commands::list(&ctx, selectors, *json).await,
        Command::Show { host, json } => commands::show(&ctx, host, *json).await,
        Command::Add {
            hosts,
            labels,
            port,
            key_types,
            rekey,
            keep_going,
        } => commands::add(&ctx, hosts, labels, *port, key_types, *rekey, *keep_going).await,
        Command::Rm { hosts, keep_going } => commands::rm(&ctx, hosts, *keep_going).await,
        Command::Label { args } => commands::label(&ctx, args).await,
        Command::Attr { command } => commands::attr(&ctx, command).await,
        Command::KnownHosts {
            selectors,
            host,
            cached,
            output,
        } => {
            commands::known_hosts(&ctx, selectors, host.as_deref(), *cached, output.as_deref())
                .await
        }
        Command::Ssh { destination, args } => {
            ssh::run(
                &ctx,
                cli.config
                    .as_deref(),
                destination,
                args,
            )
            .await
        }
        Command::Manpages { .. } => unreachable!("handled before the fleet is resolved"),
    }
}

/// The next command to run, for a failure that has an obvious one.
fn hint(cli: &Cli, err: &anyhow::Error) -> Option<String> {
    let missing = err
        .chain()
        .find_map(|c| match c.downcast_ref::<s3_fleet::Error>() {
            Some(s3_fleet::Error::NotFound { fleet, .. }) => Some(fleet),
            _ => None,
        })?;
    if matches!(cli.command, Command::Init) {
        return None;
    }
    Some(format!("create it with: s3-fleet -f {missing} init"))
}

/// Only s3-fleet's own events get the `-v` level; dependencies stay at warn.
fn init_logging(verbose: u8) {
    let filter = match std::env::var_os("RUST_LOG") {
        Some(_) => EnvFilter::from_default_env(),
        None => {
            let level = match verbose {
                0 => "warn",
                1 => "info",
                2 => "debug",
                _ => "trace",
            };
            EnvFilter::new(format!("warn,s3_fleet={level}"))
        }
    };
    let colour = std::io::stderr().is_terminal()
        && std::env::var_os("NO_COLOR").is_none_or(|v| v.is_empty());
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(filter)
        .with_ansi(colour)
        .with_target(false)
        .without_time()
        .init();
}
