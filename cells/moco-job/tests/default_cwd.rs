//! An ad-hoc start without a cwd runs in the node's default directory: its
//! allowed root when governed, the daemon's own directory when not. A caller
//! on another machine has no way to name a path here, so "none" must mean
//! somewhere that exists. An explicit cwd is still used as given and confined.
//!
//! implements: 213c9b96650e507051c35dd2d9f71d5d8ba6bb3e8c4ec72b2a9e1b6b7f460104@213c9b96650e507051c35dd2d9f71d5d8ba6bb3e8c4ec72b2a9e1b6b7f460104

#![allow(clippy::unwrap_used, clippy::expect_used, reason = "tests")]

use std::path::PathBuf;

use moco_job::wire::{self, PreflightRequest, StartRequest};
use moco_job::{JobError, JobRegistry, JobRequest, JobStatus, NodePolicy, RuleSet, SeedConfig};

fn argv(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|s| (*s).to_string()).collect()
}

/// A fresh allowed root, canonical, so it compares to what `pwd` prints.
fn allowed(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("moco-default-cwd-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d.canonicalize().unwrap()
}

fn governed(root: &PathBuf) -> JobRegistry {
    let rules = RuleSet::from_seed(SeedConfig {
        allow: vec![argv(&["pwd"])],
        deny: vec![],
    });
    JobRegistry::with_policy(NodePolicy::new(rules, root)).unwrap()
}

fn output(reg: &JobRegistry, id: &moco_job::JobId) -> String {
    let tail = reg.tail(id, 0).unwrap();
    String::from_utf8_lossy(&tail.bytes).trim().to_owned()
}

/// Governed: no cwd runs in the allowed root, and the job says so.
#[test]
fn no_cwd_runs_in_the_allowed_root() {
    let root = allowed("gov");
    let reg = governed(&root);
    let id = reg.start(JobRequest::new(["pwd"], "")).unwrap();
    assert_eq!(reg.wait(&id).unwrap().status, JobStatus::Done { code: 0 });
    assert_eq!(output(&reg, &id), root.display().to_string());
    assert_eq!(reg.cwd_of(&id), Some(root.clone()));
    let _ = std::fs::remove_dir_all(&root);
}

/// The job belongs to the directory it runs in, not to an empty path.
#[test]
fn no_cwd_scopes_the_job_to_where_it_runs() {
    let root = allowed("scope");
    let reg = governed(&root);
    let id = reg.start(JobRequest::new(["pwd"], "")).unwrap();
    reg.wait(&id).unwrap();
    assert_eq!(reg.scope_of(&id), Some(moco_job::Scope::resolve(&root)));
    let _ = std::fs::remove_dir_all(&root);
}

/// Ungoverned: no cwd runs in the daemon's own directory.
#[test]
fn ungoverned_no_cwd_runs_in_the_daemons_directory() {
    let reg = JobRegistry::ungoverned().unwrap();
    let id = reg.start(JobRequest::new(["pwd"], "")).unwrap();
    assert_eq!(reg.wait(&id).unwrap().status, JobStatus::Done { code: 0 });
    let own = std::env::current_dir().unwrap().canonicalize().unwrap();
    assert_eq!(output(&reg, &id), own.display().to_string());
}

/// A preflight without a cwd reports the allowed root, with no cwd error.
#[test]
fn a_preflight_without_a_cwd_reports_the_allowed_root() {
    let root = allowed("pre");
    let reg = governed(&root);
    let out = reg.preflight(&argv(&["pwd"]), std::path::Path::new(""));
    assert_eq!(out.cwd_error, None);
    assert_eq!(out.resolved_cwd, Some(root.clone()));
    let _ = std::fs::remove_dir_all(&root);
}

/// An explicit cwd is still confined: one outside the root is refused.
#[test]
fn an_explicit_cwd_is_still_confined() {
    let root = allowed("explicit");
    let reg = governed(&root);
    let err = reg.start(JobRequest::new(["pwd"], "/")).unwrap_err();
    assert!(matches!(err, JobError::CwdEscape { .. }), "got {err:?}");
    let _ = std::fs::remove_dir_all(&root);
}

/// Over the wire: an empty cwd in a start and a preflight is the default.
#[test]
fn an_empty_cwd_over_the_wire_is_the_default() {
    let root = allowed("wire");
    let reg = governed(&root);

    let request = wire::encode(&PreflightRequest {
        argv: argv(&["pwd"]),
        cwd: String::new(),
    })
    .unwrap();
    let reply: wire::PreflightReply =
        wire::decode(&wire::dispatch(&reg, "preflight", &request).unwrap()).unwrap();
    assert_eq!(reply.cwd_error, "");
    assert_eq!(reply.resolved_cwd, root.display().to_string());

    let request = wire::encode(&StartRequest {
        argv: argv(&["pwd"]),
        cwd: String::new(),
        deadline_ms: 0,
        caller: wire::WireCaller::Session {
            cwd: "/somewhere/on/another/machine".into(),
        },
        env: vec![],
        human_view: moco_job::lens::HumanView::Logs,
    })
    .unwrap();
    let started: wire::StartReply =
        wire::decode(&wire::dispatch(&reg, "start", &request).unwrap()).unwrap();
    let request = wire::encode(&wire::WaitRequest {
        id: started.id.clone(),
    })
    .unwrap();
    let waited: wire::WaitReply =
        wire::decode(&wire::dispatch(&reg, "wait", &request).unwrap()).unwrap();
    assert_eq!(waited.status, JobStatus::Done { code: 0 });
    assert_eq!(waited.resolved_cwd, root.display().to_string());
    let id = moco_job::JobId(started.id);
    assert_eq!(output(&reg, &id), root.display().to_string());
    let _ = std::fs::remove_dir_all(&root);
}
