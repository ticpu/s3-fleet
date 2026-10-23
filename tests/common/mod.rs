//! Settings for the tests that run against a real S3 service.
//!
//! They run only when `S3_FLEET_TEST_ENDPOINT` is set, together with
//! `S3_FLEET_TEST_REGION`, `S3_FLEET_TEST_BUCKET`, `AWS_ACCESS_KEY_ID` and
//! `AWS_SECRET_ACCESS_KEY`. The bucket is created if missing and is never
//! deleted; each test works under its own random key prefix and removes what
//! it wrote.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use s3_fleet::s3::{Credentials, Endpoint, S3};

pub struct Live {
    pub endpoint: Endpoint,
    pub bucket: String,
    pub credentials: Credentials,
}

/// The live settings, or `None` (with a note on stderr) to skip the test.
pub fn live(test: &str) -> Option<Live> {
    let Ok(authority) = std::env::var("S3_FLEET_TEST_ENDPOINT") else {
        eprintln!("{test}: skipped, S3_FLEET_TEST_ENDPOINT unset");
        return None;
    };
    let var = |name: &str| std::env::var(name).unwrap_or_else(|_| panic!("{name} unset"));
    Some(Live {
        endpoint: Endpoint {
            authority,
            region: var("S3_FLEET_TEST_REGION"),
        },
        bucket: var("S3_FLEET_TEST_BUCKET"),
        credentials: Credentials {
            access_key: var("AWS_ACCESS_KEY_ID"),
            secret_key: var("AWS_SECRET_ACCESS_KEY"),
            session_token: None,
        },
    })
}

impl Live {
    pub fn signed(&self) -> S3 {
        S3::new(
            self.endpoint
                .clone(),
            Some(
                self.credentials
                    .clone(),
            ),
            Duration::from_secs(15),
        )
        .unwrap()
    }

    pub fn anonymous(&self) -> S3 {
        S3::new(
            self.endpoint
                .clone(),
            None,
            Duration::from_secs(15),
        )
        .unwrap()
    }
}

/// A key prefix no other run shares.
pub fn unique_prefix(test: &str) -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    format!("it/{test}-{}-{nanos}", std::process::id())
}
