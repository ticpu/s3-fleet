//! Discovery through a real resolver against a private dnsmasq on loopback:
//! the search list, the per-fleet record winning over the shared one, and the
//! not-found and ambiguous cases. Skipped when dnsmasq is not installed.

use std::io::{BufRead, BufReader};
use std::net::{IpAddr, Ipv4Addr, UdpSocket};
use std::process::{Child, Command, Stdio};

use hickory_resolver::TokioResolver;
use hickory_resolver::config::{NameServerConfig, ResolverConfig};
use hickory_resolver::net::runtime::TokioRuntimeProvider;
use hickory_resolver::proto::rr::Name;
use s3_fleet::discover::{DiscoverError, discover_with};

struct Dnsmasq(Child);

impl Drop for Dnsmasq {
    fn drop(&mut self) {
        // Best effort: the child is ours and already doomed.
        let _ = self
            .0
            .kill();
        let _ = self
            .0
            .wait();
    }
}

/// Starts dnsmasq on a free loopback port; `None` when it is not installed.
fn dnsmasq() -> Option<(Dnsmasq, u16)> {
    let port = UdpSocket::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let spawned = Command::new("dnsmasq")
        .args([
            "--keep-in-foreground",
            "--conf-file=/dev/null",
            "--no-resolv",
            "--no-hosts",
            "--pid-file=",
            "--log-facility=-",
            "--bind-interfaces",
            "--listen-address=127.0.0.1",
            &format!("--port={port}"),
            "--local=/#/",
            "--txt-record=_s3-fleet.prod.site.test,v=s3-fleet1 endpoint=prod.s3.test region=p",
            "--txt-record=_s3-fleet.site.test,v=s3-fleet1 endpoint=shared.s3.test region=s",
            "--txt-record=_s3-fleet.site.test,unrelated text",
            "--txt-record=_s3-fleet.dup.test,v=s3-fleet1 endpoint=a region=a",
            "--txt-record=_s3-fleet.dup.test,v=s3-fleet1 endpoint=b region=b",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn();
    let mut child = match spawned {
        Ok(child) => child,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            eprintln!("skipped: dnsmasq not installed");
            return None;
        }
        Err(e) => panic!("spawn dnsmasq: {e}"),
    };
    let stderr = child
        .stderr
        .take()
        .unwrap();
    let guard = Dnsmasq(child);
    for line in BufReader::new(stderr).lines() {
        let line = line.unwrap();
        if line.contains("started") {
            return Some((guard, port));
        }
    }
    panic!("dnsmasq exited before it started");
}

fn resolver(port: u16, search: &[&str]) -> TokioResolver {
    let mut server = NameServerConfig::udp(IpAddr::V4(Ipv4Addr::LOCALHOST));
    server.connections[0].port = port;
    let search = search
        .iter()
        .map(|s| Name::from_ascii(s).unwrap())
        .collect();
    let config = ResolverConfig::from_parts(None, search, vec![server]);
    TokioResolver::builder_with_config(config, TokioRuntimeProvider::default())
        .build()
        .unwrap()
}

#[tokio::test]
async fn per_fleet_record_wins_then_shared_record() {
    let Some((_dns, port)) = dnsmasq() else {
        return;
    };
    let r = resolver(port, &["site.test"]);

    let prod = discover_with(&r, "prod")
        .await
        .unwrap();
    assert_eq!(
        prod.endpoint
            .authority,
        "prod.s3.test"
    );
    assert_eq!(
        prod.endpoint
            .region,
        "p"
    );
    assert_eq!(prod.record, "_s3-fleet.prod.site.test.");

    let other = discover_with(&r, "other")
        .await
        .unwrap();
    assert_eq!(
        other
            .endpoint
            .authority,
        "shared.s3.test"
    );
    assert_eq!(
        other.record, "_s3-fleet.site.test.",
        "unrelated TXT ignored"
    );
}

#[tokio::test]
async fn no_record_names_what_was_queried() {
    let Some((_dns, port)) = dnsmasq() else {
        return;
    };
    let r = resolver(port, &["empty.test"]);
    match discover_with(&r, "prod").await {
        Err(DiscoverError::NotFound { queried }) => {
            assert_eq!(queried, ["_s3-fleet.prod", "_s3-fleet"]);
        }
        other => panic!("expected NotFound, got {other:?}"),
    }
}

#[tokio::test]
async fn two_records_are_ambiguous() {
    let Some((_dns, port)) = dnsmasq() else {
        return;
    };
    let r = resolver(port, &["dup.test"]);
    let err = discover_with(&r, "prod").await;
    assert!(
        matches!(err, Err(DiscoverError::Ambiguous { count: 2, .. })),
        "{err:?}"
    );
}
