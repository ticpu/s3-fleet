//! A minimal S3 client: GET, HEAD, PUT and DELETE of single objects, and
//! bucket creation.
//!
//! Hand-rolled on reqwest and aws-sigv4 rather than aws-sdk-s3 or
//! object_store, both far heavier than these few verbs need. A fleet is one
//! object, so there is no ListObjects. DELETE exists for callers that keep
//! objects of their own beside the fleet document.
//!
//! # Environment
//!
//! The reqwest client keeps its defaults: rustls with the platform verifier,
//! so the system trust store and `SSL_CERT_FILE` apply, and the proxy
//! variables are honoured. Do not switch either off here to make a test pass;
//! isolate the test instead.
//!
//! # Anonymous and signed requests
//!
//! Without [`Credentials`] a request goes out unsigned. Every PUT carries
//! `x-amz-acl: public-read`, which is what lets those anonymous GETs work.
//!
//! radosgw (like AWS) answers an anonymous GET of a missing key with 403, not
//! 404, unless the caller may list the bucket. A 403 is therefore reported as
//! [`S3Error::Forbidden`] with `anonymous` set, never as "absent": the caller
//! cannot tell the two apart and must not be told it can.
//!
//! # Preconditions
//!
//! [`Precondition::IfMatch`] sends the ETag with its surrounding quotes
//! stripped, and [`Precondition::IfNoneMatch`] sends `*`. A 412 becomes
//! [`S3Error::PreconditionFailed`]. The quotes are stripped because radosgw
//! answers 412 to a quoted ETag that matches, so every conditional update would
//! look like a conflict; it honours the bare value. RFC 9110 wants the quoted
//! form, so re-check this against any other S3 implementation: the live tests
//! in tests/s3_live.rs fail on either mistake, a refused match or an accepted
//! stale write.
//!
//! The endpoint is an authority (`host[:port]`, a bracketed IPv6 literal
//! allowed) used verbatim in the URL and the signed Host header, never parsed.
//! Requests are path-style: `https://<endpoint>/<bucket>/<key>`.

use std::fmt;
use std::time::{Duration, Instant, SystemTime};

use aws_credential_types::Credentials as AwsCredentials;
use aws_sigv4::http_request::{
    PayloadChecksumKind, PercentEncodingMode, SignableBody, SignableRequest, SigningSettings,
    UriPathNormalizationMode, sign,
};
use aws_sigv4::sign::v4;
use aws_smithy_runtime_api::client::identity::Identity;
use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, utf8_percent_encode};
use reqwest::header::{ETAG, HeaderName, HeaderValue};
use reqwest::{Method, StatusCode};
use tracing::debug;

/// RFC 3986 unreserved characters pass through; `/` stays a key separator.
const KEY_ENCODE: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'.')
    .remove(b'_')
    .remove(b'~')
    .remove(b'/');

/// Error bodies are kept for diagnostics, capped so a proxy's HTML page does
/// not flood stderr.
const ERROR_BODY_CAP: usize = 1024;

/// Where an S3 service is and which region requests are signed for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Endpoint {
    /// `host[:port]`, a bracketed IPv6 literal allowed; used verbatim.
    pub authority: String,
    /// Signing region; radosgw calls it the zonegroup's api_name.
    pub region: String,
}

/// An S3 access keypair, with an optional STS session token.
#[derive(Clone)]
pub struct Credentials {
    /// Access key id.
    pub access_key: String,
    /// Secret access key.
    pub secret_key: String,
    /// Session token for temporary credentials.
    pub session_token: Option<String>,
}

impl fmt::Debug for Credentials {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Credentials")
            .field("access_key", &self.access_key)
            .field("secret_key", &"<redacted>")
            .field(
                "session_token",
                &self
                    .session_token
                    .as_ref()
                    .map(|_| "<redacted>"),
            )
            .finish()
    }
}

/// An object's content and the ETag it was read at.
#[derive(Clone, Debug)]
pub struct Object {
    /// The object's bytes.
    pub body: Vec<u8>,
    /// The ETag header as received, quotes included; `None` if absent.
    pub etag: Option<String>,
}

