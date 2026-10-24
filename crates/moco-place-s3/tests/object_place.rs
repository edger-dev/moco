//! The object-store place keeps the Place contract — over memory, a local
//! directory, and in parts — and behaves as a remote must: safe inside
//! another runtime, quick to say it is offline.
//!
//! implements: a7089331ac0dff49deb5acc9afce92b2a9d8a9d5022f66af915f3a2332d6f447@a7089331ac0dff49deb5acc9afce92b2a9d8a9d5022f66af915f3a2332d6f447

#![allow(clippy::unwrap_used, clippy::expect_used, reason = "tests")]

use std::time::{Duration, Instant};

use moco_place::contract::key_for;
use moco_place::{Hash, Key, Place, PlaceError, Put, put_bytes, read_all};
use moco_place_s3::ObjectPlace;

moco_place::place_contract!(memory, ((), ObjectPlace::from_url("memory://").unwrap()));

moco_place::place_contract!(directory, {
    let dir = tempfile::tempdir().unwrap();
    let url = format!("file://{}/objects", dir.path().display());
    std::fs::create_dir_all(dir.path().join("objects")).unwrap();
    (dir, ObjectPlace::from_url(&url).unwrap())
});

/// Every put larger than 1 KiB goes up in parts of 4 KiB.
moco_place::place_contract!(
    in_parts,
    (
        (),
        ObjectPlace::from_url("memory://")
            .unwrap()
            .with_multipart(1024, 4096)
    )
);

/// A prefix keeps a place inside its part of a bucket.
#[test]
fn a_prefix_scopes_the_place() {
    let store: std::sync::Arc<dyn object_store::ObjectStore> =
        std::sync::Arc::new(object_store::memory::InMemory::new());
    let a = ObjectPlace::new(store.clone(), "alpha").unwrap();
    let b = ObjectPlace::new(store, "beta").unwrap();
    let key = key_for(b"scoped", "");
    put_bytes(&a, &key, b"scoped").unwrap();
    assert!(a.exists(&key).unwrap());
    assert!(!b.exists(&key).unwrap());
    assert_eq!(b.list(None).unwrap(), vec![]);
}

/// A large put whose bytes do not match is aborted: nothing lands.
#[test]
fn a_mismatched_upload_in_parts_leaves_nothing() {
    let place = ObjectPlace::from_url("memory://")
        .unwrap()
        .with_multipart(1024, 4096);
    let good: Vec<u8> = (0..20_000u32).map(|i| (i % 7) as u8).collect();
    let mut bad = good.clone();
    bad[19_999] ^= 1;
    let key = key_for(&good, "");
    let err = place
        .put_once(
            &key,
            &Hash::of_content(&good),
            good.len() as u64,
            &mut &bad[..],
        )
        .unwrap_err();
    assert!(matches!(err, PlaceError::ContentMismatch { .. }), "{err}");
    assert!(!place.exists(&key).unwrap());
}

/// Called from inside another runtime — as kudo's tokio services will — and
/// dropped there, the place neither deadlocks nor panics.
#[tokio::test(flavor = "multi_thread")]
async fn it_works_inside_another_runtime() {
    let place = ObjectPlace::from_url("memory://").unwrap();
    let key = key_for(b"async", "");
    assert_eq!(put_bytes(&place, &key, b"async").unwrap(), Put::Stored);
    assert_eq!(read_all(&place, &key).unwrap(), b"async");
    drop(place);
}

#[tokio::test(flavor = "current_thread")]
async fn it_works_inside_a_single_threaded_runtime() {
    let place = ObjectPlace::from_url("memory://").unwrap();
    let key = key_for(b"one thread", "");
    put_bytes(&place, &key, b"one thread").unwrap();
    assert!(place.exists(&key).unwrap());
    drop(place);
}

/// An S3 endpoint nobody answers is offline, said within seconds.
#[test]
fn an_unreachable_endpoint_is_offline_within_seconds() {
    let place = ObjectPlace::from_url_with(
        "s3://bucket/prefix",
        [
            ("aws_endpoint", "http://127.0.0.1:9"),
            ("aws_access_key_id", "id"),
            ("aws_secret_access_key", "secret"),
            ("aws_region", "auto"),
        ],
    )
    .unwrap();
    let start = Instant::now();
    let err = place.exists(&key_for(b"x", "")).unwrap_err();
    assert!(matches!(err, PlaceError::Offline), "{err}");
    assert!(
        start.elapsed() < Duration::from_secs(30),
        "{:?}",
        start.elapsed()
    );
}

#[test]
fn a_url_names_a_known_store() {
    assert!(ObjectPlace::from_url("ftp://somewhere").is_err());
    assert!(ObjectPlace::from_url("s3:///no-bucket").is_err());
    assert!(ObjectPlace::from_url("not a url").is_err());
}

/// Opt-in: a round trip against a real S3 endpoint, named by
/// `MOCO_PLACE_S3_TEST=s3://bucket/prefix` with keys in the `AWS_*`
/// environment.
#[test]
fn a_real_endpoint_round_trip() {
    let Ok(url) = std::env::var("MOCO_PLACE_S3_TEST") else {
        return;
    };
    let place = ObjectPlace::from_url(&url).unwrap();
    let stamp = format!("{:?}", std::time::SystemTime::now());
    let key = key_for(stamp.as_bytes(), ".txt");
    assert_eq!(
        put_bytes(&place, &key, stamp.as_bytes()).unwrap(),
        Put::Stored
    );
    assert_eq!(
        put_bytes(&place, &key, stamp.as_bytes()).unwrap(),
        Put::Present
    );
    assert_eq!(read_all(&place, &key).unwrap(), stamp.as_bytes());
    let listed = place.list(Some(&key.parent().unwrap())).unwrap();
    assert!(listed.iter().any(|c| c.key() == &key));
    let _ = Key::new("store").unwrap();
}
