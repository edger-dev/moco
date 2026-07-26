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

// A construction failure here means the test environment cannot create a
// registry directory at all — a broken harness, not a behaviour under test.
#[allow(
    clippy::unwrap_used,
    reason = "test helper: a failure is a broken harness"
)]
fn governed() -> JobRegistry {
    JobRegistry::with_policy(policy()).unwrap()
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
    reg.start(JobRequest::new(["echo", "nope"], root()))
        .unwrap();

    let records = reg.audit().records().unwrap();
    assert_eq!(records.len(), 1, "a denial must be recorded immediately");
    assert_eq!(records[0].verdict, Verdict::SeedDeny);
    assert_eq!(
        records[0].status,
        JobStatus::Denied {
            reason: DeniedReason::Rule
        }
    );
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
        JobStatus::Denied {
            reason: DeniedReason::NoApprover
        }
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
    reg.start(JobRequest::new(["echo", "nope"], root()))
        .unwrap();
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
    let path = std::env::temp_dir().join(format!(
        "moco-audit-{}-{}.log",
        std::process::id(),
        "durable"
    ));
    let _ = std::fs::remove_file(&path);

    {
        let reg = JobRegistry::with_policy(policy())
            .unwrap()
            .with_audit(Arc::new(FileAuditLog::new(&path)));
        reg.run(JobRequest::new(["echo", "ok"], root())).unwrap();
        reg.start(JobRequest::new(["echo", "nope"], root()))
            .unwrap();
    }

    // A fresh reader over the same file sees the history the registry wrote.
    let reread = FileAuditLog::new(&path);
    let records = reread.records().unwrap();
    assert_eq!(records.len(), 2);
    assert_eq!(records[0].verdict, Verdict::SeedAllow);
    assert_eq!(records[0].argv, argv(&["echo", "ok"]));
    assert_eq!(records[1].verdict, Verdict::SeedDeny);
    assert_eq!(
        records[1].status,
        JobStatus::Denied {
            reason: DeniedReason::Rule
        }
    );

    // Appending again preserves what was there.
    {
        let reg = JobRegistry::with_policy(policy())
            .unwrap()
            .with_audit(Arc::new(FileAuditLog::new(&path)));
        reg.run(JobRequest::new(["echo", "ok"], root())).unwrap();
    }
    assert_eq!(
        reread.records().unwrap().len(),
        3,
        "history must not be rewritten"
    );

    let _ = std::fs::remove_file(&path);
}

