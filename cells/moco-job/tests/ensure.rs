//! Session autostart: the agent's half of the two triggers.
//!
//! implements: autostart-and-restart-are-orthogonal

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use moco_job::JobRegistry;
use moco_job::scope::{Caller, Scope};

static SEQ: AtomicU64 = AtomicU64::new(0);

#[allow(
    clippy::expect_used,
    reason = "test helper: a failure is a broken harness"
)]
fn workspace(name: &str, manifest: &str, linked: bool) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "moco-ensure-{}-{}-{name}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create");
    if linked {
        std::fs::write(dir.join(".git"), "gitdir: /elsewhere\n").expect(".git file");
    } else {
        std::fs::create_dir_all(dir.join(".git")).expect(".git dir");
    }
    std::fs::write(dir.join(moco_job::MANIFEST_FILE), manifest).expect("manifest");
    dir.canonicalize().expect("canonicalize")
}

#[allow(
    clippy::expect_used,
    reason = "test helper: a failure is a broken harness"
)]
fn node(name: &str) -> JobRegistry {
    JobRegistry::ungoverned().expect("registry").with_node(name)
}

/// Only `session` entries are started — `manual` waits to be asked, and `boot`
/// is the daemon's business, not a session's.
#[test]
fn ensure_starts_only_session_entries() {
    let ws = workspace(
        "select",
        r#"proc (
  {name a, argv (sleep 30), autostart @Session}
  {name b, argv (sleep 30), autostart @Manual}
  {name c, argv (sleep 30), autostart @Boot}
)"#,
        false,
    );
    let reg = node("alpha");
    let caller = Caller::Scoped(Scope::resolve(&ws));

    let started = reg.ensure(&caller).expect("ensure");
    assert_eq!(
        started.len(),
        1,
        "exactly the session entry, got {started:?}"
    );

    let scope = Scope::resolve(&ws);
    assert!(reg.declared(&scope, "a").is_some());
    assert!(
        reg.declared(&scope, "b").is_none(),
        "manual waits to be asked"
    );
    assert!(
        reg.declared(&scope, "c").is_none(),
        "boot is the daemon's job"
    );

    let _ = reg.clear(&Caller::Console);
    let _ = std::fs::remove_dir_all(&ws);
}

/// **Idempotent.** Running it twice starts nothing the second time, and stops or
/// restarts nothing — which is what makes it always safe to re-run.
#[test]
fn ensure_is_idempotent_and_never_stops_anything() {
    let ws = workspace(
        "idempotent",
        r#"proc ({name svc, argv (sleep 30), autostart @Session})"#,
        false,
    );
    let reg = node("alpha");
    let caller = Caller::Scoped(Scope::resolve(&ws));
    let scope = Scope::resolve(&ws);

    let first = reg.ensure(&caller).expect("first");
    assert_eq!(first.len(), 1);
    let (running, _) = reg.declared(&scope, "svc").expect("running");

    let second = reg.ensure(&caller).expect("second");
    assert!(second.is_empty(), "already running, so nothing to do");
    assert_eq!(
        reg.declared(&scope, "svc").map(|(id, _)| id),
        Some(running.clone()),
        "the running instance must be left exactly alone"
    );
    assert_eq!(reg.status_of(&running), Some(moco_job::JobStatus::Running));

    let _ = reg.kill(&running, &Caller::Console);
    let _ = reg.clear(&Caller::Console);
    let _ = std::fs::remove_dir_all(&ws);
}

/// **A refused entry is skipped silently.** Ensure runs unprompted at session
/// start, so a job that is not meant for this machine or this worktree is not a
/// problem to report — it is simply not this session's business. An *explicit*
/// start still says why.
#[test]
fn ensure_skips_a_refused_entry_without_complaining() {
    let ws = workspace(
        "refused",
        r#"proc (
  {name here, argv (sleep 30), autostart @Session}
  {name elsewhere, argv (sleep 30), autostart @Session, hosts (some-other-box)}
)"#,
        false,
    );
    let reg = node("alpha");
    let caller = Caller::Scoped(Scope::resolve(&ws));

    let started = reg
        .ensure(&caller)
        .expect("ensure must not fail on a refusal");
    assert_eq!(started.len(), 1, "the admissible one, and only it");

    // And the refusal is still available to anyone who asks directly.
    let err = reg.start_named("elsewhere", &caller).expect_err("explicit");
    assert!(err.to_string().contains("some-other-box"), "got: {err}");

    let _ = reg.clear(&Caller::Console);
    let _ = std::fs::remove_dir_all(&ws);
}

/// The console has no workspace, so it has no manifest to ensure from.
#[test]
fn the_console_cannot_ensure() {
    let reg = node("alpha");
    assert!(
        reg.ensure(&Caller::Console).is_err(),
        "a session's manifest needs a session"
    );
}