/// A condition the store checks before accepting a PUT.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Precondition {
    /// Write only if the current object has this ETag.
    IfMatch(String),
    /// Write only if no object exists under the key.
    IfNoneMatch,
}

/// A failed S3 request.
#[derive(Debug, thiserror::Error)]
pub enum S3Error {
    /// The request never got an HTTP response: DNS, connect, TLS, timeout.
    #[error("{method} {url}")]
    Transport {
        /// HTTP method.
        method: Method,
        /// Request URL.
        url: String,
        /// What reqwest reported.
        #[source]
        source: reqwest::Error,
    },
    /// The HTTP client could not be built.
    #[error("build HTTP client")]
    Client(#[source] reqwest::Error),
    /// The request could not be signed.
    #[error("sign {method} {url}")]
    Signing {
        /// HTTP method.
        method: Method,
        /// Request URL.
        url: String,
        /// What aws-sigv4 reported.
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },
    /// A write was attempted without credentials.
    #[error("{method} {url}: credentials required")]
    CredentialsRequired {
        /// HTTP method.
        method: Method,
        /// Request URL.
        url: String,
    },
    /// HTTP 403. When `anonymous`, the object may also simply not exist.
    #[error("{method} {url}: HTTP 403{}{}", describe(code.as_deref(), body), anonymous_hint(*anonymous))]
    Forbidden {
        /// HTTP method.
        method: Method,
        /// Request URL.
        url: String,
        /// Whether the request was unsigned.
        anonymous: bool,
        /// S3 error code from the body, such as `AccessDenied`.
        code: Option<String>,
        /// Response body, capped.
        body: String,
    },
    /// HTTP 404.
    #[error("{method} {url}: HTTP 404{}", describe(code.as_deref(), body))]
    NotFound {
        /// HTTP method.
        method: Method,
        /// Request URL.
        url: String,
        /// S3 error code from the body, such as `NoSuchKey` or `NoSuchBucket`.
        code: Option<String>,
        /// Response body, capped.
        body: String,
    },
    /// HTTP 412, or a 409 `ConditionalRequestConflict`: another writer won.
    #[error("{method} {url}: precondition failed, the object changed")]
    PreconditionFailed {
        /// HTTP method.
        method: Method,
        /// Request URL.
        url: String,
    },
    /// The bucket name is taken by another account.
    #[error("bucket {bucket} already exists and belongs to another account")]
    BucketOwnedByOther {
        /// Bucket name.
        bucket: String,
    },
    /// Any other non-success status.
    #[error("{method} {url}: HTTP {status}{}", describe(code.as_deref(), body))]
    Status {
        /// HTTP method.
        method: Method,
        /// Request URL.
        url: String,
        /// Response status.
        status: StatusCode,
        /// S3 error code from the body.
        code: Option<String>,
        /// Response body, capped.
        body: String,
    },
}

fn describe(code: Option<&str>, body: &str) -> String {
    match code {
        Some(code) => format!(" {code}"),
        None if body.is_empty() => String::new(),
        None => format!(": {body}"),
    }
}

fn anonymous_hint(anonymous: bool) -> &'static str {
    if anonymous {
        " (anonymous: the object may be missing, or not public)"
    } else {
        ""
    }
}

/// A client bound to one S3 endpoint, signing with `credentials` when given.
#[derive(Clone, Debug)]
pub struct S3 {
    http: reqwest::Client,
    endpoint: Endpoint,
    credentials: Option<Credentials>,
}

impl S3 {
    /// A client with reqwest's defaults and an overall per-request timeout.
    pub fn new(
        endpoint: Endpoint,
        credentials: Option<Credentials>,
        timeout: Duration,
    ) -> Result<Self, S3Error> {
        let http = reqwest::Client::builder()
            .timeout(timeout)
            .build()
            .map_err(S3Error::Client)?;
        Ok(Self::with_client(http, endpoint, credentials))
    }

    /// A client over a caller-built reqwest client.
    pub fn with_client(
        http: reqwest::Client,
        endpoint: Endpoint,
        credentials: Option<Credentials>,
    ) -> Self {
        Self {
            http,
            endpoint,
            credentials,
        }
    }

