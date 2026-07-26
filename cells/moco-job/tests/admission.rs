//! Two start-time gates, with refusals that say which one refused.
//!
//! implements: admission-gates-worktree-and-host

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use moco_job::manifest::MANIFEST_FILE;
use moco_job::scope::{Caller, Scope};
use moco_job::{JobRegistry, JobRequest};

static SEQ: AtomicU64 = AtomicU64::new(0);

#[allow(
    clippy::expect_used,
    reason = "test helper: a failure is a broken harness"
)]
fn tree(name: &str, manifest: &str, linked: bool) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "moco-admission-{}-{}-{name}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create");
    if linked {
        // A linked worktree's `.git` is a **file** pointing at the main tree.
        std::fs::write(dir.join(".git"), "gitdir: /elsewhere/.git/worktrees/wt\n")
            .expect("write .git file");
    } else {
        std::fs::create_dir_all(dir.join(".git")).expect("create .git dir");
    }
    std::fs::write(dir.join(MANIFEST_FILE), manifest).expect("manifest");
    dir.canonicalize().expect("canonicalize")
}

#[allow(
    clippy::expect_used,
    reason = "test helper: a failure is a broken harness"
)]
fn node(name: &str) -> JobRegistry {
    JobRegistry::ungoverned().expect("registry").with_node(name)
}

fn caller(ws: &PathBuf) -> Caller {
    Caller::Scoped(Scope::resolve(ws))
}

const EACH: &str = r#"proc ({name job, argv (echo hi), worktree @Each})"#;
const MAIN_ONLY: &str = r#"proc ({name job, argv (echo hi), worktree @MainOnly})"#;

/// `each` is the default: every worktree runs its own instance.
#[test]
fn each_runs_in_a_linked_worktree() {
    let ws = tree("each-linked", EACH, true);
    let reg = node("alpha");
    reg.start_named("job", &caller(&ws))
        .expect("`each` runs anywhere");
    let _ = std::fs::remove_dir_all(&ws);
}

/// `main-only` runs in the main working tree.
#[test]
fn main_only_runs_in_the_main_tree() {
    let ws = tree("main-main", MAIN_ONLY, false);
    let reg = node("alpha");
    reg.start_named("job", &caller(&ws))
        .expect("the main tree is where main-only belongs");
    let _ = std::fs::remove_dir_all(&ws);
}

/// **`main-only` refuses a linked worktree**, and says so — rather than running
/// everywhere and letting the OS reject the second bind, which trades a clear
/// policy for a noisy race.
#[test]
fn main_only_refuses_a_linked_worktree_and_names_the_gate() {
    let ws = tree("main-linked", MAIN_ONLY, true);
    let reg = node("alpha");

    let err = reg
        .start_named("job", &caller(&ws))
        .expect_err("a linked worktree is not the main tree");
    let message = err.to_string();

    assert!(
        message.contains("main-only") || message.contains("worktree"),
        "the refusal must name the gate: {message}"
    );
    assert!(message.contains("job"), "and the job: {message}");
    let _ = std::fs::remove_dir_all(&ws);
}

/// A **fixed** port implies `main-only` without being told: two worktrees cannot
/// bind one port, so running in both is not a thing anyone meant.
#[test]
fn a_fixed_port_implies_main_only() {
    let ws = tree(
        "fixed-implies",
        r#"proc ({name job, argv (echo hi), port @Fixed{port 8080}})"#,
        true,
    );
    let reg = node("alpha");

    let err = reg
        .start_named("job", &caller(&ws))
        .expect_err("a fixed port in a linked worktree must be refused");
    assert!(
        err.to_string().contains("fixed port"),
        "the refusal should explain the implication: {err}"
    );
    let _ = std::fs::remove_dir_all(&ws);
}

/// …unless the declaration says otherwise. The implication is a default, not a
/// rule.
#[test]
fn an_explicit_each_overrides_the_fixed_port_implication() {
    let ws = tree(
        "fixed-explicit",
        r#"proc ({name job, argv (echo hi), port @Fixed{port 8081}, worktree @Each})"#,
        true,
    );
    let reg = node("alpha");
    reg.start_named("job", &caller(&ws))
        .expect("an explicit `each` wins over the implication");
    let _ = std::fs::remove_dir_all(&ws);
}

/// An empty host list means anywhere — the permissive default.
#[test]
fn no_host_list_runs_anywhere() {
    let ws = tree("anyhost", EACH, false);
    for name in ["alpha", "beta", "gamma"] {
        let reg = node(name);
        reg.start_named("job", &caller(&ws))
            .unwrap_or_else(|e| panic!("should run on {name}: {e}"));
    }
    let _ = std::fs::remove_dir_all(&ws);
}

/// A host allow-list pins a machine-specific job to its box, from **one**
/// checked-in manifest shared across machines.
#[test]
fn a_host_list_admits_only_listed_nodes() {
    let ws = tree(
        "hosts",
        r#"proc ({name job, argv (echo hi), hosts (workstation laptop)})"#,
        false,
    );

    node("workstation")
        .start_named("job", &caller(&ws))
        .expect("listed");
    node("laptop")
        .start_named("job", &caller(&ws))
        .expect("also listed");

    let err = node("server")
        .start_named("job", &caller(&ws))
        .expect_err("not listed");
    let message = err.to_string();
    assert!(message.contains("server"), "must name this node: {message}");
    assert!(
        message.contains("workstation"),
        "and what is allowed: {message}"
    );

    let _ = std::fs::remove_dir_all(&ws);
}

/// The host is matched against the **node identity given to the engine**, not
/// the OS hostname — so the engine never calls out to the OS and cannot drift
/// from the mesh's idea of what this node is called.
#[test]
fn matching_uses_the_injected_node_identity() {
    let ws = tree(
        "identity",
        r#"proc ({name job, argv (echo hi), hosts (declared-name)})"#,
        false,
    );
    // Whatever this machine's hostname is, the engine only knows what it is told.
    node("declared-name")
        .start_named("job", &caller(&ws))
        .expect("the injected identity is what counts");
    let _ = std::fs::remove_dir_all(&ws);
}

/// Gates apply to **declarations**, which is where they are written. An ad-hoc
/// request carries none and is unaffected.
#[test]
fn an_ad_hoc_job_has_no_gates_to_fail() {
    let ws = tree("adhoc", MAIN_ONLY, true);
    let reg = node("alpha");
    reg.start(JobRequest::new(["echo", "hi"], &ws).in_scope(Scope::resolve(&ws)))
        .expect("an ad-hoc job declares nothing, so nothing gates it");
    let _ = std::fs::remove_dir_all(&ws);
}
