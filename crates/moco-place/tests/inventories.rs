//! Place-owned inventories: kept on put and verify, read without a walk,
//! published to a meeting point as snapshots and deltas.
//!
//! implements: 7778729bc8018fe949b0225f90ad2832501a9e82b9d03c973d10bdae568f7c8b@7778729bc8018fe949b0225f90ad2832501a9e82b9d03c973d10bdae568f7c8b
//! implements: 95cf2a2fda557b922e0e3ff0ddaf48c25d7e598de9358cb53985b8f1aa7b3aa0@95cf2a2fda557b922e0e3ff0ddaf48c25d7e598de9358cb53985b8f1aa7b3aa0

#![allow(clippy::unwrap_used, clippy::expect_used, reason = "tests")]

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use moco_place::{
    FsPlace, Hash, Inventoried, InventoryEntry, Key, LocalRemove, MemPlace, Place, Stream,
    Verified, published, put_bytes,
};

fn key_for(bytes: &[u8]) -> Key {
    let h = Hash::of_content(bytes);
    Key::new(format!("store/{}/{h}", h.shard())).unwrap()
}

/// A clock the test moves by hand.
fn clock() -> (Arc<AtomicU64>, impl Fn() -> u64 + Send + Sync + 'static) {
    let t = Arc::new(AtomicU64::new(1_000));
    let read = t.clone();
    (t, move || read.load(Ordering::Relaxed))
}

fn kept(dir: &std::path::Path) -> (Arc<AtomicU64>, Inventoried<FsPlace>) {
    let (t, c) = clock();
    let place = Inventoried::open(FsPlace::create(dir.join("place")).unwrap(), dir.join("inv"))
        .unwrap()
        .with_clock(c);
    (t, place)
}

fn entry(bytes: &[u8], verified_at: u64) -> InventoryEntry {
    InventoryEntry {
        key: key_for(bytes),
        hash: Hash::of_content(bytes),
        size: bytes.len() as u64,
        verified_at,
    }
}

/// A put is recorded as verified when it lands; reading the inventory reads
/// the record, not the disk.
#[test]
fn a_put_is_recorded_and_read_back_without_a_walk() {
    let dir = tempfile::tempdir().unwrap();
    let (_t, place) = kept(dir.path());
    put_bytes(&place, &key_for(b"alpha"), b"alpha").unwrap();
    assert_eq!(place.inventory().unwrap(), vec![entry(b"alpha", 1_000)]);

    // Rot the file behind the place's back: the record is unchanged until a
    // verify or a walk looks.
    let path = dir.path().join("place").join(key_for(b"alpha").as_str());
    std::fs::write(&path, b"rotten").unwrap();
    assert_eq!(place.inventory().unwrap(), vec![entry(b"alpha", 1_000)]);
}

/// A verify restamps an intact object, records a corrupt one's real hash,
/// and drops a missing one.
#[test]
fn a_verify_updates_the_record() {
    let dir = tempfile::tempdir().unwrap();
    let (t, place) = kept(dir.path());
    put_bytes(&place, &key_for(b"keep"), b"keep").unwrap();
    put_bytes(&place, &key_for(b"rot"), b"rot").unwrap();
    put_bytes(&place, &key_for(b"lose"), b"lose").unwrap();
    t.store(2_000, Ordering::Relaxed);

    assert_eq!(place.verify(&key_for(b"keep")).unwrap(), Verified::Intact);
    let root = dir.path().join("place");
    std::fs::write(root.join(key_for(b"rot").as_str()), b"tor").unwrap();
    assert!(matches!(
        place.verify(&key_for(b"rot")).unwrap(),
        Verified::Corrupt { .. }
    ));
    std::fs::remove_file(root.join(key_for(b"lose").as_str())).unwrap();
    assert_eq!(place.verify(&key_for(b"lose")).unwrap(), Verified::Missing);

    let inv = place.inventory().unwrap();
    let keep = inv.iter().find(|e| e.key == key_for(b"keep")).unwrap();
    assert_eq!(keep.verified_at, 2_000);
    let rot = inv.iter().find(|e| e.key == key_for(b"rot")).unwrap();
    assert_eq!(rot.hash, Hash::of_content(b"tor"));
    assert!(inv.iter().all(|e| e.key != key_for(b"lose")));
}

/// The record outlives the process: a reopened place reads the same.
#[test]
fn the_inventory_survives_reopening() {
    let dir = tempfile::tempdir().unwrap();
    {
        let (_t, place) = kept(dir.path());
        put_bytes(&place, &key_for(b"one"), b"one").unwrap();
        put_bytes(&place, &key_for(b"two"), b"two").unwrap();
    }
    let (_t, place) = kept(dir.path());
    let mut want = vec![entry(b"one", 1_000), entry(b"two", 1_000)];
    want.sort_by(|a, b| a.key.cmp(&b.key));
    assert_eq!(place.inventory().unwrap(), want);
    place.compact().unwrap();
    drop(place);
    let (_t, place) = kept(dir.path());
    assert_eq!(
        place.inventory().unwrap(),
        want,
        "compaction keeps every record"
    );
}

