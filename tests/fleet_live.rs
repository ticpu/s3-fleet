//! The Fleet handle against a real S3: init, load, commit, and a lost race.
//! The fleet is the test bucket itself; see tests/common for the environment.

mod common;

use s3_fleet::{Error, Fleet, Init};

#[tokio::test]
async fn concurrent_commits_conflict() {
    let Some(live) = common::live("concurrent_commits_conflict") else {
        return;
    };
    let fleet = Fleet::new(&live.bucket, live.signed()).unwrap();
    let init = fleet
        .init()
        .await
        .expect("init");
    assert!(matches!(init, Init::Created | Init::AlreadyExists));
    assert_eq!(
        fleet
            .init()
            .await
            .unwrap(),
        Init::AlreadyExists,
        "a second init leaves the document alone"
    );

    let key = common::unique_prefix("attr").replace('/', ".");
    let a = fleet
        .load()
        .await
        .unwrap();
    let b = fleet
        .load()
        .await
        .unwrap();

    let mut doc_a = a
        .doc
        .clone();
    doc_a
        .set_attribute(&key, "a")
        .unwrap();
    fleet
        .commit(a, &doc_a)
        .await
        .expect("first writer wins");

    let mut doc_b = b
        .doc
        .clone();
    doc_b
        .set_attribute(&key, "b")
        .unwrap();
    let lost = fleet
        .commit(b, &doc_b)
        .await;
    assert!(matches!(lost, Err(Error::Conflict { .. })), "{lost:?}");

    let anon = Fleet::new(&live.bucket, live.anonymous()).unwrap();
    let now = anon
        .load()
        .await
        .expect("anonymous load");
    assert_eq!(
        now.doc
            .attributes()[&key],
        "a",
        "the losing write must not land"
    );

    let mut cleanup = now
        .doc
        .clone();
    cleanup.unset_attribute(&key);
    fleet
        .commit(now, &cleanup)
        .await
        .expect("cleanup");
}

#[tokio::test]
async fn anonymous_commit_needs_credentials() {
    let Some(live) = common::live("anonymous_commit_needs_credentials") else {
        return;
    };
    // Tests run concurrently: the document may not exist yet.
    Fleet::new(&live.bucket, live.signed())
        .unwrap()
        .init()
        .await
        .unwrap();
    let anon = Fleet::new(&live.bucket, live.anonymous()).unwrap();
    let snap = anon
        .load()
        .await
        .unwrap();
    let doc = snap
        .doc
        .clone();
    let err = anon
        .commit(snap, &doc)
        .await;
    assert!(
        matches!(err, Err(Error::CredentialsRequired { .. })),
        "{err:?}"
    );
}

#[tokio::test]
async fn missing_fleet_is_forbidden_anonymously_and_not_found_signed() {
    let Some(live) = common::live("missing_fleet") else {
        return;
    };
    let name = "s3-fleet-test-never-created";
    let anon = Fleet::new(name, live.anonymous()).unwrap();
    let err = anon
        .load()
        .await;
    assert!(
        matches!(err, Err(Error::Forbidden { .. } | Error::NotFound { .. })),
        "{err:?}"
    );
    let signed = Fleet::new(name, live.signed()).unwrap();
    let err = signed
        .load()
        .await;
    assert!(matches!(err, Err(Error::NotFound { .. })), "{err:?}");
}
