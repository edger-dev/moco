//! A job's exit survives its node restarting: where the node has its holder,
//! every job runs under it, and the holder writes the job's exit code beside
//! its capture — so a job that ends while no daemon is there to reap it still
//! reads back how it ended.
//!
//! implements: 0119d16e256891de4a71a14418f59c33419207f49792e58001639f8cbef567a6@d7ba7c887089648434bc819ad57473bb930242c27396d150b11617388bb85da0

#![allow(clippy::unwrap_used, clippy::expect_used, reason = "tests")]

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use moco_job::holder::HolderPaths;
use moco_job::{JobId, JobRegistry, JobRequest, JobStatus, Liveness, RecordStore};

static SEQ: AtomicU64 = AtomicU64::new(0);

fn dir() -> PathBuf {
    let d = std::env::temp_dir().join(format!(
        "moco-exit-{}-{}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&d);
    d
}

fn root() -> PathBuf {
    std::env::temp_dir()
}

fn holder() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_moco-pty-holder"))
}

/// A daemon with its holder, keeping its records in `d`.
fn daemon(d: &PathBuf) -> JobRegistry {
    JobRegistry::ungoverned()
        .unwrap()
        .with_dir(d)
        .unwrap()
        .with_pty_holder(holder())
}

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

fn record_of(d: &PathBuf, id: &JobId) -> moco_job::record::JobRecord {
    RecordStore::open(d)
        .unwrap()
        .all()
        .unwrap()
        .into_iter()
        .find(|r| r.id == id.0)
        .unwrap()
}

/// A log job under the holder behaves as it did without one: its output in
/// the capture, its own exit code, and its own pid apart from the holder's
/// (what resource sampling reads). The exit is written beside the capture.
#[test]
fn a_log_job_runs_under_the_holder_as_it_did_without() {
    let d = dir();
    let reg = daemon(&d);
    let id = reg
        .start(JobRequest::new(["sh", "-c", "echo held; exit 3"], root()))
        .unwrap();
    let outcome = reg.wait(&id).unwrap();
    assert_eq!(outcome.status, JobStatus::Done { code: 3 });
    let tail = reg.tail(&id, 0).unwrap();
    assert_eq!(String::from_utf8_lossy(&tail.bytes), "held\n");

    let capture = PathBuf::from(record_of(&d, &id).capture);
    let exit = std::fs::read_to_string(HolderPaths::beside(&capture).exit).unwrap();
    assert_eq!(exit.trim(), "3");

    let id = reg.start(JobRequest::new(["sleep", "30"], root())).unwrap();
    assert!(
        until(Duration::from_secs(5), || reg.job_pid_of(&id)
            != reg.pid_of(&id)),
        "the job's pid is published apart from the holder's"
    );
    reg.kill(&id, &moco_job::Caller::Console).unwrap();
    assert_eq!(reg.wait(&id).unwrap().status, JobStatus::Killed);
    let _ = std::fs::remove_dir_all(&d);
}

/// The case this exists for: the daemon goes away (a deploy restarting the
/// node), the job ends with no daemon to reap it, and the next daemon reads
/// its real exit.
#[test]
fn a_job_that_ends_while_no_daemon_is_there_keeps_its_exit() {
    let d = dir();
    let id = {
        let reg = daemon(&d);
        let id = reg
            .start(JobRequest::new(["sh", "-c", "sleep 0.5; exit 7"], root()))
            .unwrap();
        // A crashed daemon runs no teardown.
        std::mem::forget(reg);
        id
    };
    let record = record_of(&d, &id);
    assert!(
        until(Duration::from_secs(10), || {
            moco_job::liveness(record.pid, record.pid_start) == Liveness::Dead
        }),
        "the job (and its holder) ended"
    );
    let reopened = daemon(&d);
    assert_eq!(
        reopened.wait(&id).unwrap().status,
        JobStatus::Done { code: 7 }
    );
    let _ = std::fs::remove_dir_all(&d);
}

/// A job still running when the next daemon comes up, and ending under it,
/// reads its exit too (the next daemon is not its parent either).
#[test]
fn a_job_readopted_while_running_keeps_its_exit() {
    let d = dir();
    let id = {
        let reg = daemon(&d);
        let id = reg
            .start(JobRequest::new(["sh", "-c", "sleep 1; exit 4"], root()))
            .unwrap();
        std::mem::forget(reg);
        id
    };
    let reopened = daemon(&d);
    assert_eq!(reopened.status_of(&id), Some(JobStatus::Running));
    assert_eq!(
        reopened.wait(&id).unwrap().status,
        JobStatus::Done { code: 4 }
    );
    let _ = std::fs::remove_dir_all(&d);
}

/// A job ended by a signal reads as the holder exits: 128 + the signal.
#[test]
fn a_job_ended_by_a_signal_reads_as_128_plus_it() {
    let d = dir();
    let reg = daemon(&d);
    let id = reg
        .start(JobRequest::new(["sh", "-c", "kill -TERM $$"], root()))
        .unwrap();
    assert_eq!(
        reg.wait(&id).unwrap().status,
        JobStatus::Done { code: 128 + 15 }
    );
    let _ = std::fs::remove_dir_all(&d);
}

/// With no exit written (the holder itself was killed), the end is still
/// unknown — never invented.
#[test]
fn without_an_exit_the_end_stays_unknown() {
    let d = dir();
    let id = {
        let reg = daemon(&d);
        let id = reg.start(JobRequest::new(["sleep", "30"], root())).unwrap();
        std::mem::forget(reg);
        id
    };
    let record = record_of(&d, &id);
    // The whole group, holder and job, with no chance to write anything.
    let _ = std::process::Command::new("kill")
        .arg("-9")
        .arg(format!("-{}", record.pid))
        .status();
    assert!(until(Duration::from_secs(5), || {
        moco_job::liveness(record.pid, record.pid_start) == Liveness::Dead
    }));
    let reopened = daemon(&d);
    assert_eq!(
        reopened.wait(&id).unwrap().status,
        JobStatus::OutcomeUnknown
    );
    let _ = std::fs::remove_dir_all(&d);
}

/// A program that cannot start is refused at the start, as without the
/// holder — not reported later as an exit code.
#[test]
fn a_program_that_cannot_start_is_refused_at_the_start() {
    let d = dir();
    let reg = daemon(&d);
    let err = reg
        .start(JobRequest::new(["no-such-program-moco-test"], root()))
        .unwrap_err();
    assert!(
        err.to_string().contains("no-such-program-moco-test"),
        "{err}"
    );
    let err = reg
        .start(JobRequest::new(["/nonexistent/bin/tool"], root()))
        .unwrap_err();
    assert!(err.to_string().contains("/nonexistent/bin/tool"), "{err}");
    let _ = std::fs::remove_dir_all(&d);
}

/// A terminal job's holder writes the exit too.
#[test]
fn a_terminal_jobs_holder_writes_the_exit_too() {
    let d = dir();
    let reg = daemon(&d);
    let id = reg
        .start(
            JobRequest::new(["sh", "-c", "exit 5"], root())
                .with_human_view(moco_job::lens::HumanView::Terminal),
        )
        .unwrap();
    assert_eq!(reg.wait(&id).unwrap().status, JobStatus::Done { code: 5 });
    let capture = PathBuf::from(record_of(&d, &id).capture);
    let exit = std::fs::read_to_string(HolderPaths::beside(&capture).exit).unwrap();
    assert_eq!(exit.trim(), "5");
    let _ = std::fs::remove_dir_all(&d);
}
