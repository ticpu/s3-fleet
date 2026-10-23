//! Man pages, rendered from the clap definition by the hidden `manpages DIR`
//! subcommand at packaging time: s3-fleet(1) and one s3-fleet-<command>(1)
//! per command. The options and commands come from clap, so they cannot
//! drift from the binary; the main page adds the sections clap has no
//! notion of, with the exit codes taken from [`Exit::meaning`].

use std::fs;
use std::path::Path;

use anyhow::Context as _;
use clap::CommandFactory as _;
use clap_mangen::Man;
use clap_mangen::roff::{Roff, bold, italic, roman};

use super::Cli;
use super::exit::Exit;

pub fn generate(dir: &Path) -> anyhow::Result<()> {
    fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
    let cmd = Cli::command();
    clap_mangen::generate_to(cmd.clone(), dir)
        .with_context(|| format!("write man pages to {}", dir.display()))?;
    let path = dir.join("s3-fleet.1");
    let mut out = Vec::new();
    main_page(cmd, &mut out)?;
    fs::write(&path, out).with_context(|| format!("write {}", path.display()))
}

fn main_page(cmd: clap::Command, out: &mut Vec<u8>) -> anyhow::Result<()> {
    let man = Man::new(cmd);
    man.render_title(out)?;
    man.render_name_section(out)?;
    man.render_synopsis_section(out)?;
    man.render_description_section(out)?;
    man.render_options_section(out)?;
    man.render_subcommands_section(out)?;
    extra_sections().to_writer(out)?;
    man.render_version_section(out)?;
    Ok(())
}

fn section(roff: &mut Roff, title: &str) {
    roff.control("SH", [title]);
}

fn entry(roff: &mut Roff, term: &str, text: &str) {
    roff.control("TP", []);
    roff.text([bold(term)]);
    roff.text([roman(text)]);
}

fn extra_sections() -> Roff {
    let mut roff = Roff::new();

    section(&mut roff, "DISCOVERY");
    roff.text([
        roman(
            "A fleet is a bucket named after it. Without --endpoint, S3_FLEET_ENDPOINT or an \
             endpoint in a config file, the endpoint comes from a DNS TXT record: ",
        ),
        bold("_s3-fleet.FLEET"),
        roman(" first, then "),
        bold("_s3-fleet"),
        roman(", both relative, so the resolver's search list applies. The record reads"),
    ]);
    roff.control("IP", []);
    roff.text([bold("v=s3-fleet1 endpoint=HOST[:PORT] [region=REGION]")]);
    roff.control("PP", []);
    roff.text([roman(
        "The region defaults to us-east-1; radosgw accepts any. Reads are anonymous; \
         writes need credentials.",
    )]);

    section(&mut roff, "EXIT STATUS");
    for code in Exit::ALL {
        entry(&mut roff, &(code as u8).to_string(), code.meaning());
    }

    section(&mut roff, "ENVIRONMENT");
    for (name, text) in [
        ("S3_FLEET", "the fleet, as --fleet"),
        ("S3_FLEET_ENDPOINT", "the S3 endpoint, as --endpoint"),
        ("S3_FLEET_REGION", "the signing region, as --region"),
        (
            "AWS_ACCESS_KEY_ID, AWS_SECRET_ACCESS_KEY, AWS_SESSION_TOKEN",
            "write credentials; they win over the config files",
        ),
        ("XDG_CONFIG_HOME", "where the user's config file is"),
        ("XDG_CACHE_HOME", "where the local copies of fleets are"),
        (
            "SSL_CERT_FILE, HTTPS_PROXY, NO_PROXY",
            "honoured as by any client using the system trust store",
        ),
        ("RUST_LOG", "log filter, overriding -v"),
        ("NO_COLOR", "turns colour off in logs"),
        (
            "COMPLETE",
            "COMPLETE=bash s3-fleet prints the shell completion script (also zsh, fish)",
        ),
    ] {
        entry(&mut roff, name, text);
    }

    section(&mut roff, "FILES");
    for (name, text) in [
        (
            "/etc/s3-fleet.yaml",
            "system config: endpoint, region, credentials",
        ),
        (
            "$XDG_CONFIG_HOME/s3-fleet.yaml",
            "user config, over the system one; refused if it holds credentials others can read",
        ),
        (
            "$XDG_CACHE_HOME/s3-fleet/FLEET.yaml",
            "the last copy read from S3, for --allow-stale, known-hosts --cached and completion",
        ),
    ] {
        entry(&mut roff, name, text);
    }

    section(&mut roff, "EXAMPLES");
    for (command, text) in [
        (
            "s3-fleet -f prod-sbc add sbc1.example.net -l env=prod -l role=sbc",
            "pin a host's ed25519 key and label it",
        ),
        (
            "s3-fleet -f prod-sbc list -l role=sbc,env!=test",
            "names of matching hosts",
        ),
        (
            "s3-fleet -f prod-sbc ssh sbc1.example.net uptime",
            "ssh trusting only the pinned key",
        ),
        (
            "ssh -o KnownHostsCommand='s3-fleet -f prod-sbc known-hosts --cached --host %H' \
             -o UserKnownHostsFile=/dev/null -o GlobalKnownHostsFile=/dev/null \
             -o StrictHostKeyChecking=yes -o CheckHostIP=no HOST",
            "the same from plain ssh",
        ),
    ] {
        roff.control("TP", []);
        roff.text([bold(command)]);
        roff.text([roman(text)]);
    }

    section(&mut roff, "SEE ALSO");
    roff.text([
        italic("ssh"),
        roman("(1), "),
        italic("ssh-keyscan"),
        roman("(1), "),
        italic("ssh_config"),
        roman("(5)"),
    ]);
    roff
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn main_page_has_every_exit_code_and_command() {
        let mut out = Vec::new();
        main_page(Cli::command(), &mut out).unwrap();
        // The two escapes roff uses in this text: hyphen and apostrophe.
        let page = String::from_utf8(out)
            .unwrap()
            .replace("\\-", "-")
            .replace("\\*(Aq", "'");
        for code in Exit::ALL {
            assert!(
                page.contains(code.meaning()),
                "exit {}: missing",
                code as u8
            );
        }
        for command in ["known-hosts", "ssh", "setup", "label"] {
            assert!(page.contains(command), "{command} missing");
        }
        assert!(
            !page.contains("manpages"),
            "the hidden command is not documented"
        );
    }
}
