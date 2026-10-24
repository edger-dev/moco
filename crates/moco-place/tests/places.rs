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

            #[test]
            fn an_empty_object_is_an_object() {
                let (_guard, place) = $make;
                let key = key_for(b"", "");
                assert_eq!(put_bytes(&place, &key, b"").unwrap(), Put::Stored);
                assert_eq!(read_all(&place, &key).unwrap(), b"");
                assert_eq!(place.verify(&key).unwrap(), Verified::Intact);
            }

            /// Ranges clamp to the object: past its end, or backwards, is empty.
            #[test]
            fn a_range_past_the_end_or_backwards_is_empty() {
                let (_guard, place) = $make;
                let key = key_for(b"abc", "");
                put_bytes(&place, &key, b"abc").unwrap();
                for range in [
                    ByteRange {
                        start: 9,
                        end: None,
                    },
                    ByteRange {
                        start: 2,
                        end: Some(1),
                    },
                ] {
                    let mut out = Vec::new();
                    place
                        .get(&key, Some(range))
                        .unwrap()
                        .read_to_end(&mut out)
                        .unwrap();
                    assert_eq!(out, b"", "{range:?}");
                }
            }

            /// Listing below an object finds nothing, the same on every place.
            #[test]
            fn a_list_below_an_object_is_empty() {
                let (_guard, place) = $make;
                let key = key_for(b"leaf", "");
                put_bytes(&place, &key, b"leaf").unwrap();
                assert_eq!(place.list(Some(&key)).unwrap(), vec![]);
                let below = key.join("x").unwrap();
                assert!(!place.exists(&below).unwrap());
                // Below a file, a named key is missing, not an error.
                let named_below = key.join(key_for(b"y", "").name()).unwrap();
                assert_eq!(place.verify(&named_below).unwrap(), Verified::Missing);
            }

            /// Writers racing on one key: exactly one stores it.
            #[test]
            fn racing_puts_store_once() {
                let (_guard, place) = $make;
                let place = std::sync::Arc::new(place);
                let key = key_for(b"contended", "");
                let outcomes: Vec<Put> = (0..8)
                    .map(|_| {
                        let place = place.clone();
                        let key = key.clone();
                        std::thread::spawn(move || put_bytes(&*place, &key, b"contended").unwrap())
                    })
                    .collect::<Vec<_>>()
                    .into_iter()
                    .map(|t| t.join().unwrap())
                    .collect();
                assert_eq!(
                    outcomes.iter().filter(|o| **o == Put::Stored).count(),
                    1,
                    "{outcomes:?}"
                );
                assert_eq!(read_all(&*place, &key).unwrap(), b"contended");
            }

            /// A corrupt object's inventory entry says so.
            #[test]
            fn an_inventory_entry_knows_whether_it_is_intact() {
                let (_guard, place) = $make;
                let key = key_for(b"fine", "");
                put_bytes(&place, &key, b"fine").unwrap();
                let inv = place.inventory().unwrap();
                assert!(inv[0].is_intact());
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
    let place = FsPlace::create(dir.path()).unwrap();
    (dir, place)
});
place_contract!(mem, ((), MemPlace::new()));

/// On disk: a corrupted file reads as corrupt, temporaries and other
/// dot-names never show, and a stray file whose name names no hash is left
/// out of the inventory.
#[test]
fn a_file_system_place_sees_corruption_and_hides_its_own_files() {
    let dir = tempfile::tempdir().unwrap();
    let place = FsPlace::create(dir.path()).unwrap();
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

/// A disk that is not mounted is not an empty place: its mount point lacks
/// the place's marker, so the place is offline and nothing lands on the host
/// disk underneath.
#[test]
fn an_unmounted_disk_is_offline_not_empty() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("usb");
    let place = FsPlace::create(&root).unwrap();
    assert!(place.capabilities().online);
    put_bytes(&place, &key_for(b"photo", ""), b"photo").unwrap();

    // Unplugged: the mount point is an empty directory again.
    std::fs::remove_dir_all(&root).unwrap();
    std::fs::create_dir(&root).unwrap();
    assert!(!place.capabilities().online);
    assert!(matches!(
        put_bytes(&place, &key_for(b"more", ""), b"more"),
        Err(PlaceError::Offline)
    ));
    assert!(matches!(place.inventory(), Err(PlaceError::Offline)));
    assert!(matches!(place.list(None), Err(PlaceError::Offline)));
    assert!(matches!(
        place.exists(&key_for(b"photo", "")),
        Err(PlaceError::Offline)
    ));
    assert_eq!(
        std::fs::read_dir(&root).unwrap().count(),
        0,
        "nothing written underneath"
    );

    assert!(matches!(
        FsPlace::open(&root),
        Err(PlaceError::NotAPlace { .. })
    ));
    assert!(matches!(
        FsPlace::open(dir.path().join("never")),
        Err(PlaceError::NotAPlace { .. })
    ));
    assert!(FsPlace::open(dir.path().join("usb2")).is_err());
    FsPlace::create(dir.path().join("usb2")).unwrap();
    assert!(FsPlace::open(dir.path().join("usb2")).is_ok());
}

