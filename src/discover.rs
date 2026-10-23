//! Finding a fleet's S3 endpoint, region and bucket from DNS.
//!
//! The record is a TXT record:
//!
//! ```text
//! _s3-fleet.prod-sbc  IN TXT "v=s3-fleet1 endpoint=s3.amazonaws.com region=eu-west-3 bucket=acme-prod-sbc"
//! _s3-fleet           IN TXT "v=s3-fleet1 endpoint=s3.example.net"
//! ```
//!
//! `region` is optional and defaults to [`DEFAULT_REGION`]; the S3 client's
//! notes say when it matters. `bucket` is optional too, and the bucket is
//! then the fleet name; it exists because bucket names are global on AWS,
//! where a short fleet name is rarely free. It is refused in the shared
//! `_s3-fleet` record, which answers for every fleet without a record of its
//! own and would point them all at one bucket.
//!
//! For fleet `<f>`, `_s3-fleet.<f>` is queried first, then `_s3-fleet`. The
//! per-fleet name lets fleets live on different clusters; the bare one serves
//! every other fleet. Both names are relative: the system resolver applies
//! its search list, so a box in `example.net` finds `_s3-fleet.example.net`
//! with no configuration, and the binary carries no site's domain.
//!
//! The text is space-separated `key=value` pairs; `v=s3-fleet1` must come
//! first. An unknown version, a missing endpoint, a repeated key, or two
//! records answering the same name is an error, never a guess between them.
//! Unknown keys are ignored so that a later version can add some without
//! breaking readers, which is the opposite of the fleet document's rule: a TXT
//! record is never written back.
//!
//! Discovery is the lowest layer. A flag, an environment variable or a config
//! file that names an endpoint wins, and is not combined with a discovered
//! region.

use hickory_resolver::TokioResolver;
use hickory_resolver::net::{DnsError, NetError};
use hickory_resolver::proto::rr::RData;

use crate::fleet::is_valid_name;
use crate::s3::{DEFAULT_REGION, Endpoint};

/// The record version this crate understands.
const VERSION_TAG: &str = "v=s3-fleet1";
const VERSION_PREFIX: &str = "v=s3-fleet";

/// An endpoint found in DNS, and the record name that held it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Discovered {
    /// Endpoint and region from the record.
    pub endpoint: Endpoint,
    /// The bucket, from a per-fleet record's `bucket=`; `None` means the
    /// fleet name.
    pub bucket: Option<String>,
    /// The fully-qualified name that answered.
    pub record: String,
}

/// Why discovery failed.
#[derive(Debug, thiserror::Error)]
pub enum DiscoverError {
    /// The system resolver could not be set up.
    #[error("set up the system DNS resolver")]
    Resolver(#[source] NetError),
    /// A query failed for a reason other than "no such record".
    #[error("look up TXT {name}")]
    Lookup {
        /// The name queried.
        name: String,
        /// The resolver's error.
        #[source]
        source: NetError,
    },
    /// Neither name has an s3-fleet record.
    #[error("no s3-fleet TXT record at {} (search list applied); set the endpoint explicitly", queried.join(" or "))]
    NotFound {
        /// The names queried, in order.
        queried: Vec<String>,
    },
    /// A record that does not parse.
    #[error("TXT {name} {text:?}: {reason}")]
    Record {
        /// The name that answered.
        name: String,
        /// The record text.
        text: String,
        /// What is wrong.
        reason: String,
    },
    /// More than one s3-fleet record at one name.
    #[error("TXT {name} holds {count} s3-fleet records; expected one")]
    Ambiguous {
        /// The name that answered.
        name: String,
        /// How many records.
        count: usize,
    },
}

/// The names queried for a fleet, in order.
pub fn record_names(fleet: &str) -> [String; 2] {
    [format!("_s3-fleet.{fleet}"), "_s3-fleet".to_string()]
}

/// Discover a fleet's endpoint with the system resolver configuration.
pub async fn discover(fleet: &str) -> Result<Discovered, DiscoverError> {
    let resolver = TokioResolver::builder_tokio()
        .map_err(DiscoverError::Resolver)?
        .build()
        .map_err(DiscoverError::Resolver)?;
    discover_with(&resolver, fleet).await
}

/// Discover a fleet's endpoint with a caller-built resolver.
pub async fn discover_with(
    resolver: &TokioResolver,
    fleet: &str,
) -> Result<Discovered, DiscoverError> {
    let names = record_names(fleet);
    for (i, name) in names
        .iter()
        .enumerate()
    {
        let shared = i > 0;
        let lookup = match resolver
            .txt_lookup(name.as_str())
            .await
        {
            Ok(lookup) => lookup,
            Err(NetError::Dns(DnsError::NoRecordsFound(_))) => continue,
            Err(source) => {
                return Err(DiscoverError::Lookup {
                    name: name.clone(),
                    source,
                });
            }
        };
        let mut found = Vec::new();
        for record in lookup.answers() {
            let RData::TXT(txt) = &record.data else {
                continue;
            };
            let text: String = txt
                .txt_data
                .iter()
                .map(|part| String::from_utf8_lossy(part))
                .collect();
            if text.starts_with(VERSION_PREFIX) {
                found.push((
                    record
                        .name
                        .to_string(),
                    text,
                ));
            }
        }
        match found.as_slice() {
            [] => continue,
            [(answered, text)] => {
                let invalid = |reason| DiscoverError::Record {
                    name: answered.clone(),
                    text: text.clone(),
                    reason,
                };
                let record = parse_record(text).map_err(invalid)?;
                if shared
                    && record
                        .bucket
                        .is_some()
                {
                    return Err(invalid(
                        "bucket= belongs in a per-fleet record (_s3-fleet.<fleet>); here it would \
                         name the bucket of every fleet"
                            .to_string(),
                    ));
                }
                return Ok(Discovered {
                    endpoint: record.endpoint,
                    bucket: record.bucket,
                    record: answered.clone(),
                });
            }
            many => {
                return Err(DiscoverError::Ambiguous {
                    name: many[0]
                        .0
                        .clone(),
                    count: many.len(),
                });
            }
        }
    }
    Err(DiscoverError::NotFound {
        queried: names.to_vec(),
    })
}

/// What one record says.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Record {
    /// Endpoint, and region or its default.
    pub endpoint: Endpoint,
    /// `bucket=`, when the fleet's bucket is not named after it.
    pub bucket: Option<String>,
}

