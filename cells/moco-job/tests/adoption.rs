//! Handing an already-running process to the supervisor.
//!
//! implements: adopt-is-readopt-parameterized

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use moco_job::scope::{Caller, Scope};
use moco_job::{JobRegistry, JobStatus};

static SEQ: AtomicU64 = AtomicU64::new(0);

#[allow(
    clippy::expect_used,
    reason = "test helper: a failure is a broken harness"
)]
fn dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!(
        "moco-adopt-{}-{}-{name}",
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
fn registry(d: &PathBuf) -> JobRegistry {
    JobRegistry::ungoverned()
        .expect("registry")
        .with_dir(d)
        .expect("with_dir")
}

/// Spawn something this registry does **not** know about.
#[allow(
    clippy::expect_used,
    reason = "test helper: a failure is a broken harness"
)]
fn stranger() -> std::process::Child {
    std::process::Command::new("sleep")
        .arg("30")
        .spawn()
        .expect("spawn a stranger")
}

/// A process the supervisor never started can be handed to it, and shows up
/// like anything else.
#[test]
fn an_outside_process_can_be_adopted_and_listed() {
    let d = dir("basic");
    let reg = registry(&d);
    let mut child = stranger();

    let id = reg
        .adopt(Scope::System, None, child.id())
        .expect("adopt the running process");

    assert_eq!(reg.status_of(&id), Some(JobStatus::Running));
    assert!(reg.list().iter().any(|(j, _)| *j == id));
    assert!(
        reg.is_external(&id),
        "it is handed over, not ours to have started"
    );

    let _ = child.kill();
    let _ = child.wait();
    let _ = std::fs::remove_dir_all(&d);
}

/// **Observe-only never respawns.** With no command there is nothing to respawn
/// from — which is what keeps the supervisor from fighting a human restarting
/// that process by hand.
#[test]
fn an_observe_only_adoption_is_never_respawned() {
    let d = dir("observe");
    let reg = registry(&d);
    let mut child = stranger();

    let id = reg.adopt(Scope::System, None, child.id()).expect("adopt");

    let _ = child.kill();
    let _ = child.wait();

    // Let the supervisor notice it is gone.
    let mut settled = false;
    for _ in 0..200 {
        reg.supervise();
        if reg.status_of(&id).is_some_and(|s| s.is_terminal()) {
            settled = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(settled, "an adopted process's death must be noticed");
    assert_eq!(
        reg.restarts_of(&id),
        0,
        "nothing was declared, so there is nothing to bring back"
    );

    let _ = std::fs::remove_dir_all(&d);
}

/// It settles as `outcome-unknown`: we were never its parent, so there is no
/// exit code to collect, and inventing one would be worse than saying so.
#[test]
fn an_adopted_process_that_ends_reports_outcome_unknown() {
    let d = dir("unknown");
    let reg = registry(&d);
    let mut child = stranger();

    let id = reg.adopt(Scope::System, None, child.id()).expect("adopt");
    let _ = child.kill();
    let _ = child.wait();

    for _ in 0..200 {
        reg.supervise();
        if reg.status_of(&id).is_some_and(|s| s.is_terminal()) {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(reg.status_of(&id), Some(JobStatus::OutcomeUnknown));

    let _ = std::fs::remove_dir_all(&d);
}

/// Adopting a pid that is not running is refused, rather than creating an entry
/// for something that was never there.
#[test]
fn adopting_a_dead_pid_is_refused() {
    let d = dir("dead");
    let reg = registry(&d);
    let mut child = stranger();
    let pid = child.id();
    let _ = child.kill();
    let _ = child.wait();

    let err = reg
        .adopt(Scope::System, None, pid)
        .expect_err("there is nothing there to adopt");
    assert!(
        err.to_string().contains(&pid.to_string()),
        "the refusal must name the pid: {err}"
    );

    let _ = std::fs::remove_dir_all(&d);
}

/// Ownership works the same for an adopted job: it is a write like any other.
#[test]
fn an_adopted_job_obeys_the_write_scope() {
    let d = dir("scope");
    let reg = registry(&d);
    let mut child = stranger();

    let owner = Scope::workspace("/ws/owner");
    let id = reg.adopt(owner.clone(), None, child.id()).expect("adopt");

    let err = reg
        .kill(&id, &Caller::Scoped(Scope::workspace("/ws/other")))
        .expect_err("a foreign workspace may not stop it");
    assert!(err.to_string().contains("owned by workspace"), "got: {err}");

    reg.kill(&id, &Caller::Scoped(owner))
        .expect("its owner may");

    let _ = child.kill();
    let _ = child.wait();
    let _ = std::fs::remove_dir_all(&d);
}

/// Adoption survives a restart like anything else, and stays marked external.
#[test]
fn an_adoption_survives_a_registry_restart() {
    let d = dir("durable");
    let mut child = stranger();
    let id = {
        let reg = registry(&d);
        reg.adopt(Scope::System, None, child.id()).expect("adopt")
    };

    let reopened = registry(&d);
    assert_eq!(reopened.status_of(&id), Some(JobStatus::Running));
    assert!(
        reopened.is_external(&id),
        "the external flag is persisted, so re-adoption preserves it"
    );

    let _ = child.kill();
    let _ = child.wait();
    let _ = std::fs::remove_dir_all(&d);
}
