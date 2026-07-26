//! Declared jobs: the manifest names them, the node still authorizes them.
//!
//! implements: manifest-declares-node-authorizes
//! implements: config-failure-never-degrades-to-empty
//! implements: workspace-is-the-owner-not-session

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use moco_job::manifest::{MANIFEST_FILE, Manifest};
use moco_job::scope::{Caller, Scope};
use moco_job::{JobRegistry, NodePolicy, RuleSet, SeedConfig};

static SEQ: AtomicU64 = AtomicU64::new(0);

#[allow(
    clippy::expect_used,
    reason = "test helper: a failure is a broken harness"
)]
fn workspace(name: &str, manifest: Option<&str>) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "moco-manifest-{}-{}-{name}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join(".git")).expect("create repo");
    if let Some(text) = manifest {
        std::fs::write(dir.join(MANIFEST_FILE), text).expect("write manifest");
    }
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

/// A manifest that is simply **not there** declares nothing, and that is not an
/// error — nothing has been declared, which is a perfectly good answer.
#[test]
fn an_absent_manifest_declares_nothing() {
    let ws = workspace("absent", None);
    let manifest = Manifest::load(&ws).expect("an absent manifest is not an error");
    assert!(manifest.is_empty());
    let _ = std::fs::remove_dir_all(&ws);
}

/// **A broken manifest is not an empty one.** One bad field must not silently
/// void every declaration in the file — that reports a missing *entry* for a
/// *file-level* problem, and sends the reader after the wrong thing entirely.
#[test]
fn a_broken_manifest_is_an_error_naming_the_file() {
    let ws = workspace("broken", Some("this is not a styx document {{{"));
    let err = Manifest::load(&ws).expect_err("a parse failure must not be silent");
    let message = err.to_string();

    assert!(
        message.contains(MANIFEST_FILE),
        "the failure must name the file, got: {message}"
    );
    let _ = std::fs::remove_dir_all(&ws);
}

/// Two entries with one name is a config error, not last-one-wins. Silently
/// picking one would mean the file says something it does not do.
#[test]
fn a_duplicate_name_is_refused() {
    let ws = workspace(
        "dupe",
        Some(
            r#"proc ({name check, argv (echo one), cwd "", deadline_ms 0} {name check, argv (echo two), cwd "", deadline_ms 0})"#,
        ),
    );
    let err = Manifest::load(&ws).expect_err("a duplicate name must be refused");
    assert!(
        err.to_string().contains("check"),
        "the failure must name the duplicate, got: {err}"
    );
    let _ = std::fs::remove_dir_all(&ws);
}

