//! Phase 2 governance tests — *the gate in front of the job*.
//!
//! A governed command is an ordinary job on the registry with a rule-set
//! evaluated **before** it spawns. These tests pin the trust core: exact-argv
//! matching with deny beating allow, a fail-closed pending state, approval as a
//! job state (including the corrected-argv back-channel), cwd confinement, and
//! read-only preflight.
//!
//! implements: rule-set-target-owned-human-mutated
//! implements: approval-is-a-job-state
//! implements: four-verdicts-exact-argv
//! implements: agent-self-sufficiency

use moco_job::{
    Decision, DeniedReason, Disposition, JobError, JobRegistry, JobRequest, JobStatus, NodePolicy,
    RuleSet, SeedConfig,
};
use std::path::PathBuf;
use std::time::Duration;

fn root() -> PathBuf {
    std::env::temp_dir()
}

fn argv(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|s| (*s).to_string()).collect()
}

/// A policy allowing `echo ok` and denying `echo nope`, with a short approval
/// timeout so the fail-closed path is testable.
fn policy() -> NodePolicy {
    let rules = RuleSet::from_seed(SeedConfig {
        allow: vec![argv(&["echo", "ok"])],
        deny: vec![argv(&["echo", "nope"])],
    });
    NodePolicy::new(rules, root()).with_approval_timeout(Duration::from_millis(200))
}

fn governed() -> JobRegistry {
    JobRegistry::with_policy(policy())
}

/// An argv a seed rule allows spawns and runs to completion.
#[test]
fn seeded_allow_argv_runs() {
    let reg = governed();
    let outcome = reg.run(JobRequest::new(["echo", "ok"], root())).unwrap();
    assert_eq!(outcome.status, JobStatus::Done { code: 0 });
}

/// An argv a seed rule denies is registered as a job but never spawns.
#[test]
fn seeded_deny_never_spawns() {
    let reg = governed();
    let id = reg.start(JobRequest::new(["echo", "nope"], root())).unwrap();

    let outcome = reg.wait(&id).unwrap();
    assert_eq!(outcome.status, JobStatus::Denied(DeniedReason::Rule));

    let tail = reg.tail(&id, 0).unwrap();
    assert!(
        tail.bytes.is_empty(),
        "a denied job must produce no output, got {:?}",
        String::from_utf8_lossy(&tail.bytes)
    );
}

/// Deny beats allow when the same exact argv appears in both lists.
#[test]
fn deny_beats_allow() {
    let rules = RuleSet::from_seed(SeedConfig {
        allow: vec![argv(&["echo", "both"])],
        deny: vec![argv(&["echo", "both"])],
    });
    assert_eq!(rules.evaluate(&argv(&["echo", "both"])), Disposition::Deny);
}

/// An argv matching no rule pends, then fails closed when nobody decides.
#[test]
fn unmatched_argv_pends_then_denies_on_timeout() {
    let reg = governed();
    let id = reg
        .start(JobRequest::new(["echo", "unlisted"], root()))
        .unwrap();

    let tail = reg.tail(&id, 0).unwrap();
    assert_eq!(tail.status, JobStatus::PendingApproval);

    let outcome = reg.wait(&id).unwrap();
    assert_eq!(outcome.status, JobStatus::Denied(DeniedReason::NoApprover));
}

/// Approving a pending job spawns it.
#[test]
fn decide_allow_once_runs_a_pending_job() {
    let reg = governed();
    let id = reg
        .start(JobRequest::new(["echo", "unlisted"], root()))
        .unwrap();

    reg.decide(&id, Decision::allow()).unwrap();

    let outcome = reg.wait(&id).unwrap();
    assert_eq!(outcome.status, JobStatus::Done { code: 0 });
}

/// Rejecting a pending job denies it, by decision.
#[test]
fn decide_deny_once_denies_the_job() {
    let reg = governed();
    let id = reg
        .start(JobRequest::new(["echo", "unlisted"], root()))
        .unwrap();

    reg.decide(&id, Decision::DenyOnce).unwrap();

    let outcome = reg.wait(&id).unwrap();
    assert_eq!(outcome.status, JobStatus::Denied(DeniedReason::Decision));
}

