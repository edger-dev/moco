//! Three long-standing rough edges, each small and each a quiet lie.
//!
//! implements: registry-is-node-state-on-disk

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use moco_job::scope::{Caller, Scope};
use moco_job::{JobRegistry, MANIFEST_FILE};

static SEQ: AtomicU64 = AtomicU64::new(0);

#[allow(
    clippy::expect_used,
    reason = "test helper: a failure is a broken harness"
)]
fn dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!(
        "moco-wr-{}-{}-{name}",
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
fn workspace(name: &str, manifest: &str) -> PathBuf {
    let d = dir(name);
    std::fs::create_dir_all(d.join(".git")).expect("repo");
    std::fs::write(d.join(MANIFEST_FILE), manifest).expect("manifest");
    d
}

fn until(mut f: impl FnMut() -> bool) -> bool {
    for _ in 0..100 {
        if f() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    false
}

/// **Clearing tombstones must not move a service's port.**
///
/// Stickiness is derived from the records, so removing them made a routine
/// tidy-up silently reassign ports on the next start — invalidating every
/// bookmark and config someone had written down, for an action that sounds like
/// housekeeping.
///
/// Two declarations, because one proves nothing: with a single job the
/// allocator hands back the same low port whether stickiness survived or not.
/// Restarting the *second* one first is what tells them apart — it keeps its own
/// port only if the memory outlived the clear.
#[test]
#[allow(clippy::expect_used, reason = "a failure here is a broken harness")]
fn clearing_tombstones_keeps_each_declarations_port() {
    let ws = workspace(
        "sticky",
        r#"proc (
             {name web, argv (sleep 30), port @Auto},
             {name api, argv (sleep 30), port @Auto}
           )"#,
    );
    let d = dir("sticky-reg");
    let reg = JobRegistry::ungoverned()
        .expect("registry")
        .with_dir(&d)
        .expect("with_dir");
    let caller = Caller::Scoped(Scope::resolve(&ws));

    let web = reg.start_named("web", &caller).expect("start web");
    let api = reg.start_named("api", &caller).expect("start api");
    let web_port = reg.port_of(&web).expect("web port");
    let api_port = reg.port_of(&api).expect("api port");
    assert_ne!(web_port, api_port, "two live jobs cannot share a port");

    for id in [&web, &api] {
        reg.kill(id, &Caller::Console).expect("kill");
        // `status_of` reports what was last observed; a read is what settles it.
        assert!(until(|| reg
            .tail(id, u64::MAX)
            .is_ok_and(|t| t.status.is_terminal())));
    }

    assert_eq!(reg.clear(&caller).expect("clear"), 2);

    // `api` first, so the lowest free port is `web`'s. Without stickiness it
    // would take it.
    let api_again = reg.start_named("api", &caller).expect("restart api");
    assert_eq!(
        reg.port_of(&api_again),
        Some(api_port),
        "a clear must not move a declaration onto someone else's port"
    );

    let web_again = reg.start_named("web", &caller).expect("restart web");
    assert_eq!(reg.port_of(&web_again), Some(web_port));

    let _ = reg.kill(&api_again, &Caller::Console);
    let _ = reg.kill(&web_again, &Caller::Console);
}

/// An adopted job says where it actually runs.
///
/// `/` was a placeholder standing in for "we did not look", and it is
/// indistinguishable from a job that genuinely runs at the root — so the one
/// field that tells you where a mystery process lives read as a plausible lie.
#[test]
#[allow(clippy::expect_used, reason = "a failure here is a broken harness")]
fn an_adopted_job_reports_the_directory_it_actually_runs_in() {
    let home = dir("adopt-cwd");
    let mut outsider = std::process::Command::new("sleep")
        .arg("300")
        .current_dir(&home)
        .spawn()
        .expect("spawn");
    let pid = outsider.id();

    let reg = JobRegistry::ungoverned().expect("registry");
    let id = reg.adopt(Scope::System, None, pid).expect("adopt");

    assert_eq!(
        reg.cwd_of(&id).as_deref(),
        Some(home.as_path()),
        "the cwd is readable from /proc, so there is no reason to guess"
    );

    let _ = reg.kill(&id, &Caller::Console);
    let _ = outsider.wait();
}

/// **The restart count is a count of restarts.**
///
/// Both the superseded entry and its replacement were assigned the new total,
/// and neither was written down at the time — so the number was only as good as
/// whatever happened to persist next, and a counter that cannot be trusted is
/// no basis for noticing a crash loop, which is the only reason to keep one.
#[test]
#[allow(clippy::expect_used, reason = "a failure here is a broken harness")]
fn a_restart_count_counts_restarts_and_survives_the_daemon() {
    let ws = workspace(
        "counting",
        r#"proc ({name flaky, argv (sh -c "exit 1"), lifetime @Service, restart @Always})"#,
    );
    let d = dir("counting-reg");
    let caller = Caller::Scoped(Scope::resolve(&ws));
    let scope = Scope::resolve(&ws);

    let reg = JobRegistry::ungoverned()
        .expect("registry")
        .with_dir(&d)
        .expect("with_dir");
    reg.start_named("flaky", &caller).expect("start");

    // Three restarts, driven deliberately rather than raced for.
    for expected in 1..=3u64 {
        assert!(
            until(|| !reg.supervise().is_empty()),
            "the supervisor should have restarted the crashing service"
        );
        let (id, _) = reg.declared(&scope, "flaky").expect("declared");
        assert_eq!(
            reg.restarts_of(&id),
            expected,
            "after {expected} restart(s) the count must be {expected}"
        );
    }

    let (id, _) = reg.declared(&scope, "flaky").expect("declared");
    let counted = reg.restarts_of(&id);

    // A new daemon over the same directory: everything comes back from disk.
    let reborn = JobRegistry::ungoverned()
        .expect("registry")
        .with_dir(&d)
        .expect("with_dir");
    let (id, _) = reborn.declared(&scope, "flaky").expect("declared");
    assert_eq!(
        reborn.restarts_of(&id),
        counted,
        "a counter that forgets across a daemon restart cannot notice a crash loop"
    );
}