    /// The endpoint this client talks to.
    pub fn endpoint(&self) -> &Endpoint {
        &self.endpoint
    }

    /// Whether requests are signed.
    pub fn has_credentials(&self) -> bool {
        self.credentials
            .is_some()
    }

    fn url(&self, bucket: &str, key: Option<&str>) -> String {
        match key {
            Some(key) => format!(
                "https://{}/{bucket}/{}",
                self.endpoint
                    .authority,
                utf8_percent_encode(key, KEY_ENCODE)
            ),
            None => format!(
                "https://{}/{bucket}",
                self.endpoint
                    .authority
            ),
        }
    }

    /// GET an object with its ETag.
    pub async fn get_object(&self, bucket: &str, key: &str) -> Result<Object, S3Error> {
        let url = self.url(bucket, Some(key));
        let resp = self
            .send(Method::GET, &url, Vec::new(), &[])
            .await?;
        let resp = self
            .check(Method::GET, &url, resp)
            .await?;
        let etag = etag(&resp);
        let body = resp
            .bytes()
            .await
            .map_err(|source| S3Error::Transport {
                method: Method::GET,
                url,
                source,
            })?;
        Ok(Object {
            body: body.to_vec(),
            etag,
        })
    }

    /// HEAD an object: `Ok(true)` if it exists, `Ok(false)` on 404.
    pub async fn head_object(&self, bucket: &str, key: &str) -> Result<bool, S3Error> {
        let url = self.url(bucket, Some(key));
        let resp = self
            .send(Method::HEAD, &url, Vec::new(), &[])
            .await?;
        match self
            .check(Method::HEAD, &url, resp)
            .await
        {
            Ok(_) => Ok(true),
            Err(S3Error::NotFound { .. }) => Ok(false),
            Err(e) => Err(e),
        }
    }

    /// PUT an object, world-readable, and return the new ETag if the store
    /// sent one. Requires credentials.
    pub async fn put_object(
        &self,
        bucket: &str,
        key: &str,
        body: Vec<u8>,
        content_type: &str,
        precondition: Option<&Precondition>,
    ) -> Result<Option<String>, S3Error> {
        let url = self.url(bucket, Some(key));
        let mut headers = vec![
            ("x-amz-acl", "public-read".to_string()),
            ("content-type", content_type.to_string()),
        ];
        match precondition {
            Some(Precondition::IfMatch(etag)) => headers.push((
                "if-match",
                etag.trim_matches('"')
                    .to_string(),
            )),
            Some(Precondition::IfNoneMatch) => headers.push(("if-none-match", "*".to_string())),
            None => {}
        }
        self.require_credentials(Method::PUT, &url)?;
        let resp = self
            .send(Method::PUT, &url, body, &headers)
            .await?;
        let resp = self
            .check(Method::PUT, &url, resp)
            .await?;
        Ok(etag(&resp))
    }

    /// DELETE an object. An object already absent (404) is success.
    /// Requires credentials.
    pub async fn delete_object(&self, bucket: &str, key: &str) -> Result<(), S3Error> {
        let url = self.url(bucket, Some(key));
        self.require_credentials(Method::DELETE, &url)?;
        let resp = self
            .send(Method::DELETE, &url, Vec::new(), &[])
            .await?;
        match self
            .check(Method::DELETE, &url, resp)
            .await
        {
            Ok(_) | Err(S3Error::NotFound { .. }) => Ok(()),
            Err(e) => Err(e),
        }
    }

    /// Create a private bucket. Succeeds when the bucket exists and the
    /// caller owns it, whether or not this call created it: radosgw answers
    /// 200 for a bucket its caller already owns, AWS a 409
    /// `BucketAlreadyOwnedByYou`. Requires credentials.
    pub async fn create_bucket(&self, bucket: &str) -> Result<(), S3Error> {
        let url = self.url(bucket, None);
        self.require_credentials(Method::PUT, &url)?;
        let body = create_bucket_body(
            &self
                .endpoint
                .region,
        );
        let resp = self
            .send(Method::PUT, &url, body, &[])
            .await?;
        match self
            .check(Method::PUT, &url, resp)
            .await
        {
            Ok(_) => Ok(()),
            Err(S3Error::Status { code, .. })
                if code.as_deref() == Some("BucketAlreadyOwnedByYou") =>
            {
                Ok(())
            }
            Err(S3Error::Status { code, .. }) if code.as_deref() == Some("BucketAlreadyExists") => {
                Err(S3Error::BucketOwnedByOther {
                    bucket: bucket.to_string(),
                })
            }
            Err(e) => Err(e),
        }
    }