/// A walk rebuilds the record from what is on disk.
#[test]
fn a_rebuild_records_what_a_walk_finds() {
    let dir = tempfile::tempdir().unwrap();
    // Objects put straight into the place, behind the inventory's back.
    let raw = FsPlace::create(dir.path().join("place")).unwrap();
    put_bytes(&raw, &key_for(b"found"), b"found").unwrap();
    let (_t, place) = kept(dir.path());
    assert_eq!(place.inventory().unwrap(), vec![]);
    let rebuilt = place.rebuild().unwrap();
    assert_eq!(rebuilt.len(), 1);
    assert_eq!(place.inventory().unwrap(), rebuilt);
}

/// Removal is local: it drops the object and its record.
#[test]
fn a_local_removal_drops_the_record() {
    let dir = tempfile::tempdir().unwrap();
    let (_t, place) = kept(dir.path());
    put_bytes(&place, &key_for(b"gone"), b"gone").unwrap();
    assert!(place.remove(&key_for(b"gone")).unwrap());
    assert!(!place.exists(&key_for(b"gone")).unwrap());
    assert_eq!(place.inventory().unwrap(), vec![]);
    assert!(!place.remove(&key_for(b"gone")).unwrap());

    let mem = MemPlace::new();
    put_bytes(&mem, &key_for(b"m"), b"m").unwrap();
    assert!(mem.remove(&key_for(b"m")).unwrap());
    assert!(!mem.exists(&key_for(b"m")).unwrap());
}

/// Published to a meeting point, the inventory reads back from there alone:
/// a snapshot first, deltas after, removals included.
#[test]
fn a_published_inventory_reads_back_from_the_meeting_point() {
    let dir = tempfile::tempdir().unwrap();
    let meeting = MemPlace::new();
    let (_t, place) = kept(dir.path());
    put_bytes(&place, &key_for(b"a"), b"a").unwrap();
    put_bytes(&place, &key_for(b"b"), b"b").unwrap();
    place.publish(&meeting, "usb-1", false).unwrap();
    assert_eq!(
        published(&meeting, "usb-1").unwrap(),
        place.inventory().unwrap()
    );

    put_bytes(&place, &key_for(b"c"), b"c").unwrap();
    place.remove(&key_for(b"a")).unwrap();
    place.publish(&meeting, "usb-1", false).unwrap();
    assert_eq!(
        published(&meeting, "usb-1").unwrap(),
        place.inventory().unwrap()
    );

    let stream = Stream::new(Key::new("inventories/usb-1").unwrap());
    let entries = stream.entries(&meeting).unwrap();
    assert_eq!(entries.len(), 2, "a snapshot, then a delta");
    let delta = moco_place::read_all(&meeting, &entries[1].key).unwrap();
    assert!(
        delta.len() < 400,
        "the delta carries only what changed: {} bytes",
        delta.len()
    );

    // Nothing changed: nothing to publish.
    place.publish(&meeting, "usb-1", false).unwrap();
    assert_eq!(stream.entries(&meeting).unwrap().len(), 2);
    // Asked for, a fresh snapshot.
    place.publish(&meeting, "usb-1", true).unwrap();
    assert_eq!(stream.entries(&meeting).unwrap().len(), 3);
    assert_eq!(
        published(&meeting, "usb-1").unwrap(),
        place.inventory().unwrap()
    );
    assert_eq!(published(&meeting, "never").unwrap(), vec![]);
}

/// A crash mid-line leaves a torn tail; the next record starts on its own
/// line, survives a reopen, and is published.
#[test]
fn a_torn_last_line_does_not_swallow_the_next_record() {
    let dir = tempfile::tempdir().unwrap();
    let meeting = MemPlace::new();
    {
        let (_t, place) = kept(dir.path());
        put_bytes(&place, &key_for(b"first"), b"first").unwrap();
        place.publish(&meeting, "disk", false).unwrap();
    }
    use std::io::Write;
    let mut journal = std::fs::OpenOptions::new()
        .append(true)
        .open(dir.path().join("inv/journal"))
        .unwrap();
    journal.write_all(b"store/ab/torn-fragm").unwrap();
    drop(journal);

    {
        let (_t, place) = kept(dir.path());
        put_bytes(&place, &key_for(b"second"), b"second").unwrap();
        place.publish(&meeting, "disk", false).unwrap();
        assert_eq!(
            published(&meeting, "disk").unwrap(),
            place.inventory().unwrap()
        );
    }
    let (_t, place) = kept(dir.path());
    assert_eq!(place.inventory().unwrap().len(), 2);
    put_bytes(&place, &key_for(b"third"), b"third").unwrap();
    place.publish(&meeting, "disk", false).unwrap();
    assert_eq!(
        published(&meeting, "disk").unwrap(),
        place.inventory().unwrap()
    );
}

