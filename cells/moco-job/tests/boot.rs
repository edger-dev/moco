//! Boot autostart: what the node itself runs, declared where the node can see it.
//!
//! implements: boot-autostart-reads-the-node-manifest

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use moco_job::scope::{Caller, Scope};
use moco_job::{JobRegistry, MANIFEST_FILE};

static SEQ: AtomicU64 = AtomicU64::new(0);

#[allow(
    clippy::expect_used,
    reason = "test helper: a failure is a broken harness"
)]
fn dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!(
        "moco-boot-{}-{}-{name}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).expect("create");
    d.canonicalize().expect("canonicalize")
}

/// A registry whose node manifest is `manifest`.
#[allow(
    clippy::expect_used,
    reason = "test helper: a failure is a broken harness"
)]
fn node(name: &str, manifest: &str) -> (JobRegistry, PathBuf) {
    let d = dir(name);
    std::fs::write(d.join(MANIFEST_FILE), manifest).expect("node manifest");
    let reg = JobRegistry::ungoverned()
        .expect("registry")
        .with_dir(&d)
        .expect("with_dir");
    (reg, d)
}

/// A workspace, for the cases where the two manifests must not be confused.
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

/// The daemon starts what the node declares, and those jobs belong to the
/// **node** rather than to any workspace.
#[test]
#[allow(clippy::expect_used, reason = "a failure here is a broken harness")]
fn boot_starts_node_declared_jobs_owned_by_the_system() {
    let home = dir("starts-home");
    let (reg, _d) = node(
        "starts",
        &format!(
            r#"proc ({{name housekeeping, argv (sleep 30), cwd "{}", autostart @Boot}})"#,
            home.display()
        ),
    );

    let started = reg.boot().expect("boot");
    assert_eq!(started.len(), 1, "one boot entry, one start");

    let id = &started[0];
    assert_eq!(
        reg.scope_of(id),
        Some(Scope::System),
        "a node-declared job is owned by the node, not by a workspace"
    );
    assert_eq!(reg.name_of(id).as_deref(), Some("housekeeping"));

    let _ = reg.kill(id, &Caller::Console);
}

/// Only `boot` entries start. A node manifest may hold manual entries too, and
/// starting those would be the daemon doing something nobody asked for.
#[test]
#[allow(clippy::expect_used, reason = "a failure here is a broken harness")]
fn only_boot_entries_start_at_boot() {
    let d = dir("only-boot");
    std::fs::write(
        d.join(MANIFEST_FILE),
        format!(
            r#"proc (
                 {{name auto, argv (sleep 30), cwd "{0}", autostart @Boot}},
                 {{name manual, argv (sleep 30), cwd "{0}"}}
               )"#,
            d.display()
        ),
    )
    .expect("manifest");
    let reg = JobRegistry::ungoverned()
        .expect("registry")
        .with_dir(&d)
        .expect("with_dir");

    let started = reg.boot().expect("boot");
    assert_eq!(started.len(), 1);
    assert_eq!(reg.name_of(&started[0]).as_deref(), Some("auto"));

    let _ = reg.kill(&started[0], &Caller::Console);
}

/// **Idempotent, because the daemon restarts.** Re-adoption runs first at
/// startup, so a job that is already running must be left alone rather than
/// started a second time alongside itself.
#[test]
#[allow(clippy::expect_used, reason = "a failure here is a broken harness")]
fn boot_leaves_an_already_running_job_alone() {
    let d = dir("idempotent");
    std::fs::write(
        d.join(MANIFEST_FILE),
        format!(
            r#"proc ({{name once, argv (sleep 30), cwd "{}", autostart @Boot}})"#,
            d.display()
        ),
    )
    .expect("manifest");
    let reg = JobRegistry::ungoverned()
        .expect("registry")
        .with_dir(&d)
        .expect("with_dir");

    let first = reg.boot().expect("boot");
    assert_eq!(first.len(), 1);

    let again = reg.boot().expect("second boot");
    assert!(
        again.is_empty(),
        "a running job must not be started twice — this is `ensure`, not `start`"
    );

    let _ = reg.kill(&first[0], &Caller::Console);
}

/// An absent node manifest is not an error. A node that declares nothing runs
/// nothing, which is a complete answer.
#[test]
#[allow(clippy::expect_used, reason = "a failure here is a broken harness")]
fn a_node_with_no_manifest_boots_nothing_without_complaint() {
    let d = dir("absent");
    let reg = JobRegistry::ungoverned()
        .expect("registry")
        .with_dir(&d)
        .expect("with_dir");
    assert!(reg.boot().expect("boot is not an error").is_empty());
}

/// **`boot` in a workspace manifest is refused, loudly.**
///
/// The daemon has no way to discover an arbitrary workspace at startup, so such
/// an entry would simply never start. Accepting it silently is the worst
/// outcome: a declaration that reads as active and is inert, costing a
/// debugging session to notice. The error says where it belongs instead.
#[test]
#[allow(clippy::expect_used, reason = "a failure here is a broken harness")]
fn boot_in_a_workspace_manifest_is_refused_and_says_where_it_belongs() {
    let ws = workspace(
        "misplaced",
        r#"proc ({name web, argv (sleep 30), autostart @Boot})"#,
    );
    let reg = JobRegistry::ungoverned().expect("registry");

    let err = reg
        .start_named("web", &Caller::Scoped(Scope::resolve(&ws)))
        .expect_err("a workspace may not declare a boot job");
    let text = err.to_string();
    assert!(text.contains("boot"), "got: {text}");
    assert!(
        text.contains("node") || text.contains("session"),
        "the message must point at the fix, got: {text}"
    );
}

/// A node-level job must say where it runs. There is no workspace root to fall
/// back on, and an ambient cwd for a system service is a bug waiting to be
/// blamed on something else.
#[test]
#[allow(clippy::expect_used, reason = "a failure here is a broken harness")]
fn a_node_job_must_declare_an_absolute_cwd() {
    let d = dir("no-cwd");
    std::fs::write(
        d.join(MANIFEST_FILE),
        r#"proc ({name rootless, argv (sleep 30), autostart @Boot})"#,
    )
    .expect("manifest");
    let reg = JobRegistry::ungoverned()
        .expect("registry")
        .with_dir(&d)
        .expect("with_dir");

    let err = reg.boot().expect_err("an unstated cwd must be refused");
    let text = err.to_string();
    assert!(text.contains("rootless"), "names the entry, got: {text}");
    assert!(text.contains("cwd"), "names the field, got: {text}");
}

/// A workspace session must not be able to stop what the node runs.
#[test]
#[allow(clippy::expect_used, reason = "a failure here is a broken harness")]
fn a_workspace_may_not_kill_a_node_job() {
    let d = dir("authority");
    std::fs::write(
        d.join(MANIFEST_FILE),
        format!(
            r#"proc ({{name guarded, argv (sleep 30), cwd "{}", autostart @Boot}})"#,
            d.display()
        ),
    )
    .expect("manifest");
    let reg = JobRegistry::ungoverned()
        .expect("registry")
        .with_dir(&d)
        .expect("with_dir");

    let started = reg.boot().expect("boot");
    let id = &started[0];

    let ws = workspace("intruder", r#"proc ()"#);
    let err = reg
        .kill(id, &Caller::Scoped(Scope::resolve(&ws)))
        .expect_err("a workspace has no authority over a node job");
    assert!(err.to_string().contains("guarded") || !err.to_string().is_empty());

    let _ = reg.kill(id, &Caller::Console);
}
