//! A job's scrollback is bounded, and a reader is told when it fell behind.
//!
//! implements: registry-is-node-state-on-disk

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use moco_job::scope::Scope;
use moco_job::{JobRegistry, JobRequest};

static SEQ: AtomicU64 = AtomicU64::new(0);

#[allow(
    clippy::expect_used,
    reason = "test helper: a failure is a broken harness"
)]
fn dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!(
        "moco-cap-{}-{}-{name}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).expect("create");
    d.canonicalize().expect("canonicalize")
}

#[allow(
    clippy::expect_used,
    reason = "test helper: a failure is a broken harness"
)]
fn capped(d: &PathBuf, cap: u64) -> JobRegistry {
    JobRegistry::ungoverned()
        .expect("registry")
        .with_dir(d)
        .expect("with_dir")
        .with_capture_cap(cap)
}

/// Write far more than the cap allows.
fn noisy() -> JobRequest {
    JobRequest::new(
        [
            "sh",
            "-c",
            "for i in $(seq 1 2000); do echo line-$i-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaa; done",
        ],
        std::env::temp_dir(),
    )
    .in_scope(Scope::workspace("/ws/noisy"))
}

/// **The file stops growing.** A redrawing TUI writes without bound; the
/// scrollback that holds it must not.
#[test]
fn a_capture_is_bounded() {
    let d = dir("bounded");
    let reg = capped(&d, 8 * 1024);

    let id = reg.start(noisy()).expect("start");
    reg.wait(&id).expect("wait");
    // Settling is where compaction happens, so read once more.
    let _ = reg.tail(&id, 0).expect("tail");

    let capture = reg.capture_path(&id).expect("capture path");
    let size = std::fs::metadata(&capture).expect("stat").len();
    assert!(
        size <= 32 * 1024,
        "a bounded capture must stay bounded, got {size} bytes"
    );

    let _ = std::fs::remove_dir_all(&d);
}

/// **Recent output is what survives.** Losing the tail of a run to keep its
/// beginning would drop exactly the part someone is looking at.
#[test]
fn compaction_keeps_the_most_recent_output() {
    let d = dir("recent");
    let reg = capped(&d, 8 * 1024);

    let id = reg.start(noisy()).expect("start");
    reg.wait(&id).expect("wait");

    let tail = reg.tail(&id, 0).expect("tail");
    let text = String::from_utf8_lossy(&tail.bytes);
    assert!(
        text.contains("line-2000-"),
        "the newest line must survive compaction"
    );
    assert!(
        !text.contains("line-1-a"),
        "and the oldest is what gives way"
    );

    let _ = std::fs::remove_dir_all(&d);
}

/// **A reader that fell behind is told so**, rather than handed bytes from the
/// wrong place. Offsets are logical — total bytes ever written — so compaction
/// cannot silently redirect a resume point onto different content.
#[test]
fn a_reader_that_fell_behind_is_told_how_much_it_missed() {
    let d = dir("behind");
    let reg = capped(&d, 8 * 1024);

    let id = reg.start(noisy()).expect("start");
    reg.wait(&id).expect("wait");
    let _ = reg.tail(&id, 0).expect("settle and compact");

    // Resume from the very beginning, which compaction has long since discarded.
    let stale = reg.tail(&id, 0).expect("tail");
    assert!(
        stale.skipped > 0,
        "the caller must learn that earlier output is gone, not silently miss it"
    );

    // And a caller reading from where it was told to resume misses nothing.
    let fresh = reg.tail(&id, stale.next_offset).expect("resume");
    assert_eq!(fresh.skipped, 0, "a current reader skipped nothing");
    assert!(fresh.bytes.is_empty(), "and there is nothing new yet");

    let _ = std::fs::remove_dir_all(&d);
}

/// Offsets stay **monotonic** across a compaction: they count bytes ever
/// written, so a resume point from before a compaction is still comparable to
/// one from after it.
#[test]
fn offsets_are_logical_and_never_go_backwards() {
    let d = dir("monotonic");
    let reg = capped(&d, 8 * 1024);

    let id = reg.start(noisy()).expect("start");
    reg.wait(&id).expect("wait");

    let first = reg.tail(&id, 0).expect("tail");
    let second = reg.tail(&id, 0).expect("tail again");
    assert!(
        second.next_offset >= first.next_offset,
        "a logical offset counts what was written, so it cannot rewind: \
         {} then {}",
        first.next_offset,
        second.next_offset
    );
    assert!(
        first.next_offset > 8 * 1024,
        "and it counts everything written, not just what is retained: {}",
        first.next_offset
    );

    let _ = std::fs::remove_dir_all(&d);
}

/// An uncapped job behaves exactly as before — nothing skipped, offsets are
/// simply byte positions.
#[test]
fn a_small_job_is_never_compacted() {
    let d = dir("small");
    let reg = capped(&d, 8 * 1024);

    let id = reg
        .start(JobRequest::new(["echo", "tiny"], std::env::temp_dir()))
        .expect("start");
    reg.wait(&id).expect("wait");

    let tail = reg.tail(&id, 0).expect("tail");
    assert_eq!(tail.skipped, 0);
    assert_eq!(tail.next_offset, tail.bytes.len() as u64);
    assert!(String::from_utf8_lossy(&tail.bytes).contains("tiny"));

    let _ = std::fs::remove_dir_all(&d);
}
