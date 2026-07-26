//! Two orthogonal fields, and what an exit *means*.
//!
//! implements: job-lifetime-oneshot-or-service
//! implements: autostart-and-restart-are-orthogonal
//! implements: manifest-declares-node-authorizes

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use moco_job::lifecycle::{Autostart, Lifetime, RestartPolicy};
use moco_job::manifest::MANIFEST_FILE;
use moco_job::scope::{Caller, Scope};
use moco_job::{JobRegistry, JobStatus, NodePolicy, RuleSet, SeedConfig};

static SEQ: AtomicU64 = AtomicU64::new(0);

#[allow(
    clippy::expect_used,
    reason = "test helper: a failure is a broken harness"
)]
fn workspace(name: &str, manifest: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "moco-lifecycle-{}-{}-{name}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join(".git")).expect("create repo");
    std::fs::write(dir.join(MANIFEST_FILE), manifest).expect("write manifest");
    dir.canonicalize().expect("canonicalize")
}

fn argv(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|s| (*s).to_string()).collect()
}

#[allow(
    clippy::expect_used,
    reason = "test helper: a failure is a broken harness"
)]
fn ungoverned() -> JobRegistry {
    JobRegistry::ungoverned().expect("registry")
}

/// Wait until `f` holds, driving `supervise` as a daemon would.
fn settle(reg: &JobRegistry, mut f: impl FnMut() -> bool) -> bool {
    for _ in 0..200 {
        reg.supervise();
        if f() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    f()
}

/// **The same exit means different things.** For a one-shot, exit 0 is success.
/// For a service, an exit nobody asked for is a failure *whatever the code* — a
/// dev server that exits cleanly is still down.
#[test]
fn a_services_clean_exit_is_still_a_failure() {
    assert!(
        Lifetime::OneShot.succeeded(&JobStatus::Done { code: 0 }),
        "a one-shot that exits 0 did what it was asked"
    );
    assert!(
        !Lifetime::Service.succeeded(&JobStatus::Done { code: 0 }),
        "a service that exits at all has stopped serving, exit code notwithstanding"
    );
    assert!(!Lifetime::OneShot.succeeded(&JobStatus::Done { code: 1 }));
    assert!(!Lifetime::Service.succeeded(&JobStatus::Done { code: 1 }));
}

/// The two fields are independent, and every pairing is expressible — including
/// "start at boot and never restart", which a single `restart = always` flag
/// cannot say.
#[test]
fn autostart_and_restart_compose_freely() {
    let daemon = (Autostart::Boot, RestartPolicy::Always);
    let checker = (Autostart::Session, RestartPolicy::OnFailure);
    let once = (Autostart::Boot, RestartPolicy::Never);

    assert_ne!(daemon.0, checker.0);
    assert_ne!(daemon.1, once.1);
    assert_eq!(
        once.0,
        Autostart::Boot,
        "boot-but-never-restart must be sayable"
    );
}

/// `restart = never`: an exited service stays exited.
#[test]
fn a_service_with_restart_never_is_not_respawned() {
    let ws = workspace(
        "never",
        r#"proc ({name once, argv ("true"), cwd "", deadline_ms 0, lifetime @Service, restart @Never, autostart @Manual})"#,
    );
    let reg = ungoverned();
    let caller = Caller::Scoped(Scope::resolve(&ws));

    let id = reg.start_named("once", &caller).expect("start");
    assert!(settle(&reg, || {
        reg.status_of(&id).is_some_and(|s| s.is_terminal())
    }));

    let before = reg.restarts_of(&id);
    reg.supervise();
    assert_eq!(reg.restarts_of(&id), before, "never means never");

    let _ = std::fs::remove_dir_all(&ws);
}

/// `restart = on-failure`: a service that falls over comes back.
#[test]
fn a_failed_service_is_respawned_on_failure() {
    let ws = workspace(
        "onfail",
        r#"proc ({name flaky, argv ("false"), cwd "", deadline_ms 0, lifetime @Service, restart @OnFailure, autostart @Manual})"#,
    );
    let reg = ungoverned();
    let caller = Caller::Scoped(Scope::resolve(&ws));

    let scope = Scope::resolve(&ws);
    reg.start_named("flaky", &caller).expect("start");

    // A restart mints a new id, so the count is read against the *declaration*:
    // `workspace:name` is a service's durable identity, not any one instance.
    assert!(
        settle(&reg, || reg
            .declared(&scope, "flaky")
            .is_some_and(|(_, n)| n >= 2)),
        "a failing service should be brought back, saw {:?}",
        reg.declared(&scope, "flaky")
    );

    let _ = std::fs::remove_dir_all(&ws);
}

/// **A requested stop is not a failure.** Asking a service to stop and having
/// the supervisor immediately restart it would make stopping impossible.
#[test]
fn an_explicitly_stopped_service_is_not_respawned() {
    let ws = workspace(
        "stopped",
        r#"proc ({name server, argv (sleep 30), cwd "", deadline_ms 0, lifetime @Service, restart @Always, autostart @Manual})"#,
    );
    let reg = ungoverned();
    let caller = Caller::Scoped(Scope::resolve(&ws));

    let id = reg.start_named("server", &caller).expect("start");
    reg.kill(&id, &caller).expect("stop it");
    assert!(settle(&reg, || {
        reg.status_of(&id) == Some(JobStatus::Killed)
    }));

    let after_stop = reg.restarts_of(&id);
    for _ in 0..5 {
        reg.supervise();
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(
        reg.restarts_of(&id),
        after_stop,
        "a job someone asked to stop must stay stopped"
    );

    let _ = std::fs::remove_dir_all(&ws);
}

/// A **one-shot** is never respawned, whatever it exits with. Restart policy is
/// a service's field; a one-shot that ran is finished.
#[test]
fn a_one_shot_is_never_respawned() {
    let ws = workspace(
        "oneshot",
        r#"proc ({name task, argv ("false"), cwd "", deadline_ms 0, lifetime @OneShot, restart @Always, autostart @Manual})"#,
    );
    let reg = ungoverned();
    let caller = Caller::Scoped(Scope::resolve(&ws));

    let id = reg.start_named("task", &caller).expect("start");
    assert!(settle(&reg, || {
        reg.status_of(&id).is_some_and(|s| s.is_terminal())
    }));
    for _ in 0..5 {
        reg.supervise();
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(reg.restarts_of(&id), 0, "a one-shot has run; it is done");

    let _ = std::fs::remove_dir_all(&ws);
}

/// **Restart re-reads the manifest.** Editing the file and restarting must run
/// the *new* declaration — the prototype's version respawned a spec cached at
/// first start, so a config change silently reverted itself.
#[test]
fn restart_re_reads_the_manifest() {
    let ws = workspace(
        "reread",
        r#"proc ({name thing, argv (echo before), cwd "", deadline_ms 0, lifetime @OneShot, restart @Never, autostart @Manual})"#,
    );
    let reg = ungoverned();
    let caller = Caller::Scoped(Scope::resolve(&ws));

    let id = reg.start_named("thing", &caller).expect("start");
    std::fs::write(
        ws.join(MANIFEST_FILE),
        r#"proc ({name thing, argv (echo after), cwd "", deadline_ms 0, lifetime @OneShot, restart @Never, autostart @Manual})"#,
    )
    .expect("edit");

    let restarted = reg.restart(&id, &caller).expect("restart");
    assert_ne!(
        restarted, id,
        "a restart is a new instance of the declaration"
    );
    assert_eq!(
        reg.argv_of(&restarted).as_deref(),
        Some(&argv(&["echo", "after"])[..]),
        "restart must run the edited declaration, not a cached one"
    );

    let _ = std::fs::remove_dir_all(&ws);
}

/// **Restart re-gates.** Re-reading without re-authorizing would let an edit to
/// a file the agent controls run a never-approved argv: edit the manifest, hit
/// restart, and the node never gets a say.
#[test]
fn restart_re_gates_the_new_declaration() {
    let ws = workspace(
        "regate",
        r#"proc ({name thing, argv (echo ok), cwd "", deadline_ms 0, lifetime @OneShot, restart @Never, autostart @Manual})"#,
    );
    // The node permits exactly one argv.
    let rules = RuleSet::from_seed(SeedConfig {
        allow: vec![argv(&["echo", "ok"])],
        deny: vec![],
    });
    let reg = JobRegistry::with_policy(
        NodePolicy::new(rules, std::env::temp_dir())
            .with_approval_timeout(Duration::from_millis(50)),
    )
    .expect("registry");
    let caller = Caller::Scoped(Scope::resolve(&ws));

    let id = reg.start_named("thing", &caller).expect("start");
    reg.wait(&id).expect("the permitted argv runs");

    // Now the declaration changes to something no rule permits.
    std::fs::write(
        ws.join(MANIFEST_FILE),
        r#"proc ({name thing, argv (echo smuggled), cwd "", deadline_ms 0, lifetime @OneShot, restart @Never, autostart @Manual})"#,
    )
    .expect("edit");

    let restarted = reg.restart(&id, &caller).expect("restart yields a job");
    let outcome = reg.wait(&restarted).expect("wait");
    assert!(
        matches!(outcome.status, JobStatus::Denied { .. }),
        "a restart must re-authorize, not inherit the old admission; got {:?}",
        outcome.status
    );

    let _ = std::fs::remove_dir_all(&ws);
}

/// Restart is a write, so a foreign workspace cannot do it either.
#[test]
fn a_foreign_workspace_cannot_restart() {
    let ws = workspace(
        "foreign",
        r#"proc ({name thing, argv (echo hi), cwd "", deadline_ms 0, lifetime @OneShot, restart @Never, autostart @Manual})"#,
    );
    let other = workspace(
        "other",
        r#"proc ({name thing, argv (echo hi), cwd "", deadline_ms 0, lifetime @OneShot, restart @Never, autostart @Manual})"#,
    );
    let reg = ungoverned();

    let id = reg
        .start_named("thing", &Caller::Scoped(Scope::resolve(&ws)))
        .expect("start");
    let err = reg
        .restart(&id, &Caller::Scoped(Scope::resolve(&other)))
        .expect_err("a foreign workspace must be refused");
    assert!(err.to_string().contains("owned by workspace"), "got: {err}");

    let _ = std::fs::remove_dir_all(&ws);
    let _ = std::fs::remove_dir_all(&other);
}

/// An ad-hoc job has no declaration to re-read, so restarting one is refused
/// rather than silently respawning a cached spec.
#[test]
fn an_ad_hoc_job_cannot_be_restarted() {
    let reg = ungoverned();
    let dir = std::env::temp_dir();
    let caller = Caller::Scoped(Scope::resolve(&dir));

    let id = reg
        .start(moco_job::JobRequest::new(["true"], &dir).in_scope(Scope::resolve(&dir)))
        .expect("start");
    let err = reg
        .restart(&id, &caller)
        .expect_err("an ad-hoc job has nothing to re-read");
    assert!(
        err.to_string().contains("ad-hoc"),
        "the refusal must explain why, got: {err}"
    );
}
