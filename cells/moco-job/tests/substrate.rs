//! Phase 1 substrate tests — *a local job is the unit*.
//!
//! These are the first failing tests of the v1 plan: they exercise the five job
//! properties (id, live output, control handle, terminal record) over real local
//! commands, spawned argv-only. They fail today because the registry methods are
//! scaffolded stubs (`JobError::NotImplemented`); the next step implements the
//! minimum to make them pass.
//!
//! implements: job-is-the-unit-not-rpc
//! implements: argv-not-shell
//! implements: governed-command-is-a-job

use moco_job::{JobRegistry, JobRequest, JobStatus};
use std::env;
use std::time::Duration;

fn cwd() -> std::path::PathBuf {
    env::current_dir().unwrap_or_else(|_| ".".into())
}

/// An `echo` job captures its output (readable via `tail`) and exits 0.
#[test]
fn echo_job_captures_output_and_exits_zero() {
    let reg = JobRegistry::new();
    let id = reg.start(JobRequest::new(["echo", "hello"], cwd())).unwrap();

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
    let reg = JobRegistry::new();
    let id = reg.start(JobRequest::new(["sleep", "10"], cwd())).unwrap();

    reg.kill(&id).unwrap();

    let outcome = reg.wait(&id).unwrap();
    assert_eq!(outcome.status, JobStatus::Killed);
}

/// A job that outlives its execution deadline lands `TimedOut`.
#[test]
fn job_past_deadline_times_out() {
    let reg = JobRegistry::new();
    let req = JobRequest::new(["sleep", "10"], cwd()).with_deadline(Duration::from_millis(100));

    let outcome = reg.run(req).unwrap();
    assert_eq!(outcome.status, JobStatus::TimedOut);
}

/// `run` is exactly `start` + `wait` sugar and returns the terminal outcome.
#[test]
fn run_is_start_plus_wait_sugar() {
    let reg = JobRegistry::new();
    let outcome = reg.run(JobRequest::new(["true"], cwd())).unwrap();
    assert_eq!(outcome.status, JobStatus::Done { code: 0 });
}