/// A declared job starts by name, and is owned by the workspace that declared
/// it.
#[test]
fn a_declared_job_starts_by_name_and_belongs_to_its_workspace() {
    let ws = workspace(
        "named",
        Some(r#"proc ({name greet, argv (echo hello), cwd "", deadline_ms 0})"#),
    );
    let reg = ungoverned();

    let id = reg
        .start_named("greet", &Caller::Scoped(Scope::resolve(&ws)))
        .expect("a declared job should start");

    assert_eq!(reg.scope_of(&id), Some(Scope::workspace(&ws)));
    assert_eq!(
        reg.name_of(&id).as_deref(),
        Some("greet"),
        "a declared job carries the name it was declared under"
    );
    let _ = std::fs::remove_dir_all(&ws);
}

/// **Hot reload.** The manifest is re-read on every start, so editing it takes
/// effect without restarting anything — the alternative is a config change that
/// silently does not apply, which costs a debugging session to notice.
#[test]
fn the_manifest_is_re_read_on_every_start() {
    let ws = workspace(
        "reload",
        Some(r#"proc ({name thing, argv (echo before), cwd "", deadline_ms 0})"#),
    );
    let reg = ungoverned();

    let first = reg
        .start_named("thing", &Caller::Scoped(Scope::resolve(&ws)))
        .expect("start");
    assert_eq!(
        reg.argv_of(&first).as_deref(),
        Some(&argv(&["echo", "before"])[..])
    );

    std::fs::write(
        ws.join(MANIFEST_FILE),
        r#"proc ({name thing, argv (echo after), cwd "", deadline_ms 0})"#,
    )
    .expect("rewrite manifest");

    let second = reg
        .start_named("thing", &Caller::Scoped(Scope::resolve(&ws)))
        .expect("start again");
    assert_eq!(
        reg.argv_of(&second).as_deref(),
        Some(&argv(&["echo", "after"])[..]),
        "the edit must take effect without a restart"
    );
    let _ = std::fs::remove_dir_all(&ws);
}

/// An undeclared name is refused, and the refusal names **the manifest that was
/// actually read** — so nobody hunts for a typo in the name when the file they
/// edited was a different one.
#[test]
fn an_undeclared_name_names_the_manifest_it_looked_in() {
    let ws = workspace(
        "missing",
        Some(r#"proc ({name other, argv (echo hi), cwd "", deadline_ms 0})"#),
    );
    let reg = ungoverned();

    let err = reg
        .start_named("nope", &Caller::Scoped(Scope::resolve(&ws)))
        .expect_err("an undeclared name must be refused");
    let message = err.to_string();

    assert!(
        message.contains("nope"),
        "must name what was asked for: {message}"
    );
    assert!(
        message.contains(&ws.display().to_string()),
        "must name the manifest it read: {message}"
    );
    let _ = std::fs::remove_dir_all(&ws);
}

/// **The manifest declares; the node authorizes.** A declared job whose argv no
/// rule permits is denied exactly like an ad-hoc one. Being written down in a
/// file the agent can edit earns no authority.
#[test]
fn a_declared_job_still_passes_the_node_s_gate() {
    let ws = workspace(
        "gated",
        Some(r#"proc ({name sneaky, argv (echo unlisted), cwd "", deadline_ms 0})"#),
    );
    // The node permits one exact argv, and it is not the declared one.
    let rules = RuleSet::from_seed(SeedConfig {
        allow: vec![argv(&["echo", "ok"])],
        deny: vec![],
    });
    let reg = JobRegistry::with_policy(
        NodePolicy::new(rules, std::env::temp_dir())
            .with_approval_timeout(std::time::Duration::from_millis(50)),
    )
    .expect("registry");

    let id = reg
        .start_named("sneaky", &Caller::Scoped(Scope::resolve(&ws)))
        .expect("it becomes a job");

    // Unmatched by any rule: it parks pending and fails closed, exactly as an
    // ad-hoc argv would. It is emphatically not auto-allowed.
    let outcome = reg.wait(&id).expect("wait");
    assert!(
        matches!(outcome.status, moco_job::JobStatus::Denied { .. }),
        "a declared job must not bypass the gate, got {:?}",
        outcome.status
    );
    let _ = std::fs::remove_dir_all(&ws);
}

/// The console has no workspace, so it cannot start something *by name*: the
/// name only means anything relative to a workspace's manifest.
#[test]
fn the_console_cannot_start_by_name() {
    let ws = workspace(
        "console",
        Some(r#"proc ({name thing, argv (echo hi), cwd "", deadline_ms 0})"#),
    );
    let reg = ungoverned();

    let err = reg
        .start_named("thing", &Caller::Console)
        .expect_err("a name needs a workspace to resolve against");
    assert!(
        err.to_string().to_lowercase().contains("workspace"),
        "the refusal must explain why, got: {err}"
    );
    let _ = std::fs::remove_dir_all(&ws);
}

/// **Only `name` and `argv` are required.** A job that wants no automatic
/// behaviour of any kind says nothing about it, and gets the unsurprising
/// defaults — a one-shot, started manually, never restarted.
///
/// implements: autostart-and-restart-are-orthogonal
#[test]
fn everything_but_name_and_argv_defaults() {
    let ws = workspace(
        "defaults",
        Some(r#"proc ({name build, argv (cargo build)})"#),
    );

    let manifest = Manifest::load(&ws).expect("a minimal entry should load");
    let entry = manifest.get("build").expect("declared");

    assert_eq!(entry.argv, argv(&["cargo", "build"]));
    assert_eq!(entry.cwd, "", "the workspace root");
    assert_eq!(entry.deadline_ms, 0, "unbounded");
    assert_eq!(entry.lifetime, moco_job::Lifetime::OneShot);
    assert_eq!(entry.restart, moco_job::RestartPolicy::Never);
    assert_eq!(entry.autostart, moco_job::Autostart::Manual);

    let _ = std::fs::remove_dir_all(&ws);
}