    fn require_credentials(&self, method: Method, url: &str) -> Result<(), S3Error> {
        if self
            .credentials
            .is_none()
        {
            return Err(S3Error::CredentialsRequired {
                method,
                url: url.to_string(),
            });
        }
        Ok(())
    }

    /// Sends the request, signed when credentials are set. Logged on
    /// completion with the elapsed time, which tells a slow endpoint from a
    /// slow caller.
    async fn send(
        &self,
        method: Method,
        url: &str,
        body: Vec<u8>,
        headers: &[(&str, String)],
    ) -> Result<reqwest::Response, S3Error> {
        let transport = |source| S3Error::Transport {
            method: method.clone(),
            url: url.to_string(),
            source,
        };
        let mut req = self
            .http
            .request(method.clone(), url);
        for (name, value) in headers {
            req = req.header(*name, value);
        }
        let mut request = req
            .body(body)
            .build()
            .map_err(transport)?;
        if let Some(creds) = &self.credentials {
            // Signed as reqwest will send it, never set by hand: an explicit
            // Host header is a protocol error on HTTP/2, where nginx answers 400.
            let mut signed = vec![("host", host_of(request.url()))];
            signed.extend_from_slice(headers);
            let body = request
                .body()
                .and_then(|b| b.as_bytes())
                .unwrap_or_default();
            for (name, value) in self.signature(creds, &method, url, body, &signed)? {
                request
                    .headers_mut()
                    .insert(name, value);
            }
        }
        let started = Instant::now();
        let result = self
            .http
            .execute(request)
            .await;
        match &result {
            Ok(resp) => debug!(
                "{method} {url} -> {} in {:.3?}",
                resp.status(),
                started.elapsed()
            ),
            Err(e) => debug!("{method} {url} failed in {:.3?}: {e}", started.elapsed()),
        }
        result.map_err(transport)
    }

    /// The headers SigV4 adds for this request: `authorization`, `x-amz-date`,
    /// `x-amz-content-sha256` and, with a session token, `x-amz-security-token`.
    /// S3's rules apply: the path is signed as sent, never normalised or
    /// encoded a second time.
    fn signature(
        &self,
        creds: &Credentials,
        method: &Method,
        url: &str,
        body: &[u8],
        headers: &[(&str, String)],
    ) -> Result<Vec<(HeaderName, HeaderValue)>, S3Error> {
        let failed = |source: Box<dyn std::error::Error + Send + Sync>| S3Error::Signing {
            method: method.clone(),
            url: url.to_string(),
            source,
        };
        let identity: Identity = AwsCredentials::new(
            &creds.access_key,
            &creds.secret_key,
            creds
                .session_token
                .clone(),
            None,
            "s3-fleet",
        )
        .into();
        let mut settings = SigningSettings::default();
        settings.payload_checksum_kind = PayloadChecksumKind::XAmzSha256;
        settings.percent_encoding_mode = PercentEncodingMode::Single;
        settings.uri_path_normalization_mode = UriPathNormalizationMode::Disabled;
        let params = v4::SigningParams::builder()
            .identity(&identity)
            .region(
                &self
                    .endpoint
                    .region,
            )
            .name("s3")
            .time(SystemTime::now())
            .settings(settings)
            .build()
            .map_err(|e| failed(e.into()))?
            .into();
        let signable = SignableRequest::new(
            method.as_str(),
            url,
            headers
                .iter()
                .map(|(name, value)| (*name, value.as_str())),
            SignableBody::Bytes(body),
        )
        .map_err(|e| failed(e.into()))?;
        let (instructions, _signature) = sign(signable, &params)
            .map_err(|e| failed(e.into()))?
            .into_parts();
        instructions
            .headers()
            .map(|(name, value)| {
                Ok((
                    HeaderName::try_from(name).map_err(|e| failed(e.into()))?,
                    HeaderValue::try_from(value).map_err(|e| failed(e.into()))?,
                ))
            })
            .collect()
    }

