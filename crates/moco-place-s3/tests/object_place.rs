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

// Every put larger than 1 KiB goes up in parts of 4 KiB.
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

moco_place::place_contract!(directory_in_parts, {
    let dir = tempfile::tempdir().unwrap();
    let url = format!("file://{}/objects", dir.path().display());
    (
        dir,
        ObjectPlace::from_url(&url)
            .unwrap()
            .with_multipart(1024, 4096),
    )
});

/// A body outliving its place ends in an error, never a clean short read.
#[test]
fn a_body_outliving_its_place_does_not_end_short() {
    let place = ObjectPlace::from_url("memory://")
        .unwrap()
        .with_multipart(1024, 4096);
    let bytes: Vec<u8> = (0..2_000_000u32).map(|i| (i % 249) as u8).collect();
    let key = key_for(&bytes, "");
    put_bytes(&place, &key, &bytes).unwrap();
    let mut body = place.get(&key, None).unwrap();
    let len = body.len();
    drop(place);
    let mut out = Vec::new();
    match std::io::Read::read_to_end(&mut body, &mut out) {
        Ok(_) => assert_eq!(out.len() as u64, len, "a clean end is a whole body"),
        Err(e) => assert_eq!(e.kind(), std::io::ErrorKind::UnexpectedEof, "{e}"),
    }
}

/// A range of an object that went up in parts reads back exactly.
#[test]
fn a_range_of_an_object_in_parts() {
    let place = ObjectPlace::from_url("memory://")
        .unwrap()
        .with_multipart(1024, 4096);
    let bytes: Vec<u8> = (0..50_000u32).map(|i| (i % 241) as u8).collect();
    let key = key_for(&bytes, "");
    put_bytes(&place, &key, &bytes).unwrap();
    let mut out = Vec::new();
    let mut body = place
        .get(
            &key,
            Some(moco_place::ByteRange {
                start: 10_000,
                end: Some(30_000),
            }),
        )
        .unwrap();
    assert_eq!(body.len(), 20_000);
    std::io::Read::read_to_end(&mut body, &mut out).unwrap();
    assert_eq!(out, bytes[10_000..30_000]);
}

/// A prefix of several segments scopes listing and reads.
#[test]
fn a_nested_prefix_scopes_the_place() {
    let store: std::sync::Arc<dyn object_store::ObjectStore> =
        std::sync::Arc::new(object_store::memory::InMemory::new());
    let place = ObjectPlace::new(store.clone(), "team/alice").unwrap();
    let key = key_for(b"nested", ".md");
    put_bytes(&place, &key, b"nested").unwrap();
    assert_eq!(
        place.list(None).unwrap(),
        vec![moco_place::Child::Prefix(Key::new("store").unwrap())]
    );
    let shard = key.parent().unwrap();
    assert_eq!(place.list(Some(&shard)).unwrap().len(), 1);
    let other = ObjectPlace::new(store, "team").unwrap();
    assert_eq!(
        other.list(None).unwrap(),
        vec![moco_place::Child::Prefix(Key::new("alice").unwrap())]
    );
}

/// A local directory place that does not exist yet is made.
#[test]
fn a_missing_directory_is_made() {
    let dir = tempfile::tempdir().unwrap();
    let url = format!("file://{}/new/objects", dir.path().display());
    let place = ObjectPlace::from_url(&url).unwrap();
    put_bytes(&place, &key_for(b"made", ""), b"made").unwrap();
}

/// An S3 place with no credentials is refused at once, not left to time
/// out against a metadata service.
#[test]
fn an_s3_place_without_credentials_is_refused() {
    let err = ObjectPlace::from_url_with(
        "s3://bucket/prefix",
        [("aws_endpoint", "http://127.0.0.1:9")],
    )
    .err()
    .unwrap();
    assert!(matches!(err, PlaceError::Denied(_)), "{err}");
}

/// Streaming a large body from blocking code inside a single-threaded
/// runtime completes.
#[tokio::test(flavor = "current_thread")]
async fn a_large_body_streams_from_spawn_blocking() {
    let total = tokio::task::spawn_blocking(|| {
        let place = ObjectPlace::from_url("memory://").unwrap();
        let bytes: Vec<u8> = (0..5_000_000u32).map(|i| (i % 233) as u8).collect();
        let key = key_for(&bytes, "");
        put_bytes(&place, &key, &bytes).unwrap();
        read_all(&place, &key).unwrap().len()
    })
    .await
    .unwrap();
    assert_eq!(total, 5_000_000);
}
