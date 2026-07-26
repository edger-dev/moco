//! Whose job is this, and who may write to it.
//!
//! implements: workspace-is-the-owner-not-session
//! implements: reads-global-writes-own-workspace

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use moco_job::scope::{Caller, Scope};
use moco_job::{JobRegistry, JobRequest};

static SEQ: AtomicU64 = AtomicU64::new(0);

#[allow(
    clippy::expect_used,
    reason = "test helper: a failure is a broken harness"
)]
fn registry() -> JobRegistry {
    JobRegistry::ungoverned().expect("registry")
}

/// A throwaway directory tree, removed by the caller.
#[allow(
    clippy::expect_used,
    reason = "test helper: a failure is a broken harness"
)]
fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "moco-ownership-{}-{}-{name}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create scratch");
    dir.canonicalize().expect("canonicalize")
}

/// A repo is a directory holding a `.git` **directory** — the main working tree.
#[allow(
    clippy::expect_used,
    reason = "test helper: a failure is a broken harness"
)]
fn fake_repo(name: &str) -> PathBuf {
    let root = scratch(name);
    std::fs::create_dir_all(root.join(".git")).expect("create .git dir");
    root
}

/// A linked worktree holds a `.git` **file**, not a directory. It is a workspace
/// in its own right, not part of the tree it points at.
#[allow(
    clippy::expect_used,
    reason = "test helper: a failure is a broken harness"
)]
fn fake_linked_worktree(name: &str, points_at: &Path) -> PathBuf {
    let root = scratch(name);
    std::fs::write(
        root.join(".git"),
        format!("gitdir: {}/.git/worktrees/wt\n", points_at.display()),
    )
    .expect("write .git file");
    root
}

/// The owner is the **worktree root**, not whatever subdirectory you happened to
/// be standing in.
#[test]
fn a_subdirectory_belongs_to_its_worktree_root() {
    let root = fake_repo("subdir");
    let deep = root.join("crates").join("thing").join("src");
    std::fs::create_dir_all(&deep).expect("create subdirs");

    assert_eq!(
        Scope::resolve(&deep),
        Scope::workspace(&root),
        "a job started three levels down belongs to the repo, not the directory"
    );

    let _ = std::fs::remove_dir_all(&root);
}

/// A linked worktree is its own workspace. Its `.git` is a *file*, and treating
/// only a `.git` directory as a root would silently attribute its jobs to the
/// main tree — or to the filesystem root.
#[test]
fn a_linked_worktree_is_its_own_workspace() {
    let main = fake_repo("main-tree");
    let linked = fake_linked_worktree("linked-tree", &main);

    assert_eq!(Scope::resolve(&linked), Scope::workspace(&linked));
    assert_ne!(Scope::resolve(&linked), Scope::resolve(&main));

    let _ = std::fs::remove_dir_all(&main);
    let _ = std::fs::remove_dir_all(&linked);
}

/// Outside a repo there is still an answer: the directory itself. Ownership is
/// never absent, so no code path has to handle "no workspace".
#[test]
fn outside_a_repo_the_directory_is_the_workspace() {
    let plain = scratch("no-repo");
    assert_eq!(Scope::resolve(&plain), Scope::workspace(&plain));
    let _ = std::fs::remove_dir_all(&plain);
}

/// A job records which workspace owns it.
#[test]
fn a_job_records_its_owning_workspace() {
    let repo = fake_repo("owner");
    let reg = registry();

    let id = reg
        .start(JobRequest::new(["true"], &repo).in_scope(Scope::resolve(&repo)))
        .expect("start");

    let owner = reg.scope_of(&id).expect("the job should have an owner");
    assert_eq!(owner, Scope::workspace(&repo));

    let _ = std::fs::remove_dir_all(&repo);
}

/// **Reads are node-global.** A session in one workspace still sees every job on
/// the machine — that is the question a machine-global supervisor exists to
/// answer, and scoping reads would remove it.
#[test]
fn reads_see_every_workspace() {
    let a = fake_repo("read-a");
    let b = fake_repo("read-b");
    let reg = registry();

    let in_a = reg
        .start(JobRequest::new(["true"], &a).in_scope(Scope::resolve(&a)))
        .expect("start in a");
    let in_b = reg
        .start(JobRequest::new(["true"], &b).in_scope(Scope::resolve(&b)))
        .expect("start in b");

    let listed: Vec<_> = reg.list().into_iter().map(|(id, _)| id).collect();
    assert!(
        listed.contains(&in_a) && listed.contains(&in_b),
        "got {listed:?}"
    );

    let _ = std::fs::remove_dir_all(&a);
    let _ = std::fs::remove_dir_all(&b);
}

/// **A foreign workspace cannot write**, and the refusal names both workspaces
/// so the caller can see what it got wrong. It is refused, never silently
/// retargeted and never silently ignored.
#[test]
fn a_foreign_workspace_cannot_kill_and_the_refusal_names_both() {
    let mine = fake_repo("mine");
    let theirs = fake_repo("theirs");
    let reg = registry();

    let id = reg
        .start(JobRequest::new(["sleep", "30"], &theirs).in_scope(Scope::resolve(&theirs)))
        .expect("start");

    let err = reg
        .kill(&id, &Caller::Scoped(Scope::resolve(&mine)))
        .expect_err("a foreign workspace must be refused");

    let message = err.to_string();
    assert!(
        message.contains(&mine.display().to_string()),
        "must name the caller's workspace, got: {message}"
    );
    assert!(
        message.contains(&theirs.display().to_string()),
        "must name the owning workspace, got: {message}"
    );

    // And it really did not act.
    let _ = reg.kill(&id, &Caller::Console);
    let _ = std::fs::remove_dir_all(&mine);
    let _ = std::fs::remove_dir_all(&theirs);
}

/// The owning workspace may write to its own jobs.
#[test]
fn the_owning_workspace_may_write() {
    let mine = fake_repo("own-write");
    let reg = registry();

    let id = reg
        .start(JobRequest::new(["sleep", "30"], &mine).in_scope(Scope::resolve(&mine)))
        .expect("start");

    reg.kill(&id, &Caller::Scoped(Scope::resolve(&mine)))
        .expect("a workspace may kill its own job");

    let _ = std::fs::remove_dir_all(&mine);
}

/// **The console is the carve-out** — the one caller with global write
/// authority, and it says so explicitly rather than getting it by omission.
#[test]
fn the_console_may_write_across_workspaces() {
    let theirs = fake_repo("console-target");
    let reg = registry();

    let id = reg
        .start(JobRequest::new(["sleep", "30"], &theirs).in_scope(Scope::resolve(&theirs)))
        .expect("start");

    reg.kill(&id, &Caller::Console)
        .expect("the console may act globally");

    let _ = std::fs::remove_dir_all(&theirs);
}

/// Ownership is durable: it survives the daemon that recorded it.
#[test]
fn scope_survives_a_restart() {
    let repo = fake_repo("durable");
    let dir = scratch("durable-registry");

    let id = {
        let reg = registry().with_dir(&dir).expect("with_dir");
        reg.start(JobRequest::new(["sleep", "30"], &repo).in_scope(Scope::resolve(&repo)))
            .expect("start")
    };

    let reopened = registry().with_dir(&dir).expect("reopen");
    assert_eq!(
        reopened
            .scope_of(&id)
            .expect("re-adopted job keeps an owner"),
        Scope::workspace(&repo),
        "ownership must survive the daemon that recorded it"
    );

    let _ = reopened.kill(&id, &Caller::Console);
    let _ = std::fs::remove_dir_all(&repo);
    let _ = std::fs::remove_dir_all(&dir);
}