    /// Maps a non-success response to its [`S3Error`], reading the body.
    async fn check(
        &self,
        method: Method,
        url: &str,
        resp: reqwest::Response,
    ) -> Result<reqwest::Response, S3Error> {
        let status = resp.status();
        if status.is_success() {
            return Ok(resp);
        }
        let body = match resp
            .text()
            .await
        {
            Ok(text) => text,
            Err(e) => format!("<body unreadable: {e}>"),
        };
        Err(classify(
            method,
            url,
            status,
            &body,
            self.credentials
                .is_none(),
        ))
    }
}

fn classify(method: Method, url: &str, status: StatusCode, body: &str, anonymous: bool) -> S3Error {
    let code = xml_element(body, "Code");
    let url = url.to_string();
    let body = cap(body.trim());
    match status {
        StatusCode::FORBIDDEN => S3Error::Forbidden {
            method,
            url,
            anonymous,
            code,
            body,
        },
        StatusCode::NOT_FOUND => S3Error::NotFound {
            method,
            url,
            code,
            body,
        },
        StatusCode::PRECONDITION_FAILED => S3Error::PreconditionFailed { method, url },
        StatusCode::CONFLICT if code.as_deref() == Some("ConditionalRequestConflict") => {
            S3Error::PreconditionFailed { method, url }
        }
        status => S3Error::Status {
            method,
            url,
            status,
            code,
            body,
        },
    }
}

fn cap(body: &str) -> String {
    match body
        .char_indices()
        .nth(ERROR_BODY_CAP)
    {
        Some((end, _)) => format!("{}...", &body[..end]),
        None => body.to_string(),
    }
}

/// The text of the first `<tag>` element. S3 error codes are plain ASCII
/// identifiers, so no entity decoding is attempted; the full body is kept
/// beside the code for anything else.
fn xml_element(body: &str, tag: &str) -> Option<String> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = body.find(&open)? + open.len();
    let len = body[start..].find(&close)?;
    Some(body[start..start + len].to_string())
}

/// The Host value reqwest derives from a URL: the default port omitted, an
/// IPv6 literal bracketed.
fn host_of(url: &reqwest::Url) -> String {
    let host = url
        .host_str()
        .unwrap_or_default();
    match url.port() {
        Some(port) => format!("{host}:{port}"),
        None => host.to_string(),
    }
}

fn etag(resp: &reqwest::Response) -> Option<String> {
    resp.headers()
        .get(ETAG)
        .and_then(|v| {
            v.to_str()
                .ok()
        })
        .map(str::to_string)
}

