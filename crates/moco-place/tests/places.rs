//! The `Place` contract, held by every place: the same suite runs against the
//! file-system place and the in-memory place.
//!
//! implements: a7089331ac0dff49deb5acc9afce92b2a9d8a9d5022f66af915f3a2332d6f447@a7089331ac0dff49deb5acc9afce92b2a9d8a9d5022f66af915f3a2332d6f447

#![allow(clippy::unwrap_used, clippy::expect_used, reason = "tests")]

use std::io::Read;

use moco_place::{
    ByteRange, Capabilities, Child, FsPlace, Hash, Key, MemPlace, Place, PlaceError, Put, Verified,
    put_bytes, read_all,
};

/// The key a sharded store files `bytes` under.
fn key_for(bytes: &[u8], ext: &str) -> Key {
    let h = Hash::of_content(bytes);
    Key::new(format!("store/{}/{}{ext}", h.shard(), h.as_hex())).unwrap()
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

macro_rules! place_contract {
    ($module:ident, $make:expr) => {
        mod $module {
            use super::*;

            #[test]
            fn a_put_reads_back() {
                let (_guard, place) = $make;
                let key = key_for(b"hello", ".md");
                assert_eq!(put_bytes(&place, &key, b"hello").unwrap(), Put::Stored);
                assert!(place.exists(&key).unwrap());
                assert_eq!(read_all(&place, &key).unwrap(), b"hello");
            }

            #[test]
            fn a_second_put_finds_it_present_and_writes_nothing() {
                let (_guard, place) = $make;
                let key = key_for(b"once", "");
                put_bytes(&place, &key, b"once").unwrap();
                assert_eq!(put_bytes(&place, &key, b"once").unwrap(), Put::Present);
                assert_eq!(read_all(&place, &key).unwrap(), b"once");
            }

            #[test]
            fn a_put_under_a_key_naming_another_hash_is_refused() {
                let (_guard, place) = $make;
                let key = key_for(b"other", "");
                let err = put_bytes(&place, &key, b"mine").unwrap_err();
                assert!(matches!(err, PlaceError::KeyNamesOtherHash { .. }), "{err}");
                assert!(!place.exists(&key).unwrap());
                let unnamed = Key::new("notes/plain.md").unwrap();
                let err = put_bytes(&place, &unnamed, b"mine").unwrap_err();
                assert!(matches!(err, PlaceError::KeyNamesOtherHash { .. }), "{err}");
            }

            #[test]
            fn bytes_that_do_not_match_the_declared_hash_store_nothing() {
                let (_guard, place) = $make;
                let key = key_for(b"declared", "");
                let declared = Hash::of_content(b"declared");
                let err = place
                    .put_once(&key, &declared, 8, &mut &b"imposter"[..])
                    .unwrap_err();
                assert!(matches!(err, PlaceError::ContentMismatch { .. }), "{err}");
                assert!(!place.exists(&key).unwrap());
                assert_eq!(
                    place.list(Some(&key.parent().unwrap())).unwrap(),
                    vec![],
                    "no temporary is left to see"
                );
            }

            #[test]
            fn bytes_shorter_than_declared_store_nothing() {
                let (_guard, place) = $make;
                let key = key_for(b"twelve bytes", "");
                let declared = Hash::of_content(b"twelve bytes");
                let err = place
                    .put_once(&key, &declared, 99, &mut &b"twelve bytes"[..])
                    .unwrap_err();
                assert!(matches!(err, PlaceError::SizeMismatch { .. }), "{err}");
                assert!(!place.exists(&key).unwrap());
            }

            #[test]
            fn a_range_reads_part_of_an_object() {
                let (_guard, place) = $make;
                let key = key_for(b"0123456789", "");
                put_bytes(&place, &key, b"0123456789").unwrap();
                let read = |range| {
                    let mut out = Vec::new();
                    place
                        .get(&key, Some(range))
                        .unwrap()
                        .read_to_end(&mut out)
                        .unwrap();
                    out
                };
                assert_eq!(
                    read(ByteRange {
                        start: 2,
                        end: Some(5)
                    }),
                    b"234"
                );
                assert_eq!(
                    read(ByteRange {
                        start: 7,
                        end: None
                    }),
                    b"789"
                );
                assert_eq!(
                    read(ByteRange {
                        start: 8,
                        end: Some(99)
                    }),
                    b"89"
                );
            }

            #[test]
            fn a_missing_object_is_not_found() {
                let (_guard, place) = $make;
                let key = key_for(b"absent", "");
                assert!(!place.exists(&key).unwrap());
                assert!(matches!(
                    place.get(&key, None),
                    Err(PlaceError::NotFound(_))
                ));
            }

            #[test]
            fn a_list_shows_one_level_sorted() {
                let (_guard, place) = $make;
                let a = key_for(b"a", ".md");
                let b = key_for(b"b", "");
                put_bytes(&place, &a, b"a").unwrap();
                put_bytes(&place, &b, b"b").unwrap();
                let staged = Key::new(format!("staged/{}", b.name())).unwrap();
                put_bytes(&place, &staged, b"b").unwrap();

                assert_eq!(
                    place.list(None).unwrap(),
                    vec![
                        Child::Prefix(Key::new("staged").unwrap()),
                        Child::Prefix(Key::new("store").unwrap()),
                    ]
                );
                let shard = a.parent().unwrap();
                let listed = place.list(Some(&shard)).unwrap();
                assert!(listed.contains(&Child::Object {
                    key: a.clone(),
                    size: 1
                }));
                let mut sorted = listed.clone();
                sorted.sort_by_key(|c| match c {
                    Child::Object { key, .. } | Child::Prefix(key) => key.clone(),
                });
                assert_eq!(listed, sorted);
                assert_eq!(
                    place.list(Some(&Key::new("nothing").unwrap())).unwrap(),
                    vec![]
                );
            }

            #[test]
            fn verify_checks_an_object_against_its_key() {
                let (_guard, place) = $make;
                let key = key_for(b"kept", "");
                put_bytes(&place, &key, b"kept").unwrap();
                assert_eq!(place.verify(&key).unwrap(), Verified::Intact);
                assert_eq!(
                    place.verify(&key_for(b"never put", "")).unwrap(),
                    Verified::Missing
                );
            }

            #[test]
            fn an_inventory_lists_every_object_with_its_hash_and_size() {
                let (_guard, place) = $make;
                let a = key_for(b"alpha", ".md");
                let staged =
                    Key::new(format!("staged/x/1700-{}", Hash::of_content(b"beta"))).unwrap();
                put_bytes(&place, &a, b"alpha").unwrap();
                put_bytes(&place, &staged, b"beta").unwrap();
                let before = now();
                let mut inv = place.inventory().unwrap();
                inv.sort_by(|x, y| x.key.cmp(&y.key));
                assert_eq!(inv.len(), 2);
                assert_eq!(inv[0].key, staged);
                assert_eq!(inv[0].hash, Hash::of_content(b"beta"));
                assert_eq!(inv[0].size, 4);
                assert_eq!(inv[1].key, a);
                assert!(
                    inv.iter()
                        .all(|e| e.verified_at >= before && e.verified_at <= now())
                );
            }

            /// A large object moves as a stream, intact.
            #[test]
            fn a_large_object_streams_through() {
                let (_guard, place) = $make;
                let bytes: Vec<u8> = (0..5_000_000u32).map(|i| (i % 253) as u8).collect();
                let key = key_for(&bytes, ".bin");
                let hash = Hash::of_content(&bytes);
                assert_eq!(
                    place
                        .put_once(&key, &hash, bytes.len() as u64, &mut &bytes[..])
                        .unwrap(),
                    Put::Stored
                );
                let (got, len) = Hash::of_reader(&mut place.get(&key, None).unwrap()).unwrap();
                assert_eq!((got, len), (hash, bytes.len() as u64));
            }
        }
    };
}

place_contract!(fs, {
    let dir = tempfile::tempdir().unwrap();
    let place = FsPlace::new(dir.path());
    (dir, place)
});
place_contract!(mem, ((), MemPlace::new()));

/// On disk: a corrupted file reads as corrupt, temporaries and other
/// dot-names never show, and a stray file whose name names no hash is left
/// out of the inventory.
#[test]
fn a_file_system_place_sees_corruption_and_hides_its_own_files() {
    let dir = tempfile::tempdir().unwrap();
    let place = FsPlace::new(dir.path());
    let key = key_for(b"original", ".md");
    put_bytes(&place, &key, b"original").unwrap();
    std::fs::write(dir.path().join(key.as_str()), b"rotted!!").unwrap();
    assert_eq!(
        place.verify(&key).unwrap(),
        Verified::Corrupt {
            got: Hash::of_content(b"rotted!!")
        }
    );

    let shard = dir.path().join(key.parent().unwrap().as_str());
    std::fs::write(shard.join(".tmp-123"), b"half").unwrap();
    std::fs::write(shard.join("README"), b"stray").unwrap();
    let listed = place.list(Some(&key.parent().unwrap())).unwrap();
    assert!(
        listed
            .iter()
            .all(|c| !matches!(c, Child::Object { key, .. } if key.name().starts_with('.')))
    );
    let inv = place.inventory().unwrap();
    assert_eq!(inv.len(), 1, "{inv:?}");
    assert_eq!(
        inv[0].hash,
        Hash::of_content(b"rotted!!"),
        "the inventory states what is there now"
    );
}

/// A read-only place refuses a put; an offline one says so.
#[test]
fn a_place_that_takes_no_writes_refuses_a_put() {
    let ro = MemPlace::with_capabilities(Capabilities {
        online: true,
        writable: false,
        cold: false,
    });
    let key = key_for(b"x", "");
    assert!(matches!(
        put_bytes(&ro, &key, b"x"),
        Err(PlaceError::NotWritable)
    ));
    let off = MemPlace::with_capabilities(Capabilities {
        online: false,
        writable: true,
        cold: false,
    });
    assert!(matches!(
        put_bytes(&off, &key, b"x"),
        Err(PlaceError::Offline)
    ));
    assert!(matches!(off.get(&key, None), Err(PlaceError::Offline)));
}
