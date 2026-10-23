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
//! Without `Credentials` a request goes out unsigned. Every PUT carries
//! `x-amz-acl: public-read`, which is what lets those anonymous GETs work.
//!
//! radosgw (like AWS) answers an anonymous GET of a missing key with 403, not
//! 404, unless the caller may list the bucket. A 403 is therefore reported as
//! `S3Error::Forbidden` with `anonymous` set, never as "absent": the caller
//! cannot tell the two apart and must not be told it can.
//!
//! # Preconditions
//!
//! `Precondition::IfMatch` sends the ETag with its surrounding quotes
//! stripped, and `Precondition::IfNoneMatch` sends `*`. A 412 becomes
//! `S3Error::PreconditionFailed`. The quotes are stripped because radosgw
//! answers 412 to a quoted ETag that matches, so every conditional update would
//! look like a conflict; it honours the bare value. RFC 9110 wants the quoted
//! form, so re-check this against any other S3 implementation: the live tests
//! in tests/s3_live.rs fail on either mistake, a refused match or an accepted
//! stale write.
//!
//! The endpoint is an authority (`host[:port]`, a bracketed IPv6 literal
//! allowed) used verbatim in the URL and the signed Host header, never parsed.
//! Requests are path-style: `https://<endpoint>/<bucket>/<key>`.