/// A program-not-found error names the binary *and* the PATH that was searched.
#[test]
fn spawn_failure_names_the_searched_path() {
    let reg = JobRegistry::ungoverned().unwrap();
    let err = reg
        .start(JobRequest::new(
            ["definitely-not-a-real-program-xyz"],
            root(),
        ))
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

// ── regression tests for the Phase 3 review findings ───────────────────────

/// A sink that always fails, to pin the audit-failure path.
#[derive(Default)]
struct FailingAuditLog;

impl AuditSink for FailingAuditLog {
    fn append(&self, _record: moco_job::AuditRecord) -> Result<(), JobError> {
        Err(JobError::Audit("sink is down".into()))
    }
    fn records(&self) -> Result<Vec<moco_job::AuditRecord>, JobError> {
        Ok(Vec::new())
    }
}

/// An audit write failure must not be swallowed on a retry: marking the job
/// audited before the write succeeded would turn one failure into permanent,
/// silent history loss.
#[test]
fn failed_audit_write_is_not_silently_swallowed() {
    let reg = JobRegistry::with_policy(policy())
        .unwrap()
        .with_audit(Arc::new(FailingAuditLog));
    let id = reg.start(JobRequest::new(["echo", "ok"], root())).unwrap();

    let first = reg.wait(&id);
    assert!(
        first.is_err(),
        "a failed audit write must fail the operation"
    );

    // The retry must fail the same way, not report a clean success for a job
    // that was never recorded.
    let second = reg.wait(&id);
    assert!(
        second.is_err(),
        "a job with no record must not later report success"
    );
}

/// Hostile argv values must round-trip through the durable log, and each record
/// must stay exactly one line. A value with two or more newlines is rendered by
/// Styx as a multi-line heredoc, which would otherwise split one record across
/// several lines and make the whole history unreadable.
#[test]
fn hostile_argv_round_trips_as_one_line() {
    let hostile = [
        "",
        "a b",
        "@tag",
        "{}",
        "//x",
        "a\"b",
        "a\nb",
        "a\nb\nc",
        "{job 9, verdict @SeedAllow}",
        "back\\slash",
        "héllo→",
    ];

    for (i, value) in hostile.iter().enumerate() {
        let path = std::env::temp_dir().join(format!(
            "moco-audit-hostile-{}-{}.log",
            std::process::id(),
            i
        ));
        let _ = std::fs::remove_file(&path);

        let rules = RuleSet::from_seed(SeedConfig {
            allow: vec![vec!["echo".to_string(), (*value).to_string()]],
            deny: vec![],
        });
        {
            let reg = JobRegistry::with_policy(NodePolicy::new(rules, root()))
                .unwrap()
                .with_audit(Arc::new(FileAuditLog::new(&path)));
            reg.run(JobRequest::new(["echo", *value], root())).unwrap();
        }

        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(
            text.lines().count(),
            1,
            "value {value:?} produced a multi-line record: {text:?}"
        );

        let records = FileAuditLog::new(&path).records().unwrap();
        assert_eq!(records.len(), 1, "value {value:?} did not read back");
        assert_eq!(
            records[0].argv,
            vec!["echo".to_string(), (*value).to_string()],
            "value {value:?} did not survive the round trip"
        );

        let _ = std::fs::remove_file(&path);
    }
}

/// Every status and verdict shape must survive the wire format, not just the
/// two the happy-path tests happen to produce.
#[test]
fn all_status_and_verdict_shapes_round_trip() {
    use moco_job::AuditRecord;

    let statuses = [
        JobStatus::Done { code: 0 },
        JobStatus::Done { code: -1 },
        JobStatus::Killed,
        JobStatus::TimedOut,
        JobStatus::Denied {
            reason: DeniedReason::Rule,
        },
        JobStatus::Denied {
            reason: DeniedReason::Decision,
        },
        JobStatus::Denied {
            reason: DeniedReason::NoApprover,
        },
        JobStatus::Denied {
            reason: DeniedReason::CwdEscape,
        },
    ];
    let verdicts = [
        Verdict::Ungoverned,
        Verdict::SeedAllow,
        Verdict::ApprovedOnce,
        Verdict::SeedDeny,
        Verdict::RejectedOnce,
        Verdict::NoApprover,
    ];

    let path = std::env::temp_dir().join(format!("moco-audit-shapes-{}.log", std::process::id()));
    let _ = std::fs::remove_file(&path);
    let log = FileAuditLog::new(&path);

    let mut expected = Vec::new();
    for status in &statuses {
        for verdict in &verdicts {
            let rec = AuditRecord::new(
                "test-job-1",
                argv(&["echo", "x"]),
                root(),
                *verdict,
                status.clone(),
            );
            log.append(rec.clone()).unwrap();
            expected.push(rec);
        }
    }

    let back = log.records().unwrap();
    assert_eq!(back.len(), expected.len());
    for (got, want) in back.iter().zip(expected.iter()) {
        assert_eq!(got.status, want.status);
        assert_eq!(got.verdict, want.verdict);
    }

    let _ = std::fs::remove_file(&path);
}

/// A killed job and a timed-out job are both recorded.
#[test]
fn killed_and_timed_out_jobs_are_audited() {
    let reg = JobRegistry::ungoverned().unwrap();

    let killed = reg.start(JobRequest::new(["sleep", "10"], root())).unwrap();
    reg.kill(&killed).unwrap();
    reg.wait(&killed).unwrap();

    let timed = reg
        .start(JobRequest::new(["sleep", "10"], root()).with_deadline(Duration::from_millis(100)))
        .unwrap();
    reg.wait(&timed).unwrap();

    let records = reg.audit().records().unwrap();
    let statuses: Vec<_> = records.iter().map(|r| r.status.clone()).collect();
    assert!(statuses.contains(&JobStatus::Killed), "got {statuses:?}");
    assert!(statuses.contains(&JobStatus::TimedOut), "got {statuses:?}");
}

/// An attempt rejected before it could become a job — a probe at the
/// confinement boundary — is still recorded.
#[test]
fn cwd_escape_attempt_is_recorded() {
    let reg = governed();
    let err = reg.start(JobRequest::new(["echo", "ok"], "/")).unwrap_err();
    assert!(matches!(err, JobError::CwdEscape { .. }), "got {err:?}");

    let records = reg.audit().records().unwrap();
    assert_eq!(records.len(), 1, "a rejected attempt must still be history");
    assert_eq!(
        records[0].status,
        JobStatus::Denied {
            reason: DeniedReason::CwdEscape
        }
    );
}

/// A permitted attempt that cannot be started is recorded as Failed, since the
/// caller never gets an id to await.
#[test]
fn unstartable_attempt_is_recorded_as_failed() {
    let reg = JobRegistry::ungoverned().unwrap();
    let err = reg
        .start(JobRequest::new(
            ["definitely-not-a-real-program-xyz"],
            root(),
        ))
        .unwrap_err();
    assert!(matches!(err, JobError::Spawn { .. }), "got {err:?}");

    let records = reg.audit().records().unwrap();
    assert_eq!(records.len(), 1);
    assert!(
        matches!(records[0].status, JobStatus::Failed { .. }),
        "got {:?}",
        records[0].status
    );
}

/// A job id is node-unique, so two registries sharing one log file produce
/// records that are distinguishable **by the id itself** — no second
/// discriminator field is needed.
///
/// This used to assert the opposite: ids were per-registry counters, so both
/// records claimed "job 0" and a separate `registry` field told them apart.
/// Making the id node-unique removed the need for that field, and with it the
/// second answer to "what identifies a job".
///
/// implements: registry-is-node-state-on-disk
#[test]
fn records_from_two_registries_carry_distinct_job_ids() {
    let path = std::env::temp_dir().join(format!("moco-audit-reg-{}.log", std::process::id()));
    let _ = std::fs::remove_file(&path);

    for _ in 0..2 {
        let reg = JobRegistry::with_policy(policy())
            .unwrap()
            .with_audit(Arc::new(FileAuditLog::new(&path)));
        reg.run(JobRequest::new(["echo", "ok"], root())).unwrap();
    }

    let records = FileAuditLog::new(&path).records().unwrap();
    assert_eq!(records.len(), 2);
    assert_ne!(
        records[0].job, records[1].job,
        "two registries must never issue the same job id"
    );

    let _ = std::fs::remove_file(&path);
}

/// `records_since` returns only the tail, so polling a growing log is cheap.
#[test]
fn records_since_returns_only_the_tail() {
    let reg = governed();
    reg.run(JobRequest::new(["echo", "ok"], root())).unwrap();
    let seen = reg.audit().records().unwrap().len();

    reg.run(JobRequest::new(["echo", "ok"], root())).unwrap();
    reg.start(JobRequest::new(["echo", "nope"], root()))
        .unwrap();

    let tail = reg.audit().records_since(seen).unwrap();
    assert_eq!(tail.len(), 2, "only the records added since");
    assert_eq!(tail[1].verdict, Verdict::SeedDeny);
}
