//! Hashes and keys: the names everything in a place goes by.
//!
//! implements: a7089331ac0dff49deb5acc9afce92b2a9d8a9d5022f66af915f3a2332d6f447@a7089331ac0dff49deb5acc9afce92b2a9d8a9d5022f66af915f3a2332d6f447

#![allow(clippy::unwrap_used, clippy::expect_used, reason = "tests")]

use moco_place::{Hash, HashParseError, Hasher, Key};

const EMPTY: &str = "af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262";

/// The same hashes kinora writes: BLAKE3, 64 lowercase hex digits.
#[test]
fn a_hash_is_kinoras_blake3_hex() {
    let h = Hash::of_content(b"");
    assert_eq!(h.as_hex(), EMPTY);
    assert_eq!(h.shard(), "af");
    assert_eq!(h.shorthash(), "af1349b9");
    assert_eq!(h.to_string(), EMPTY);
    assert_eq!(EMPTY.parse::<Hash>().unwrap(), h);
}

#[test]
fn a_hash_parses_only_from_64_lowercase_hex_digits() {
    assert_eq!(
        "abc".parse::<Hash>(),
        Err(HashParseError::WrongLength { got: 3 })
    );
    assert_eq!(
        EMPTY.to_uppercase().parse::<Hash>(),
        Err(HashParseError::NotLowerHex)
    );
}

/// A stream hashes to what its bytes in one piece hash to.
#[test]
fn a_stream_hashes_as_its_bytes_do() {
    let bytes: Vec<u8> = (0..100_000u32).map(|i| (i % 251) as u8).collect();
    let mut h = Hasher::new();
    for chunk in bytes.chunks(777) {
        h.update(chunk);
    }
    assert_eq!(h.len(), bytes.len() as u64);
    assert_eq!(h.finalize(), Hash::of_content(&bytes));
    let (hash, len) = Hash::of_reader(&mut &bytes[..]).unwrap();
    assert_eq!((hash, len), (Hash::of_content(&bytes), bytes.len() as u64));
}

#[test]
fn a_key_is_a_relative_path_of_plain_segments() {
    for good in [
        "store",
        "store/ab",
        "roots/main/1700000000-x.styxl",
        "a_b-c.d",
    ] {
        assert!(Key::new(good).is_ok(), "{good}");
    }
    for bad in [
        "",
        "/abs",
        "trailing/",
        "a//b",
        "..",
        "a/../b",
        ".tmp-1",
        "a/.hidden",
        "sp ace",
        "back\\slash",
        "ü",
    ] {
        assert!(Key::new(bad).is_err(), "{bad:?} should be refused");
    }
}

#[test]
fn a_key_knows_its_name_and_parent() {
    let k = Key::new("store/ab/x.md").unwrap();
    assert_eq!(k.name(), "x.md");
    assert_eq!(k.parent(), Some(Key::new("store/ab").unwrap()));
    assert_eq!(Key::new("store").unwrap().parent(), None);
    assert_eq!(
        Key::new("store").unwrap().join("ab").unwrap(),
        Key::new("store/ab").unwrap()
    );
    assert!(Key::new("store").unwrap().join("../x").is_err());
}

/// The last segment names the content: `<hash>`, `<hash>.<ext>`,
/// `<prefix>-<hash>`, `<prefix>-<hash>.<ext>`.
#[test]
fn a_key_names_its_content_hash() {
    let h: Hash = EMPTY.parse().unwrap();
    for named in [
        format!("store/af/{EMPTY}"),
        format!("store/af/{EMPTY}.md"),
        format!("roots/main/1700000000-{EMPTY}"),
        format!("roots/main/1700000000-{EMPTY}.styxl"),
    ] {
        assert_eq!(
            Key::new(&named).unwrap().named_hash(),
            Some(h.clone()),
            "{named}"
        );
    }
    for unnamed in ["store/af", "notes.md", "1700-abc"] {
        assert_eq!(Key::new(unnamed).unwrap().named_hash(), None, "{unnamed}");
    }
}