/// Something other than a file at a key's path is not the object.
#[test]
fn a_directory_at_a_key_is_not_the_object() {
    let dir = tempfile::tempdir().unwrap();
    let place = FsPlace::create(dir.path()).unwrap();
    let key = key_for(b"blocked", "");
    std::fs::create_dir_all(dir.path().join(key.as_str())).unwrap();
    assert!(put_bytes(&place, &key, b"blocked").is_err());
    assert!(!place.exists(&key).unwrap());
}

/// A store moved to another disk behind a symlink is still walked.
#[cfg(unix)]
#[test]
fn a_symlinked_directory_is_followed() {
    let dir = tempfile::tempdir().unwrap();
    let other = tempfile::tempdir().unwrap();
    let elsewhere = FsPlace::create(other.path()).unwrap();
    let key = key_for(b"moved", "");
    put_bytes(&elsewhere, &key, b"moved").unwrap();
    let place = FsPlace::create(dir.path()).unwrap();
    std::os::unix::fs::symlink(other.path().join("store"), dir.path().join("store")).unwrap();
    assert_eq!(
        place.list(None).unwrap(),
        vec![Child::Prefix(Key::new("store").unwrap())]
    );
    assert_eq!(place.inventory().unwrap().len(), 1);
}

/// Crashed puts leave temporaries; a sweep clears old ones.
#[test]
fn a_sweep_clears_old_temporaries() {
    let dir = tempfile::tempdir().unwrap();
    let place = FsPlace::create(dir.path()).unwrap();
    let key = key_for(b"x", "");
    put_bytes(&place, &key, b"x").unwrap();
    let shard = dir.path().join(key.parent().unwrap().as_str());
    std::fs::write(shard.join(".tmp-1-2-3"), b"half").unwrap();
    assert_eq!(
        place
            .sweep_temporaries(std::time::Duration::from_secs(3600))
            .unwrap(),
        0
    );
    assert_eq!(
        place.sweep_temporaries(std::time::Duration::ZERO).unwrap(),
        1
    );
    assert!(!shard.join(".tmp-1-2-3").exists());
    assert!(place.exists(&key).unwrap());
}

/// On a cold place the read-everything defaults refuse rather than run up a
/// bill; such a place verifies itself or not at all.
#[test]
fn the_defaults_refuse_on_a_cold_place() {
    let cold = MemPlace::with_capabilities(Capabilities {
        online: true,
        writable: true,
        cold: true,
    });
    let key = key_for(b"archived", "");
    put_bytes(&cold, &key, b"archived").unwrap();
    assert!(matches!(cold.verify(&key), Err(PlaceError::Cold)));
    assert!(matches!(cold.inventory(), Err(PlaceError::Cold)));
}

/// Places are usable behind a pointer, as a working copy holds its remotes.
#[test]
fn a_place_behind_a_pointer_is_a_place() {
    let shared: std::sync::Arc<dyn Place> = std::sync::Arc::new(MemPlace::new());
    let boxed: Box<dyn Place> = Box::new(MemPlace::new());
    let key = key_for(b"p", "");
    put_bytes(&shared, &key, b"p").unwrap();
    put_bytes(&boxed, &key, b"p").unwrap();
    put_bytes(&&*boxed, &key, b"p").unwrap();
    assert!(shared.exists(&key).unwrap() && boxed.exists(&key).unwrap());
}
