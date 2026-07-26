//! Phase 1 substrate tests — *a local job is the unit*.
//!
//! These exercise the job properties (id, live output, control handle, terminal
//! record) over real local commands, spawned argv-only, on an **ungoverned**
//! registry — the bare substrate with no rule-set in front. The governance gate
//! is covered separately in `governance.rs`.
//!
//! implements: job-is-the-unit-not-rpc
//! implements: argv-not-shell
//! implements: governed-command-is-a-job

use moco_job::{JobError, JobId, JobRegistry, JobRequest, JobStatus};
use std::env;
use std::time::Duration;

fn cwd() -> std::path::PathBuf {
    env::current_dir().unwrap_or_else(|_| ".".into())
}

/// An `echo` job captures its output (readable via `tail`) and exits 0.
#[test]
fn echo_job_captures_output_and_exits_zero() {
    let reg = JobRegistry::ungoverned().unwrap();
    let id = reg
        .start(JobRequest::new(["echo", "hello"], cwd()))
        .unwrap();

    let outcome = reg.wait(&id).unwrap();
    assert_eq!(outcome.status, JobStatus::Done { code: 0 });

    let tail = reg.tail(&id, 0).unwrap();
    assert!(
        String::from_utf8_lossy(&tail.bytes).contains("hello"),
        "expected job output to contain 'hello', got {:?}",
        String::from_utf8_lossy(&tail.bytes)
    );
    assert!(tail.status.is_terminal());
}

/// A long-running job can be killed, and its terminal record says so.
#[test]
fn sleep_job_can_be_killed() {
    let reg = JobRegistry::ungoverned().unwrap();
    let id = reg.start(JobRequest::new(["sleep", "10"], cwd())).unwrap();

    reg.kill(&id).unwrap();

    let outcome = reg.wait(&id).unwrap();
    assert_eq!(outcome.status, JobStatus::Killed);
}

/// A job that outlives its execution deadline lands `TimedOut`.
#[test]
fn job_past_deadline_times_out() {
    let reg = JobRegistry::ungoverned().unwrap();
    let req = JobRequest::new(["sleep", "10"], cwd()).with_deadline(Duration::from_millis(100));

    let outcome = reg.run(req).unwrap();
    assert_eq!(outcome.status, JobStatus::TimedOut);
}

/// `run` is exactly `start` + `wait` sugar and returns the terminal outcome.
#[test]
fn run_is_start_plus_wait_sugar() {
    let reg = JobRegistry::ungoverned().unwrap();
    let outcome = reg.run(JobRequest::new(["true"], cwd())).unwrap();
    assert_eq!(outcome.status, JobStatus::Done { code: 0 });
}

/// Concurrent jobs on one registry get distinct ids.
#[test]
fn two_jobs_get_distinct_ids() {
    let reg = JobRegistry::ungoverned().unwrap();
    let a = reg.start(JobRequest::new(["true"], cwd())).unwrap();
    let b = reg.start(JobRequest::new(["true"], cwd())).unwrap();
    assert_ne!(a, b);
    reg.wait(&a).unwrap();
    reg.wait(&b).unwrap();
}

/// `tail` reads incrementally: reading from a returned `next_offset` yields
/// nothing new and the offset does not move.
#[test]
fn tail_reads_incrementally_from_offset() {
    let reg = JobRegistry::ungoverned().unwrap();
    let id = reg
        .start(JobRequest::new(["echo", "hello"], cwd()))
        .unwrap();
    reg.wait(&id).unwrap();

    let first = reg.tail(&id, 0).unwrap();
    assert!(!first.bytes.is_empty());

    let second = reg.tail(&id, first.next_offset).unwrap();
    assert!(second.bytes.is_empty());
    assert_eq!(second.next_offset, first.next_offset);
}

/// Spawning a program that does not exist is a legible `Spawn` error.
#[test]
fn spawn_of_missing_program_errors() {
    let reg = JobRegistry::ungoverned().unwrap();
    let err = reg
        .start(JobRequest::new(
            ["definitely-not-a-real-program-xyz"],
            cwd(),
        ))
        .unwrap_err();
    assert!(matches!(err, JobError::Spawn { .. }), "got {err:?}");
}

/// A job with no program is rejected before any spawn.
#[test]
fn empty_argv_is_rejected() {
    let reg = JobRegistry::ungoverned().unwrap();
    let err = reg
        .start(JobRequest::new(Vec::<String>::new(), cwd()))
        .unwrap_err();
    assert!(matches!(err, JobError::EmptyArgv), "got {err:?}");
}

/// Operating on an unknown id is `NotFound`, not a panic.
#[test]
fn unknown_job_is_not_found() {
    let reg = JobRegistry::ungoverned().unwrap();
    let err = reg.wait(&JobId::new("no-such-job")).unwrap_err();
    assert!(matches!(err, JobError::NotFound(_)), "got {err:?}");
}
