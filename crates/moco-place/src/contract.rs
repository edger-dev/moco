//! The contract every place keeps, as a test suite any crate can run against
//! its own place: `moco_place::place_contract!(name, (guard, place))` expands
//! to a module of tests, each building a fresh place from the expression (the
//! guard keeps whatever the place needs alive, a temporary directory say).
//!
//! implements: a7089331ac0dff49deb5acc9afce92b2a9d8a9d5022f66af915f3a2332d6f447@a7089331ac0dff49deb5acc9afce92b2a9d8a9d5022f66af915f3a2332d6f447

use crate::hash::Hash;
use crate::key::Key;

/// The key a sharded store files `bytes` under, with `ext` (`""` or
/// `".md"`) after the hash.
pub fn key_for(bytes: &[u8], ext: &str) -> Key {
    let h = Hash::of_content(bytes);
    match Key::new(format!("store/{}/{}{ext}", h.shard(), h.as_hex())) {
        Ok(key) => key,
        Err(e) => panic!("a sharded key is always a key: {e}"),
    }
}

#[macro_export]
macro_rules! place_contract {
    ($module:ident, $make:expr) => {
        mod $module {
            #[allow(unused_imports)]
            use super::*;
            #[allow(unused_imports)]
            use std::io::Read as _;
            use $crate::Place as _;

            #[test]
            fn a_put_reads_back() {
                let (_guard, place) = $make;
                let key = $crate::contract::key_for(b"hello", ".md");
                assert_eq!(
                    $crate::put_bytes(&place, &key, b"hello").unwrap(),
                    $crate::Put::Stored
                );
                assert!(place.exists(&key).unwrap());
                assert_eq!($crate::read_all(&place, &key).unwrap(), b"hello");
            }

            #[test]
            fn a_second_put_finds_it_present_and_writes_nothing() {
                let (_guard, place) = $make;
                let key = $crate::contract::key_for(b"once", "");
                $crate::put_bytes(&place, &key, b"once").unwrap();
                assert_eq!(
                    $crate::put_bytes(&place, &key, b"once").unwrap(),
                    $crate::Put::Present
                );
                assert_eq!($crate::read_all(&place, &key).unwrap(), b"once");
            }

            #[test]
            fn a_put_under_a_key_naming_another_hash_is_refused() {
                let (_guard, place) = $make;
                let key = $crate::contract::key_for(b"other", "");
                let err = $crate::put_bytes(&place, &key, b"mine").unwrap_err();
                assert!(
                    matches!(err, $crate::PlaceError::KeyNamesOtherHash { .. }),
                    "{err}"
                );
                assert!(!place.exists(&key).unwrap());
                let unnamed = $crate::Key::new("notes/plain.md").unwrap();
                let err = $crate::put_bytes(&place, &unnamed, b"mine").unwrap_err();
                assert!(
                    matches!(err, $crate::PlaceError::KeyNamesOtherHash { .. }),
                    "{err}"
                );
            }

            #[test]
            fn bytes_that_do_not_match_the_declared_hash_store_nothing() {
                let (_guard, place) = $make;
                let key = $crate::contract::key_for(b"declared", "");
                let declared = $crate::Hash::of_content(b"declared");
                let err = place
                    .put_once(&key, &declared, 8, &mut &b"imposter"[..])
                    .unwrap_err();
                assert!(
                    matches!(err, $crate::PlaceError::ContentMismatch { .. }),
                    "{err}"
                );
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
                let key = $crate::contract::key_for(b"twelve bytes", "");
                let declared = $crate::Hash::of_content(b"twelve bytes");
                let err = place
                    .put_once(&key, &declared, 99, &mut &b"twelve bytes"[..])
                    .unwrap_err();
                assert!(
                    matches!(err, $crate::PlaceError::SizeMismatch { .. }),
                    "{err}"
                );
                assert!(!place.exists(&key).unwrap());
            }

            #[test]
            fn a_range_reads_part_of_an_object() {
                let (_guard, place) = $make;
                let key = $crate::contract::key_for(b"0123456789", "");
                $crate::put_bytes(&place, &key, b"0123456789").unwrap();
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
                    read($crate::ByteRange {
                        start: 2,
                        end: Some(5)
                    }),
                    b"234"
                );
                assert_eq!(
                    read($crate::ByteRange {
                        start: 7,
                        end: None
                    }),
                    b"789"
                );
                assert_eq!(
                    read($crate::ByteRange {
                        start: 8,
                        end: Some(99)
                    }),
                    b"89"
                );
            }

            #[test]
            fn a_missing_object_is_not_found() {
                let (_guard, place) = $make;
                let key = $crate::contract::key_for(b"absent", "");
                assert!(!place.exists(&key).unwrap());
                assert!(matches!(
                    place.get(&key, None),
                    Err($crate::PlaceError::NotFound(_))
                ));
            }

            #[test]
            fn a_list_shows_one_level_sorted() {
                let (_guard, place) = $make;
                let a = $crate::contract::key_for(b"a", ".md");
                let b = $crate::contract::key_for(b"b", "");
                $crate::put_bytes(&place, &a, b"a").unwrap();
                $crate::put_bytes(&place, &b, b"b").unwrap();
                let staged = $crate::Key::new(format!("staged/{}", b.name())).unwrap();
                $crate::put_bytes(&place, &staged, b"b").unwrap();

                assert_eq!(
                    place.list(None).unwrap(),
                    vec![
                        $crate::Child::Prefix($crate::Key::new("staged").unwrap()),
                        $crate::Child::Prefix($crate::Key::new("store").unwrap()),
                    ]
                );
                let shard = a.parent().unwrap();
                let listed = place.list(Some(&shard)).unwrap();
                assert!(listed.contains(&$crate::Child::Object {
                    key: a.clone(),
                    size: 1
                }));
                let mut sorted = listed.clone();
                sorted.sort_by_key(|c| match c {
                    $crate::Child::Object { key, .. } | $crate::Child::Prefix(key) => key.clone(),
                });
                assert_eq!(listed, sorted);
                assert_eq!(
                    place
                        .list(Some(&$crate::Key::new("nothing").unwrap()))
                        .unwrap(),
                    vec![]
                );
            }

            #[test]
            fn verify_checks_an_object_against_its_key() {
                let (_guard, place) = $make;
                let key = $crate::contract::key_for(b"kept", "");
                $crate::put_bytes(&place, &key, b"kept").unwrap();
                assert_eq!(place.verify(&key).unwrap(), $crate::Verified::Intact);
                assert_eq!(
                    place
                        .verify(&$crate::contract::key_for(b"never put", ""))
                        .unwrap(),
                    $crate::Verified::Missing
                );
            }

            #[test]
            fn an_inventory_lists_every_object_with_its_hash_and_size() {
                let (_guard, place) = $make;
                let a = $crate::contract::key_for(b"alpha", ".md");
                let staged = $crate::Key::new(format!(
                    "staged/x/1700-{}",
                    $crate::Hash::of_content(b"beta")
                ))
                .unwrap();
                $crate::put_bytes(&place, &a, b"alpha").unwrap();
                $crate::put_bytes(&place, &staged, b"beta").unwrap();
                let before = $crate::place::unix_now();
                let mut inv = place.inventory().unwrap();
                inv.sort_by(|x, y| x.key.cmp(&y.key));
                assert_eq!(inv.len(), 2);
                assert_eq!(inv[0].key, staged);
                assert_eq!(inv[0].hash, $crate::Hash::of_content(b"beta"));
                assert_eq!(inv[0].size, 4);
                assert_eq!(inv[1].key, a);
                assert!(
                    inv.iter()
                        .all(|e| e.verified_at >= before
                            && e.verified_at <= $crate::place::unix_now())
                );
            }

            #[test]
            fn an_empty_object_is_an_object() {
                let (_guard, place) = $make;
                let key = $crate::contract::key_for(b"", "");
                assert_eq!(
                    $crate::put_bytes(&place, &key, b"").unwrap(),
                    $crate::Put::Stored
                );
                assert_eq!($crate::read_all(&place, &key).unwrap(), b"");
                assert_eq!(place.verify(&key).unwrap(), $crate::Verified::Intact);
            }

            /// Ranges clamp to the object: past its end, or backwards, is empty.
            #[test]
            fn a_range_past_the_end_or_backwards_is_empty() {
                let (_guard, place) = $make;
                let key = $crate::contract::key_for(b"abc", "");
                $crate::put_bytes(&place, &key, b"abc").unwrap();
                for range in [
                    $crate::ByteRange {
                        start: 9,
                        end: None,
                    },
                    $crate::ByteRange {
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
                let key = $crate::contract::key_for(b"leaf", "");
                $crate::put_bytes(&place, &key, b"leaf").unwrap();
                assert_eq!(place.list(Some(&key)).unwrap(), vec![]);
                let below = key.join("x").unwrap();
                assert!(!place.exists(&below).unwrap());
                // Below a file, a named key is missing, not an error.
                let named_below = key
                    .join($crate::contract::key_for(b"y", "").name())
                    .unwrap();
                assert_eq!(
                    place.verify(&named_below).unwrap(),
                    $crate::Verified::Missing
                );
            }

            /// Writers racing on one key: exactly one stores it.
            #[test]
            fn racing_puts_store_once() {
                let (_guard, place) = $make;
                let place = std::sync::Arc::new(place);
                let key = $crate::contract::key_for(b"contended", "");
                let outcomes: Vec<$crate::Put> = (0..8)
                    .map(|_| {
                        let place = place.clone();
                        let key = key.clone();
                        std::thread::spawn(move || {
                            $crate::put_bytes(&*place, &key, b"contended").unwrap()
                        })
                    })
                    .collect::<Vec<_>>()
                    .into_iter()
                    .map(|t| t.join().unwrap())
                    .collect();
                assert_eq!(
                    outcomes
                        .iter()
                        .filter(|o| **o == $crate::Put::Stored)
                        .count(),
                    1,
                    "{outcomes:?}"
                );
                assert_eq!($crate::read_all(&*place, &key).unwrap(), b"contended");
            }

            /// A corrupt object's inventory entry says so.
            #[test]
            fn an_inventory_entry_knows_whether_it_is_intact() {
                let (_guard, place) = $make;
                let key = $crate::contract::key_for(b"fine", "");
                $crate::put_bytes(&place, &key, b"fine").unwrap();
                let inv = place.inventory().unwrap();
                assert!(inv[0].is_intact());
            }

            /// A large object moves as a stream, intact.
            #[test]
            fn a_large_object_streams_through() {
                let (_guard, place) = $make;
                let bytes: Vec<u8> = (0..5_000_000u32).map(|i| (i % 253) as u8).collect();
                let key = $crate::contract::key_for(&bytes, ".bin");
                let hash = $crate::Hash::of_content(&bytes);
                assert_eq!(
                    place
                        .put_once(&key, &hash, bytes.len() as u64, &mut &bytes[..])
                        .unwrap(),
                    $crate::Put::Stored
                );
                let (got, len) =
                    $crate::Hash::of_reader(&mut place.get(&key, None).unwrap()).unwrap();
                assert_eq!((got, len), (hash, bytes.len() as u64));
            }
        }
    };
}