/// AWS rejects a LocationConstraint of us-east-1, its default region, and
/// requires one everywhere else; radosgw checks it against the zonegroup.
fn create_bucket_body(region: &str) -> Vec<u8> {
    if region == "us-east-1" {
        return Vec::new();
    }
    format!(
        "<CreateBucketConfiguration xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\">\
         <LocationConstraint>{}</LocationConstraint></CreateBucketConfiguration>",
        xml_escape(region)
    )
    .into_bytes()
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn client(creds: bool) -> S3 {
        S3::new(
            Endpoint {
                authority: "[2001:db8::1]:7480".to_string(),
                region: "test".to_string(),
            },
            creds.then(|| Credentials {
                access_key: "AK".to_string(),
                secret_key: "SECRET".to_string(),
                session_token: None,
            }),
            Duration::from_secs(1),
        )
        .unwrap()
    }

    #[test]
    fn url_keeps_ipv6_authority_and_encodes_key() {
        let s3 = client(false);
        assert_eq!(
            s3.url("fleet", Some("dir/a b+c.yaml")),
            "https://[2001:db8::1]:7480/fleet/dir/a%20b%2Bc.yaml"
        );
        assert_eq!(s3.url("fleet", None), "https://[2001:db8::1]:7480/fleet");
    }

    #[test]
    fn credentials_debug_redacts_secrets() {
        let creds = Credentials {
            access_key: "AK".to_string(),
            secret_key: "SECRET".to_string(),
            session_token: Some("TOKEN".to_string()),
        };
        let shown = format!("{creds:?}");
        assert!(shown.contains("AK"), "{shown}");
        assert!(!shown.contains("SECRET"), "{shown}");
        assert!(!shown.contains("TOKEN"), "{shown}");
    }

    #[test]
    fn signing_covers_preconditions_and_acl() {
        let s3 = client(true);
        let url = s3.url("fleet", Some("fleet.yaml"));
        let added = s3
            .signature(
                s3.credentials
                    .as_ref()
                    .unwrap(),
                &Method::PUT,
                &url,
                b"x",
                &[
                    ("host", "[2001:db8::1]:7480".to_string()),
                    ("x-amz-acl", "public-read".to_string()),
                    ("if-match", "\"abc\"".to_string()),
                ],
            )
            .unwrap();
        let header = |name: &str| {
            added
                .iter()
                .find(|(n, _)| n.as_str() == name)
                .map(|(_, v)| {
                    v.to_str()
                        .unwrap()
                })
        };
        let auth = header("authorization").expect("authorization added");
        assert!(
            auth.starts_with("AWS4-HMAC-SHA256 Credential=AK/"),
            "{auth}"
        );
        assert!(auth.contains("/test/s3/aws4_request"), "{auth}");
        for signed in ["host", "if-match", "x-amz-acl", "x-amz-content-sha256"] {
            assert!(auth.contains(signed), "{signed} not signed: {auth}");
        }
        assert!(header("x-amz-date").is_some(), "{added:?}");
        assert!(header("x-amz-content-sha256").is_some(), "{added:?}");
    }

    #[test]
    fn status_classification() {
        let url = "https://e/b/k";
        let denied = "<Error><Code>AccessDenied</Code></Error>";
        assert!(matches!(
            classify(Method::GET, url, StatusCode::FORBIDDEN, denied, true),
            S3Error::Forbidden { anonymous: true, code: Some(c), .. } if c == "AccessDenied"
        ));
        assert!(matches!(
            classify(Method::GET, url, StatusCode::NOT_FOUND, "<Code>NoSuchKey</Code>", false),
            S3Error::NotFound { code: Some(c), .. } if c == "NoSuchKey"
        ));
        assert!(matches!(
            classify(Method::PUT, url, StatusCode::PRECONDITION_FAILED, "", false),
            S3Error::PreconditionFailed { .. }
        ));
        assert!(matches!(
            classify(
                Method::PUT,
                url,
                StatusCode::CONFLICT,
                "<Code>ConditionalRequestConflict</Code>",
                false
            ),
            S3Error::PreconditionFailed { .. }
        ));
        assert!(matches!(
            classify(
                Method::PUT,
                url,
                StatusCode::CONFLICT,
                "<Code>BucketAlreadyExists</Code>",
                false
            ),
            S3Error::Status {
                status: StatusCode::CONFLICT,
                ..
            }
        ));
    }

    #[test]
    fn anonymous_forbidden_says_it_may_be_missing() {
        let e = classify(Method::GET, "u", StatusCode::FORBIDDEN, "", true);
        assert!(
            e.to_string()
                .contains("may be missing"),
            "{e}"
        );
        let e = classify(Method::GET, "u", StatusCode::FORBIDDEN, "", false);
        assert!(
            !e.to_string()
                .contains("may be missing"),
            "{e}"
        );
    }

    #[test]
    fn error_body_is_capped_on_a_char_boundary() {
        let long = "é".repeat(ERROR_BODY_CAP + 10);
        let capped = cap(&long);
        assert_eq!(
            capped
                .chars()
                .count(),
            ERROR_BODY_CAP + 3
        );
    }

    #[test]
    fn bucket_body_omits_constraint_only_for_us_east_1() {
        assert!(create_bucket_body("us-east-1").is_empty());
        let body = String::from_utf8(create_bucket_body("a<b")).unwrap();
        assert!(
            body.contains("<LocationConstraint>a&lt;b</LocationConstraint>"),
            "{body}"
        );
    }
}