/// Edit-and-approve: the corrected argv is what actually runs.
#[test]
fn edited_argv_on_approve_runs_the_correction() {
    let reg = governed();
    let id = reg
        .start(JobRequest::new(["echo", "wrong"], root()))
        .unwrap();

    reg.decide(&id, Decision::allow_edited(["echo", "corrected"]))
        .unwrap();
    reg.wait(&id).unwrap();

    let tail = reg.tail(&id, 0).unwrap();
    let out = String::from_utf8_lossy(&tail.bytes);
    assert!(out.contains("corrected"), "got {out:?}");
    assert!(!out.contains("wrong"), "original argv must not run, got {out:?}");
}

/// A decision on a job that is not pending is an error, not a silent no-op.
#[test]
fn decide_on_a_non_pending_job_errors() {
    let reg = governed();
    let id = reg.start(JobRequest::new(["echo", "ok"], root())).unwrap();
    reg.wait(&id).unwrap();

    let err = reg.decide(&id, Decision::allow()).unwrap_err();
    assert!(matches!(err, JobError::NotPending(_)), "got {err:?}");
}

/// A cwd outside the node's allowed root is rejected before anything runs.
#[test]
fn cwd_escaping_allowed_root_is_rejected() {
    let reg = governed();
    let err = reg
        .start(JobRequest::new(["echo", "ok"], "/"))
        .unwrap_err();
    assert!(matches!(err, JobError::CwdEscape { .. }), "got {err:?}");
}

/// Preflight reports each disposition and runs/queues nothing.
#[test]
fn preflight_reports_disposition_without_running() {
    let reg = governed();

    assert_eq!(
        reg.preflight(&argv(&["echo", "ok"]), &root()).disposition,
        Disposition::Allow
    );
    assert_eq!(
        reg.preflight(&argv(&["echo", "nope"]), &root()).disposition,
        Disposition::Deny
    );
    assert_eq!(
        reg.preflight(&argv(&["echo", "unlisted"]), &root())
            .disposition,
        Disposition::NeedsApproval
    );

    // Nothing was queued: an id that would exist if preflight registered a job
    // must not be there.
    assert!(reg.wait(&moco_job::JobId(0)).is_err());
}

/// Preflight resolves `argv[0]` on PATH and reports the effective PATH.
#[test]
fn preflight_resolves_program_and_path() {
    let reg = governed();
    let pre = reg.preflight(&argv(&["echo", "ok"]), &root());

    let program = pre.program.expect("echo should resolve on PATH");
    assert!(program.is_absolute(), "expected absolute path, got {program:?}");
    assert!(!pre.effective_path.is_empty());
    assert!(pre.resolved_cwd.is_some());
    assert!(pre.cwd_error.is_none());
}

/// A program that does not resolve is reported as such, with the PATH searched.
#[test]
fn preflight_reports_missing_program() {
    let reg = governed();
    let pre = reg.preflight(&argv(&["definitely-not-a-real-program-xyz"]), &root());

    assert!(pre.program.is_none());
    assert!(!pre.effective_path.is_empty(), "must name the PATH searched");
}

/// Preflight reports a cwd confinement failure rather than resolving it.
#[test]
fn preflight_reports_cwd_confinement_error() {
    let reg = governed();
    let pre = reg.preflight(&argv(&["echo", "ok"]), std::path::Path::new("/"));

    assert!(pre.resolved_cwd.is_none());
    assert!(pre.cwd_error.is_some());
}

/// The rule-set parses from a node's committed Styx seed.
#[test]
fn rule_set_loads_from_styx_seed() {
    let seed = r#"
allow (
  ("echo" "ok")
)
deny (
  ("echo" "nope")
)
"#;
    let rules = RuleSet::from_styx(seed).expect("seed should parse");

    assert_eq!(rules.evaluate(&argv(&["echo", "ok"])), Disposition::Allow);
    assert_eq!(rules.evaluate(&argv(&["echo", "nope"])), Disposition::Deny);
    assert_eq!(
        rules.evaluate(&argv(&["echo", "other"])),
        Disposition::NeedsApproval
    );
}

/// An ungoverned registry (no policy) runs without a gate — the bare substrate.
#[test]
fn ungoverned_registry_has_no_gate() {
    let reg = JobRegistry::new();
    assert!(reg.policy().is_none());
    let outcome = reg.run(JobRequest::new(["echo", "ungoverned"], root())).unwrap();
    assert_eq!(outcome.status, JobStatus::Done { code: 0 });
}

