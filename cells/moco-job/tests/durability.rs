//! Phase 4 durability tests — *the registry is node state on disk*.
//!
//! The property under test is that a job outlives the registry that started it.
//! Everything here works a **shared directory**: one registry starts a job, that
//! registry goes away, and a second registry opened on the same directory has to
//! pick the job back up — or say honestly that it cannot know how it ended.
//!
//! implements: registry-is-node-state-on-disk
//! implements: job-durability-both-kill-vectors

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use moco_job::{JobRegistry, JobRequest, JobStatus, RecordStore};

static DIR_SEQ: AtomicU64 = AtomicU64::new(0);

/// A fresh shared directory for one test.
fn shared_dir() -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "moco-job-durability-{}-{}",
        std::process::id(),
        DIR_SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

fn root() -> PathBuf {
    std::env::temp_dir()
}

/// Spin until `f` holds or the budget runs out, so the tests do not race a
/// child's exit on a loaded machine.
fn until(budget: Duration, mut f: impl FnMut() -> bool) -> bool {
    let start = Instant::now();
    while start.elapsed() < budget {
        if f() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    f()
}

/// The headline: a job survives the registry that started it.
///
/// The first registry starts a long sleep and is dropped. A second registry
/// opened on the same directory still knows the job, and its scrollback is still
/// readable — because the capture file is the scrollback, and neither the record
/// nor the file is removed when a registry goes away.
#[test]
fn a_job_survives_the_registry_that_started_it() {
    let dir = shared_dir();

    let (id, pid) = {
        let reg = JobRegistry::ungoverned().unwrap().with_dir(&dir).unwrap();
        let id = reg
            .start(JobRequest::new(
                ["sh", "-c", "echo alive; sleep 30"],
                root(),
            ))
            .unwrap();
        // Wait for the greeting so there is scrollback to find after the restart.
        assert!(until(Duration::from_secs(5), || {
            reg.tail(&id, 0)
                .map(|t| !t.bytes.is_empty())
                .unwrap_or(false)
        }));
        let pid = RecordStore::open(&dir)
            .unwrap()
            .all()
            .unwrap()
            .into_iter()
            .find(|r| r.id == id.0)
            .map(|r| r.pid)
            .unwrap();
        (id, pid)
    }; // first registry dropped here

    let reopened = JobRegistry::ungoverned().unwrap().with_dir(&dir).unwrap();
    let listed = reopened.list();
    assert_eq!(listed.len(), 1, "the job is still known after a restart");
    assert_eq!(listed[0].0, id, "and keeps the same id");
    assert_eq!(
        listed[0].1,
        JobStatus::Running,
        "a live process is still reported running"
    );

    let tail = reopened.tail(&id, 0).unwrap();
    assert!(
        String::from_utf8_lossy(&tail.bytes).contains("alive"),
        "its scrollback survives re-adoption"
    );

    // Cleanup: this job was deliberately not killed by the drop, so kill it now.
    let _ = std::process::Command::new("kill")
        .arg("-9")
        .arg(pid.to_string())
        .status();
    let _ = std::fs::remove_dir_all(&dir);
}

/// A re-adopted job whose process is gone reports `OutcomeUnknown` — never a
/// fabricated exit code.
#[test]
fn a_re_adopted_job_that_ended_reports_outcome_unknown() {
    let dir = shared_dir();

    let id = {
        let reg = JobRegistry::ungoverned().unwrap().with_dir(&dir).unwrap();
        let id = reg
            .start(JobRequest::new(["sh", "-c", "sleep 30"], root()))
            .unwrap();
        // Kill it *without* telling this registry, so the record still says
        // `Running` when the registry goes away — exactly the state a crashed
        // daemon leaves behind.
        let record = RecordStore::open(&dir)
            .unwrap()
            .all()
            .unwrap()
            .into_iter()
            .find(|r| r.id == id.0)
            .unwrap();
        let _ = std::process::Command::new("kill")
            .arg("-9")
            .arg(record.pid.to_string())
            .status();
        // Probe with the *recorded* start time — the same pair re-adoption will
        // use. Probing against 0 would report `Dead` whatever the process did,
        // and the test would pass without the kill having worked.
        assert!(
            until(Duration::from_secs(5), || {
                moco_job::liveness(record.pid, record.pid_start) == moco_job::Liveness::Dead
            }),
            "the child must actually be gone before the registry is re-opened"
        );
        // Leak rather than drop: a daemon that *crashed* never ran its
        // teardown, and that is the state re-adoption has to cope with. A clean
        // drop would reap the child and write a real terminal status, which is a
        // different scenario.
        std::mem::forget(reg);
        id
    };

    let reopened = JobRegistry::ungoverned().unwrap().with_dir(&dir).unwrap();
    let outcome = reopened.wait(&id).unwrap();
    assert_eq!(
        outcome.status,
        JobStatus::OutcomeUnknown,
        "we were not its parent, so there is no exit code to report"
    );
    assert!(
        outcome.code().is_none(),
        "and no code may be invented for it"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// A recycled pid must never be mistaken for the original job.
///
/// The guard is the recorded start time: same pid, different start, means the
/// original process ended and something else now wears its number.
#[test]
fn a_recycled_pid_is_not_mistaken_for_the_original_job() {
    if !moco_job::procfs::PROBE_SUPPORTED {
        return;
    }
    let pid = std::process::id();
    let real_start = moco_job::procfs::start_time(pid).unwrap();

    assert_eq!(
        moco_job::liveness(pid, real_start),
        moco_job::Liveness::Alive,
        "the same process, matched by start time"
    );
    assert_eq!(
        moco_job::liveness(pid, real_start.wrapping_add(1)),
        moco_job::Liveness::Dead,
        "a live pid with a different start time is a *different* process"
    );
}

/// Two registries sharing one directory never hand out the same id, because the
/// id is claimed by exclusive file creation rather than by a local counter.
#[test]
fn ids_from_two_registries_over_one_directory_never_collide() {
    let dir = shared_dir();

    let a = JobRegistry::ungoverned().unwrap().with_dir(&dir).unwrap();
    let b = JobRegistry::ungoverned().unwrap().with_dir(&dir).unwrap();

    let mut ids = Vec::new();
    for _ in 0..8 {
        ids.push(a.start(JobRequest::new(["true"], root())).unwrap());
        ids.push(b.start(JobRequest::new(["true"], root())).unwrap());
    }

    let unique: std::collections::HashSet<_> = ids.iter().collect();
    assert_eq!(unique.len(), ids.len(), "every id is distinct: {ids:?}");

    let _ = std::fs::remove_dir_all(&dir);
}

/// Dropping a registry does not kill a running job — the durability contract's
/// other half. If it did, no job could ever be re-adopted.
#[test]
fn dropping_a_registry_does_not_kill_a_running_job() {
    let dir = shared_dir();

    let pid = {
        let reg = JobRegistry::ungoverned().unwrap().with_dir(&dir).unwrap();
        let id = reg
            .start(JobRequest::new(["sh", "-c", "sleep 30"], root()))
            .unwrap();
        RecordStore::open(&dir)
            .unwrap()
            .all()
            .unwrap()
            .into_iter()
            .find(|r| r.id == id.0)
            .map(|r| r.pid)
            .unwrap()
    }; // dropped

    let start = moco_job::procfs::start_time(pid).unwrap_or(0);
    if moco_job::procfs::PROBE_SUPPORTED {
        assert_eq!(
            moco_job::liveness(pid, start),
            moco_job::Liveness::Alive,
            "the child must outlive the registry that spawned it"
        );
    }

    let _ = std::process::Command::new("kill")
        .arg("-9")
        .arg(pid.to_string())
        .status();
    let _ = std::fs::remove_dir_all(&dir);
}

/// A record round-trips through the store byte-for-byte, including an argv
/// carrying newlines — which would otherwise split one record across lines and
/// make the directory unreadable.
#[test]
fn records_round_trip_including_multiline_argv() {
    let dir = shared_dir();
    let reg = JobRegistry::ungoverned().unwrap().with_dir(&dir).unwrap();

    let nasty = "a\nb\r\nc\\d";
    let id = reg.start(JobRequest::new(["echo", nasty], root())).unwrap();
    reg.wait(&id).unwrap();

    let records = RecordStore::open(&dir).unwrap().all().unwrap();
    let record = records.iter().find(|r| r.id == id.0).unwrap();
    assert_eq!(
        record.argv,
        vec!["echo".to_string(), nasty.to_string()],
        "argv survives escaping and unescaping exactly"
    );

    let _ = std::fs::remove_dir_all(&dir);
}
