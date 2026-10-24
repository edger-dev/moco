//! Streams: append-only entries in place of a mutable pointer.
//!
//! implements: c08aa5551e04cf03a5f4df25c0f647b02dd78d7eacfb3872cb197affc6b52d2a@c08aa5551e04cf03a5f4df25c0f647b02dd78d7eacfb3872cb197affc6b52d2a

#![allow(clippy::unwrap_used, clippy::expect_used, reason = "tests")]

use moco_place::{FsPlace, Hash, Key, MemPlace, Place, Stream, put_bytes, read_all};

fn stream() -> Stream {
    Stream::new(Key::new("roots/main").unwrap())
}

/// An entry's content names its parents: `parents <hash> <hash>…` on its
/// first line, the rest is payload.
fn entry(parents: &[&Hash], payload: &str) -> Vec<u8> {
    let names: Vec<&str> = parents.iter().map(|h| h.as_hex()).collect();
    format!("parents {}\n{payload}", names.join(" ")).into_bytes()
}

fn parents_of(bytes: &[u8]) -> Vec<Hash> {
    let text = String::from_utf8_lossy(bytes);
    text.lines()
        .next()
        .and_then(|l| l.strip_prefix("parents"))
        .map(|rest| {
            rest.split_whitespace()
                .filter_map(|h| h.parse().ok())
                .collect()
        })
        .unwrap_or_default()
}

#[test]
fn an_entry_is_named_by_its_time_and_hash() {
    let place = MemPlace::new();
    let e = stream()
        .append_at(&place, 1_700_000_000_123, b"v1")
        .unwrap();
    let h = Hash::of_content(b"v1");
    assert_eq!(e.ts, 1_700_000_000_123);
    assert_eq!(e.hash, h);
    assert_eq!(e.key.as_str(), format!("roots/main/001700000000123-{h}"));
    assert_eq!(read_all(&place, &e.key).unwrap(), b"v1");

    let styxl = stream()
        .with_ext("styxl")
        .append_at(&place, 5, b"v2")
        .unwrap();
    assert_eq!(
        styxl.key.as_str(),
        format!(
            "roots/main/000000000000005-{}.styxl",
            Hash::of_content(b"v2")
        )
    );
}

/// A plain append stamps the entry with the time now.
#[test]
fn an_append_is_stamped_now() {
    let place = MemPlace::new();
    let before = moco_place::place::unix_now() * 1000;
    let e = stream().append(&place, b"now").unwrap();
    assert!(e.ts >= before && e.ts <= before + 60_000, "{}", e.ts);
}

#[test]
fn entries_come_oldest_first_and_the_latest_is_last() {
    let place = MemPlace::new();
    let s = stream();
    s.append_at(&place, 300, b"c").unwrap();
    s.append_at(&place, 100, b"a").unwrap();
    s.append_at(&place, 2_000, b"d").unwrap();
    s.append_at(&place, 200, b"b").unwrap();
    let ts: Vec<u64> = s.entries(&place).unwrap().iter().map(|e| e.ts).collect();
    assert_eq!(ts, vec![100, 200, 300, 2_000]);
    assert_eq!(
        s.latest(&place).unwrap().unwrap().hash,
        Hash::of_content(b"d")
    );
    assert_eq!(
        Stream::new(Key::new("roots/none").unwrap())
            .latest(&place)
            .unwrap(),
        None
    );
}

/// The same content at the same time is one entry; put-once keeps it.
#[test]
fn the_same_entry_appended_twice_is_one() {
    let place = MemPlace::new();
    let s = stream();
    let a = s.append_at(&place, 7, b"same").unwrap();
    let b = s.append_at(&place, 7, b"same").unwrap();
    assert_eq!(a, b);
    assert_eq!(s.entries(&place).unwrap().len(), 1);
}

/// Two writers folding nothing leave two heads; the next writer names both
/// and leaves one.
#[test]
fn concurrent_appends_fork_and_the_next_one_folds() {
    let place = MemPlace::new();
    let s = stream();
    let root = s.append_at(&place, 1, &entry(&[], "root")).unwrap();
    assert_eq!(s.heads(&place, &parents_of).unwrap(), vec![root.clone()]);

    let laptop = s
        .append_at(&place, 2, &entry(&[&root.hash], "laptop"))
        .unwrap();
    let phone = s
        .append_at(&place, 3, &entry(&[&root.hash], "phone"))
        .unwrap();
    assert_eq!(
        s.heads(&place, &parents_of).unwrap(),
        vec![laptop.clone(), phone.clone()]
    );

    let fold = s
        .append_at(&place, 4, &entry(&[&laptop.hash, &phone.hash], "fold"))
        .unwrap();
    assert_eq!(s.heads(&place, &parents_of).unwrap(), vec![fold]);
}

/// Anything under the prefix not named like an entry is not one.
#[test]
fn only_entry_names_are_entries() {
    let place = MemPlace::new();
    let s = stream();
    s.append_at(&place, 1, b"real").unwrap();
    let h = Hash::of_content(b"stray");
    put_bytes(
        &place,
        &Key::new(format!("roots/main/{h}")).unwrap(),
        b"stray",
    )
    .unwrap();
    put_bytes(
        &place,
        &Key::new(format!("roots/main/sub/9-{h}")).unwrap(),
        b"stray",
    )
    .unwrap();
    put_bytes(
        &place,
        &Key::new(format!("roots/main/notatime-{h}")).unwrap(),
        b"stray",
    )
    .unwrap();
    let entries = s.entries(&place).unwrap();
    assert_eq!(entries.len(), 1, "{entries:?}");
}

/// A stream behaves the same on disk.
#[test]
fn a_stream_lives_on_disk_too() {
    let dir = tempfile::tempdir().unwrap();
    let place = FsPlace::new(dir.path());
    let s = stream();
    let a = s.append_at(&place, 1, &entry(&[], "a")).unwrap();
    let b = s.append_at(&place, 2, &entry(&[&a.hash], "b")).unwrap();
    assert!(place.exists(&b.key).unwrap());
    assert_eq!(s.entries(&place).unwrap(), vec![a, b.clone()]);
    assert_eq!(s.heads(&place, &parents_of).unwrap(), vec![b]);
}