/// Parse `v=s3-fleet1 endpoint=... [region=...] [bucket=...]`.
pub fn parse_record(text: &str) -> Result<Record, String> {
    let mut fields = text.split_ascii_whitespace();
    match fields.next() {
        Some(VERSION_TAG) => {}
        Some(other) => {
            return Err(format!(
                "unsupported version {other:?}, expected {VERSION_TAG}"
            ));
        }
        None => return Err("empty record".to_string()),
    }
    let mut endpoint = None;
    let mut region = None;
    let mut bucket = None;
    for field in fields {
        let Some((key, value)) = field.split_once('=') else {
            return Err(format!("{field:?} is not key=value"));
        };
        let slot = match key {
            "endpoint" => &mut endpoint,
            "region" => &mut region,
            "bucket" => &mut bucket,
            _ => continue,
        };
        if value.is_empty() {
            return Err(format!("{key} is empty"));
        }
        if slot
            .replace(value.to_string())
            .is_some()
        {
            return Err(format!("{key} given twice"));
        }
    }
    if let Some(bucket) = &bucket
        && !is_valid_name(bucket)
    {
        return Err(format!(
            "bucket {bucket:?} is not 3 to 63 of a-z 0-9 -, starting and ending with a letter or digit"
        ));
    }
    Ok(Record {
        endpoint: Endpoint {
            authority: endpoint.ok_or("no endpoint=")?,
            region: region.unwrap_or_else(|| DEFAULT_REGION.to_string()),
        },
        bucket,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_record() {
        let r =
            parse_record("v=s3-fleet1 endpoint=[2001:db8::1]:7480 region=test future=x").unwrap();
        assert_eq!(
            r.endpoint
                .authority,
            "[2001:db8::1]:7480"
        );
        assert_eq!(
            r.endpoint
                .region,
            "test"
        );
        assert_eq!(r.bucket, None);
        let r = parse_record("v=s3-fleet1 endpoint=s3.example bucket=acme-odf-prod").unwrap();
        assert_eq!(
            r.endpoint
                .region,
            DEFAULT_REGION
        );
        assert_eq!(
            r.bucket
                .as_deref(),
            Some("acme-odf-prod")
        );
    }

    #[test]
    fn rejects_bad_records() {
        for (text, why) in [
            ("", "empty"),
            ("endpoint=e region=r", "no version first"),
            ("v=s3-fleet2 endpoint=e region=r", "unknown version"),
            ("v=s3-fleet1 region=r", "no endpoint"),
            ("v=s3-fleet1 endpoint=e endpoint=f region=r", "repeated key"),
            ("v=s3-fleet1 endpoint= region=r", "empty value"),
            ("v=s3-fleet1 endpoint=e region=r junk", "not key=value"),
            ("v=s3-fleet1 endpoint=e bucket=Acme_Odf", "invalid bucket"),
            ("v=s3-fleet1 endpoint=e bucket=a.b.c", "dotted bucket"),
        ] {
            assert!(parse_record(text).is_err(), "{why}: {text:?} accepted");
        }
    }
}
