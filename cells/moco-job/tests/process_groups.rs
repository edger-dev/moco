//! Killing a job kills the tree it started, not just the process we spawned.
//!
//! implements: a-job-is-a-process-group

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use moco_job::scope::Caller;
use moco_job::{JobRegistry, JobRequest, JobStatus};

static SEQ: AtomicU64 = AtomicU64::new(0);

#[allow(
    clippy::expect_used,
    reason = "test helper: a failure is a broken harness"
)]
fn dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!(
        "moco-pg-{}-{}-{name}",
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
fn registry(name: &str) -> (JobRegistry, PathBuf) {
    let d = dir(name);
    let reg = JobRegistry::ungoverned()
        .expect("registry")
        .with_dir(&d)
        .expect("with_dir");
    (reg, d)
}

/// Is `pid` a running process?
///
/// A **zombie** is not: its `/proc` entry lingers until someone reaps it, so
/// merely checking that the directory exists reports a process that has already
/// exited as still running. Exactly the trap `procfs::liveness` exists to avoid.
fn alive(pid: u32) -> bool {
    let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else {
        return false;
    };
    let Some((_, after)) = stat.rsplit_once(')') else {
        return false;
    };
    !matches!(after.split_whitespace().next(), Some("Z") | None)
}

/// Wait for `f`, so a test asserts on a settled state rather than a race.
fn until(mut f: impl FnMut() -> bool) -> bool {
    for _ in 0..100 {
        if f() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    false
}

/// **The pain this exists for.** A job that is a shell wrapping a real program
/// leaves that program running when only the shell is signalled — the job reads
/// as stopped while the thing it started keeps holding its port.
#[test]
#[allow(clippy::expect_used, reason = "a failure here is a broken harness")]
fn killing_a_job_kills_the_tree_it_started() {
    let (reg, d) = registry("tree");
    let marker = d.join("grandchild.pid");

    let id = reg
        .start(JobRequest::new(
            [
                "sh".to_string(),
                "-c".to_string(),
                format!("sleep 300 & echo $! > {}; wait", marker.display()),
            ],
            &d,
        ))
        .expect("start");

    assert!(
        until(|| marker.exists()),
        "the wrapped program never reported its pid"
    );
    let grandchild: u32 = std::fs::read_to_string(&marker)
        .expect("read")
        .trim()
        .parse()
        .expect("pid");
    assert!(alive(grandchild), "the wrapped program should be running");

    reg.kill(&id, &Caller::Console).expect("kill");

    assert!(
        until(|| !alive(grandchild)),
        "the wrapped program outlived the job — signalling only the shell is \
         exactly the failure process groups exist to prevent"
    );
}

/// Each job leads its own group, so one job's stop can never reach another's
/// processes.
#[test]
#[allow(clippy::expect_used, reason = "a failure here is a broken harness")]
fn every_job_leads_its_own_process_group() {
    let (reg, d) = registry("leader");
    let id = reg
        .start(JobRequest::new(["sleep".to_string(), "30".to_string()], &d))
        .expect("start");

    let pid = reg.pid_of(&id).expect("pid");
    let pgid = std::fs::read_to_string(format!("/proc/{pid}/stat"))
        .ok()
        .and_then(|s| {
            let after = s.rsplit_once(')')?.1.to_string();
            after.split_whitespace().nth(2)?.parse::<u32>().ok()
        })
        .expect("pgid from /proc");

    assert_eq!(
        pgid, pid,
        "a job must lead its own group, or a stop would reach the daemon's"
    );

    let _ = reg.kill(&id, &Caller::Console);
}

/// A stop asks first. A service gets the chance to flush and clean up, which is
/// the difference between a restart and a corrupted state file.
#[test]
#[allow(clippy::expect_used, reason = "a failure here is a broken harness")]
fn a_stop_sends_term_before_anything_harsher() {
    let (reg, d) = registry("term");
    let id = reg
        .start(JobRequest::new(
            [
                "sh".to_string(),
                "-c".to_string(),
                "trap 'echo CLEANED-UP; exit 0' TERM; while :; do sleep 0.05; done".to_string(),
            ],
            &d,
        ))
        .expect("start");

    // Let the trap be installed before signalling it.
    std::thread::sleep(Duration::from_millis(300));
    reg.kill(&id, &Caller::Console).expect("kill");

    assert!(
        until(|| {
            let read = reg.tail(&id, 0).expect("tail");
            String::from_utf8_lossy(&read.bytes).contains("CLEANED-UP")
        }),
        "the job never got a chance to shut down cleanly"
    );
}

/// **And escalates.** A job that ignores the polite signal is not left running
/// forever — the same tick that drives restart policy escalates it.
#[test]
#[allow(clippy::expect_used, reason = "a failure here is a broken harness")]
fn a_job_that_ignores_term_is_escalated_by_the_supervisor() {
    let (reg, d) = registry("escalate");
    let reg = reg.with_kill_grace(Duration::from_millis(200));

    let id = reg
        .start(JobRequest::new(
            [
                "sh".to_string(),
                "-c".to_string(),
                "trap '' TERM; while :; do sleep 0.05; done".to_string(),
            ],
            &d,
        ))
        .expect("start");

    std::thread::sleep(Duration::from_millis(300));
    reg.kill(&id, &Caller::Console).expect("kill");

    // It survives the polite signal, exactly as it was written to.
    std::thread::sleep(Duration::from_millis(150));
    assert_eq!(
        reg.status_of(&id),
        Some(JobStatus::Running),
        "a job that traps TERM should still be running before the grace expires"
    );

    assert!(
        until(|| {
            reg.supervise();
            reg.status_of(&id).is_some_and(|s| s == JobStatus::Killed)
        }),
        "the supervisor must escalate a job that ignored TERM"
    );
}

/// **An adopted job is killable.** It has no child handle, so the old path
/// signalled nothing at all and returned success — a caller that asked to stop
/// something and got a quiet `Ok` would believe it had.
#[test]
#[allow(clippy::expect_used, reason = "a failure here is a broken harness")]
fn an_adopted_job_can_actually_be_stopped() {
    let (reg, d) = registry("adopted");

    // A process this registry did not start.
    let mut outsider = std::process::Command::new("sleep")
        .arg("300")
        .current_dir(&d)
        .spawn()
        .expect("spawn");
    let pid = outsider.id();

    let id = reg
        .adopt(moco_job::Scope::System, None, pid)
        .expect("adopt");
    assert!(alive(pid));

    reg.kill(&id, &Caller::Console).expect("kill");
    assert!(
        until(|| !alive(pid)),
        "an adopted job that cannot be stopped is a job nobody can stop"
    );
    let _ = outsider.wait();
}

/// A job that has already exited is not signalled. Its pid may belong to
/// something else by now, and signalling a **group** makes that mistake wide.
#[test]
#[allow(clippy::expect_used, reason = "a failure here is a broken harness")]
fn a_dead_jobs_pid_is_never_signalled() {
    let (reg, d) = registry("reuse");
    let id = reg
        .start(JobRequest::new(["true".to_string()], &d))
        .expect("start");
    reg.wait(&id).expect("wait");

    // Killing a finished job is a no-op that must not reach any pid.
    reg.kill(&id, &Caller::Console).expect("kill of a dead job");
    assert!(
        reg.status_of(&id).is_some_and(|s| s.is_terminal()),
        "its recorded outcome must not be rewritten by a late stop"
    );
}
