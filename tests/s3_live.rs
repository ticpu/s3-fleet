//! The S3 client against a real service: preconditions, anonymous reads and
//! the 403/404 distinction. See tests/common for the environment.

mod common;

use s3_fleet::s3::{Precondition, S3Error};

#[tokio::test]
async fn conditional_puts_and_anonymous_reads() {
    let Some(live) = common::live("conditional_puts_and_anonymous_reads") else {
        return;
    };
    let s3 = live.signed();
    let anon = live.anonymous();
    let bucket = &live.bucket;
    let key = format!("{}/doc.yaml", common::unique_prefix("cond"));

    s3.create_bucket(bucket)
        .await
        .expect("create bucket");
    s3.create_bucket(bucket)
        .await
        .expect("create bucket again is not an error");

    let first = s3
        .put_object(
            bucket,
            &key,
            b"one".to_vec(),
            "application/yaml",
            Some(&Precondition::IfNoneMatch),
        )
        .await
        .expect("create with If-None-Match")
        .expect("PUT returns an ETag");

    let again = s3
        .put_object(
            bucket,
            &key,
            b"dup".to_vec(),
            "application/yaml",
            Some(&Precondition::IfNoneMatch),
        )
        .await;
    assert!(
        matches!(again, Err(S3Error::PreconditionFailed { .. })),
        "If-None-Match on an existing object: {again:?}"
    );

    let read = anon
        .get_object(bucket, &key)
        .await
        .expect("anonymous GET after a public-read PUT");
    assert_eq!(read.body, b"one");
    assert_eq!(
        read.etag
            .as_deref(),
        Some(first.as_str())
    );

    let second = s3
        .put_object(
            bucket,
            &key,
            b"two".to_vec(),
            "application/yaml",
            Some(&Precondition::IfMatch(first.clone())),
        )
        .await
        .expect("update with the current ETag")
        .expect("PUT returns an ETag");
    assert_ne!(first, second);

    let stale = s3
        .put_object(
            bucket,
            &key,
            b"lost".to_vec(),
            "application/yaml",
            Some(&Precondition::IfMatch(first.clone())),
        )
        .await;
    assert!(
        matches!(stale, Err(S3Error::PreconditionFailed { .. })),
        "If-Match with a stale ETag must be refused: {stale:?}"
    );
    let read = anon
        .get_object(bucket, &key)
        .await
        .unwrap();
    assert_eq!(read.body, b"two", "the stale write must not have landed");

    s3.delete_object(bucket, &key)
        .await
        .expect("delete");
    s3.delete_object(bucket, &key)
        .await
        .expect("delete of an absent object is success");
}

#[tokio::test]
async fn missing_object_anonymous_is_forbidden_signed_is_not_found() {
    let Some(live) = common::live("missing_object_anonymous_is_forbidden_signed_is_not_found")
    else {
        return;
    };
    let key = format!("{}/absent.yaml", common::unique_prefix("absent"));
    live.signed()
        .create_bucket(&live.bucket)
        .await
        .unwrap();

    let anon = live
        .anonymous()
        .get_object(&live.bucket, &key)
        .await;
    assert!(
        matches!(
            anon,
            Err(S3Error::Forbidden {
                anonymous: true,
                ..
            })
        ),
        "anonymous GET of a missing key: {anon:?}"
    );
    let signed = live
        .signed()
        .get_object(&live.bucket, &key)
        .await;
    assert!(
        matches!(signed, Err(S3Error::NotFound { .. })),
        "signed GET of a missing key: {signed:?}"
    );
    assert!(
        !live
            .signed()
            .head_object(&live.bucket, &key)
            .await
            .unwrap()
    );
}

#[tokio::test]
async fn anonymous_write_needs_credentials() {
    let Some(live) = common::live("anonymous_write_needs_credentials") else {
        return;
    };
    let err = live
        .anonymous()
        .put_object(&live.bucket, "never", Vec::new(), "text/plain", None)
        .await;
    assert!(
        matches!(err, Err(S3Error::CredentialsRequired { .. })),
        "{err:?}"
    );
}