/// After a compaction the next publish is a snapshot, and it sorts after
/// every earlier entry even within one millisecond.
#[test]
fn a_publish_after_compaction_is_a_snapshot_that_sorts_last() {
    let dir = tempfile::tempdir().unwrap();
    let meeting = MemPlace::new();
    let (_t, place) = kept(dir.path());
    put_bytes(&place, &key_for(b"a"), b"a").unwrap();
    place.publish(&meeting, "disk", false).unwrap();
    put_bytes(&place, &key_for(b"b"), b"b").unwrap();
    place.publish(&meeting, "disk", false).unwrap();
    place.remove(&key_for(b"a")).unwrap();
    place.compact().unwrap();
    place.publish(&meeting, "disk", false).unwrap();
    let stream = Stream::new(Key::new("inventories/disk").unwrap());
    let entries = stream.entries(&meeting).unwrap();
    let last = moco_place::read_all(&meeting, &entries.last().unwrap().key).unwrap();
    assert!(
        last.starts_with(b"snapshot"),
        "{}",
        String::from_utf8_lossy(&last)
    );
    assert_eq!(
        published(&meeting, "disk").unwrap(),
        place.inventory().unwrap()
    );
}

/// Publishing under another name, or to another meeting point, starts with a
/// snapshot there: a delta never lands where its base is missing.
#[test]
fn a_new_name_or_meeting_point_gets_a_snapshot() {
    let dir = tempfile::tempdir().unwrap();
    let (first, second) = (MemPlace::new(), MemPlace::new());
    let (_t, place) = kept(dir.path());
    put_bytes(&place, &key_for(b"a"), b"a").unwrap();
    place.publish(&first, "disk", false).unwrap();
    put_bytes(&place, &key_for(b"b"), b"b").unwrap();
    place.publish(&second, "disk", false).unwrap();
    assert_eq!(
        published(&second, "disk").unwrap(),
        place.inventory().unwrap()
    );
    place.publish(&first, "other-name", false).unwrap();
    assert_eq!(
        published(&first, "other-name").unwrap(),
        place.inventory().unwrap()
    );
}

/// Deltas with no snapshot under them are not an inventory.
#[test]
fn deltas_without_a_snapshot_are_refused() {
    let meeting = MemPlace::new();
    let stream = Stream::new(Key::new("inventories/orphan").unwrap());
    stream
        .append_at(&meeting, 1, b"delta\nstore/ab/x -\n")
        .unwrap();
    assert!(published(&meeting, "orphan").is_err());
}

/// One inventory, one keeper: a second opener is turned away.
#[test]
fn a_second_keeper_of_one_inventory_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let (_t, first) = kept(dir.path());
    let second = Inventoried::open(
        FsPlace::create(dir.path().join("place")).unwrap(),
        dir.path().join("inv"),
    );
    assert!(second.is_err());
    drop(first);
    assert!(
        Inventoried::open(
            FsPlace::create(dir.path().join("place")).unwrap(),
            dir.path().join("inv"),
        )
        .is_ok()
    );
}

/// A put of an object already there but never recorded is recorded as never
/// verified.
#[test]
fn an_unrecorded_present_object_is_recorded_unverified() {
    let dir = tempfile::tempdir().unwrap();
    let raw = FsPlace::create(dir.path().join("place")).unwrap();
    put_bytes(&raw, &key_for(b"old"), b"old").unwrap();
    let (_t, place) = kept(dir.path());
    put_bytes(&place, &key_for(b"old"), b"old").unwrap();
    assert_eq!(place.inventory().unwrap(), vec![entry(b"old", 0)]);
}

/// A verify-only change is published as a delta.
#[test]
fn a_verify_is_published() {
    let dir = tempfile::tempdir().unwrap();
    let meeting = MemPlace::new();
    let (t, place) = kept(dir.path());
    put_bytes(&place, &key_for(b"v"), b"v").unwrap();
    place.publish(&meeting, "disk", false).unwrap();
    t.store(5_000, Ordering::Relaxed);
    place.verify(&key_for(b"v")).unwrap();
    place.publish(&meeting, "disk", false).unwrap();
    assert_eq!(published(&meeting, "disk").unwrap()[0].verified_at, 5_000);
}
