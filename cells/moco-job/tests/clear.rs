//! Tombstones linger; clearing them is explicit.
//!
//! implements: reads-global-writes-own-workspace

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use moco_job::scope::{Caller, Scope};
use moco_job::{JobRegistry, JobRequest};

static SEQ: AtomicU64 = AtomicU64::new(0);

#[allow(
    clippy::expect_used,
    reason = "test helper: a failure is a broken harness"
)]
fn repo(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "moco-clear-{}-{}-{name}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join(".git")).expect("repo");
    dir.canonicalize().expect("canonicalize")
}

#[allow(
    clippy::expect_used,
    reason = "test helper: a failure is a broken harness"
)]
fn registry() -> JobRegistry {
    JobRegistry::ungoverned().expect("registry")
}

/// A finished job **stays** until someone clears it: you have to be able to see
/// *that* it died and read its last output.
#[test]
fn a_finished_job_lingers_until_cleared() {
    let ws = repo("linger");
    let reg = registry();
    let caller = Caller::Scoped(Scope::resolve(&ws));

    let id = reg
        .start(JobRequest::new(["echo", "bye"], &ws).in_scope(Scope::resolve(&ws)))
        .expect("start");
    reg.wait(&id).expect("wait");

    assert!(
        reg.list().iter().any(|(j, _)| *j == id),
        "a terminal job is history, not litter"
    );
    assert_eq!(reg.clear(&caller).expect("clear"), 1);
    assert!(!reg.list().iter().any(|(j, _)| *j == id));

    let _ = std::fs::remove_dir_all(&ws);
}

/// **Clear never signals anything.** A running job is left strictly alone —
/// tombstone cleanup and kill-all are different verbs, and conflating them is
/// how someone loses a dev server by tidying up.
#[test]
fn clear_leaves_a_running_job_untouched() {
    let ws = repo("running");
    let reg = registry();
    let caller = Caller::Scoped(Scope::resolve(&ws));

    let id = reg
        .start(JobRequest::new(["sleep", "30"], &ws).in_scope(Scope::resolve(&ws)))
        .expect("start");

    assert_eq!(
        reg.clear(&caller).expect("clear"),
        0,
        "nothing was terminal"
    );
    assert!(
        reg.list().iter().any(|(j, _)| *j == id),
        "a live job must survive a clear"
    );
    assert_eq!(reg.status_of(&id), Some(moco_job::JobStatus::Running));

    reg.kill(&id, &caller).expect("stop it ourselves");
    let _ = std::fs::remove_dir_all(&ws);
}

/// A session clears **its own** workspace, and leaves another's history alone.
#[test]
fn a_session_clears_only_its_own_workspace() {
    let mine = repo("mine");
    let theirs = repo("theirs");
    let reg = registry();

    for ws in [&mine, &theirs] {
        let id = reg
            .start(JobRequest::new(["echo", "x"], ws).in_scope(Scope::resolve(ws)))
            .expect("start");
        reg.wait(&id).expect("wait");
    }

    let cleared = reg
        .clear(&Caller::Scoped(Scope::resolve(&mine)))
        .expect("clear");
    assert_eq!(cleared, 1, "only this workspace's tombstone");
    assert_eq!(
        reg.list().len(),
        1,
        "the other workspace's history is not ours to tidy"
    );

    let _ = std::fs::remove_dir_all(&mine);
    let _ = std::fs::remove_dir_all(&theirs);
}

/// The console clears globally — the same carve-out that lets it write anywhere.
#[test]
fn the_console_clears_every_workspace() {
    let a = repo("console-a");
    let b = repo("console-b");
    let reg = registry();

    for ws in [&a, &b] {
        let id = reg
            .start(JobRequest::new(["echo", "x"], ws).in_scope(Scope::resolve(ws)))
            .expect("start");
        reg.wait(&id).expect("wait");
    }

    assert_eq!(reg.clear(&Caller::Console).expect("clear"), 2);
    assert!(reg.list().is_empty());

    let _ = std::fs::remove_dir_all(&a);
    let _ = std::fs::remove_dir_all(&b);
}
