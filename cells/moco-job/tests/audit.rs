//! Phase 3 audit tests — *every attempt is durable history*.
//!
//! Denials included. The verdict handed back to the caller must be the same
//! value written to the record — one source of truth, not two.
//!
//! implements: audit-every-attempt
//! implements: agent-self-sufficiency

use moco_job::{
    AuditSink, Decision, DeniedReason, FileAuditLog, JobError, JobRegistry, JobRequest, JobStatus,
    NodePolicy, RuleSet, SeedConfig, Verdict,
};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

fn root() -> PathBuf {
    std::env::temp_dir()
}

fn argv(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|s| (*s).to_string()).collect()
}

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

/// A completed job is audited with the authority it ran under.
#[test]
fn completed_job_is_audited() {
    let reg = governed();
    let id = reg.start(JobRequest::new(["echo", "ok"], root())).unwrap();
    reg.wait(&id).unwrap();

    let records = reg.audit().records().unwrap();
    assert_eq!(records.len(), 1, "expected exactly one record");
    assert_eq!(records[0].verdict, Verdict::SeedAllow);
    assert_eq!(records[0].status, JobStatus::Done { code: 0 });
    assert_eq!(records[0].argv, argv(&["echo", "ok"]));
}

/// **A denied attempt is recorded too** — written at denial time, not
/// reconstructed later. This is the record most worth keeping.
#[test]
fn denied_attempt_is_audited_at_denial_time() {
    let reg = governed();
    // No wait() call at all: the record must already exist.
    reg.start(JobRequest::new(["echo", "nope"], root())).unwrap();

    let records = reg.audit().records().unwrap();
    assert_eq!(records.len(), 1, "a denial must be recorded immediately");
    assert_eq!(records[0].verdict, Verdict::SeedDeny);
    assert_eq!(records[0].status, JobStatus::Denied(DeniedReason::Rule));
}

/// A human rejection is audited under its own authority.
#[test]
fn human_rejection_is_audited() {
    let reg = governed();
    let id = reg
        .start(JobRequest::new(["echo", "unlisted"], root()))
        .unwrap();
    reg.decide(&id, Decision::DenyOnce).unwrap();

    let records = reg.audit().records().unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].verdict, Verdict::RejectedOnce);
}

/// A fail-closed denial (nobody decided) is audited as such.
#[test]
fn no_approver_denial_is_audited() {
    let reg = governed();
    let id = reg
        .start(JobRequest::new(["echo", "unlisted"], root()))
        .unwrap();
    reg.wait(&id).unwrap();

    let records = reg.audit().records().unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].verdict, Verdict::NoApprover);
    assert_eq!(
        records[0].status,
        JobStatus::Denied(DeniedReason::NoApprover)
    );
}

/// An approved job is audited under `ApprovedOnce`, with the argv that ran.
#[test]
fn approved_job_is_audited_with_the_corrected_argv() {
    let reg = governed();
    let id = reg
        .start(JobRequest::new(["echo", "wrong"], root()))
        .unwrap();
    reg.decide(&id, Decision::allow_edited(["echo", "corrected"]))
        .unwrap();
    reg.wait(&id).unwrap();

    let records = reg.audit().records().unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].verdict, Verdict::ApprovedOnce);
    assert_eq!(
        records[0].argv,
        argv(&["echo", "corrected"]),
        "the audit must record exactly what ran"
    );
}

/// The outcome is self-describing, and its verdict/cwd are byte-equal to the
/// audit row — one source of truth.
#[test]
fn outcome_is_self_describing_and_matches_the_audit() {
    let reg = governed();
    let outcome = reg.run(JobRequest::new(["echo", "ok"], root())).unwrap();

    assert_eq!(outcome.verdict, Verdict::SeedAllow);
    assert!(outcome.resolved_cwd.is_absolute());

    let records = reg.audit().records().unwrap();
    assert_eq!(records[0].verdict, outcome.verdict);
    assert_eq!(records[0].cwd, outcome.resolved_cwd.display().to_string());
    assert_eq!(records[0].status, outcome.status);
}

/// Every attempt lands in the log, in order.
#[test]
fn every_attempt_is_recorded() {
    let reg = governed();
    reg.run(JobRequest::new(["echo", "ok"], root())).unwrap();
    reg.start(JobRequest::new(["echo", "nope"], root())).unwrap();
    reg.run(JobRequest::new(["echo", "ok"], root())).unwrap();

    let records = reg.audit().records().unwrap();
    assert_eq!(records.len(), 3);
    let verdicts: Vec<_> = records.iter().map(|r| r.verdict).collect();
    assert_eq!(
        verdicts,
        vec![Verdict::SeedAllow, Verdict::SeedDeny, Verdict::SeedAllow]
    );
}

/// The durable sink survives the registry: records are appended to a file and
/// read back with their fields intact.
#[test]
fn file_audit_log_is_durable_and_append_only() {
    let path =
        std::env::temp_dir().join(format!("moco-audit-{}-{}.log", std::process::id(), "durable"));
    let _ = std::fs::remove_file(&path);

    {
        let reg = JobRegistry::with_policy(policy())
            .with_audit(Arc::new(FileAuditLog::new(&path)));
        reg.run(JobRequest::new(["echo", "ok"], root())).unwrap();
        reg.start(JobRequest::new(["echo", "nope"], root())).unwrap();
    }

    // A fresh reader over the same file sees the history the registry wrote.
    let reread = FileAuditLog::new(&path);
    let records = reread.records().unwrap();
    assert_eq!(records.len(), 2);
    assert_eq!(records[0].verdict, Verdict::SeedAllow);
    assert_eq!(records[0].argv, argv(&["echo", "ok"]));
    assert_eq!(records[1].verdict, Verdict::SeedDeny);
    assert_eq!(records[1].status, JobStatus::Denied(DeniedReason::Rule));

    // Appending again preserves what was there.
    {
        let reg = JobRegistry::with_policy(policy())
            .with_audit(Arc::new(FileAuditLog::new(&path)));
        reg.run(JobRequest::new(["echo", "ok"], root())).unwrap();
    }
    assert_eq!(reread.records().unwrap().len(), 3, "history must not be rewritten");

    let _ = std::fs::remove_file(&path);
}

/// A program-not-found error names the binary *and* the PATH that was searched.
#[test]
fn spawn_failure_names_the_searched_path() {
    let reg = JobRegistry::ungoverned();
    let err = reg
        .start(JobRequest::new(["definitely-not-a-real-program-xyz"], root()))
        .unwrap_err();

    match &err {
        JobError::Spawn {
            program,
            searched_path,
            ..
        } => {
            assert_eq!(program, "definitely-not-a-real-program-xyz");
            assert!(!searched_path.is_empty(), "must name the PATH searched");
        }
        other => panic!("expected Spawn, got {other:?}"),
    }
    let rendered = err.to_string();
    assert!(rendered.contains("PATH searched"), "got {rendered}");
}
