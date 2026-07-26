//! An audit write must never be attempted while the registry lock is held.
//!
//! `flush` re-locks the registry to release the claim when a write fails. Any
//! path that flushes *under* the lock therefore deadlocks the whole registry on
//! the first audit failure — not just that call, but every later `start`,
//! `tail`, `wait` and `kill` too, because the mutex is never released.
//!
//! These tests use a sink that always fails, and bound every operation with a
//! timeout: a deadlock shows up as a timeout rather than as a test that hangs
//! forever. Each one fails against the previous implementation, where `start`,
//! `decide` and `kill` all flushed inline.
//!
//! implements: audit-every-attempt

use std::sync::Arc;
use std::sync::mpsc;
use std::time::Duration;

use moco_job::{AuditRecord, AuditSink, Decision, JobError, JobRegistry, JobRequest, NodePolicy};
use moco_job::{RuleSet, SeedConfig};

/// Long enough that a slow machine does not report a false deadlock, short
/// enough that a real one does not stall the suite.
const BUDGET: Duration = Duration::from_secs(10);

struct FailingAuditLog;

impl AuditSink for FailingAuditLog {
    fn append(&self, _record: AuditRecord) -> Result<(), JobError> {
        Err(JobError::Audit("sink is down".into()))
    }
    fn records(&self) -> Result<Vec<AuditRecord>, JobError> {
        Ok(Vec::new())
    }
}

fn argv(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|s| (*s).to_string()).collect()
}

fn root() -> std::path::PathBuf {
    std::env::temp_dir()
}

#[allow(
    clippy::expect_used,
    reason = "test helper: a failure is a broken harness"
)]
fn governed() -> JobRegistry {
    let rules = RuleSet::from_seed(SeedConfig {
        allow: vec![argv(&["echo", "ok"])],
        deny: vec![argv(&["echo", "nope"])],
    });
    JobRegistry::with_policy(NodePolicy::new(rules, root()).with_approval_timeout(BUDGET))
        .expect("registry")
        .with_audit(Arc::new(FailingAuditLog))
}

/// Run `f` on its own thread and fail if it does not finish inside the budget.
///
/// A deadlocked registry leaves that thread parked on the mutex forever, so the
/// timeout is the observation. The thread is deliberately left running rather
/// than joined: it can never finish, and the suite must still exit.
fn within<F>(what: &str, f: F)
where
    F: FnOnce() + Send + 'static,
{
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        f();
        let _ = tx.send(());
    });
    assert!(
        rx.recv_timeout(BUDGET).is_ok(),
        "{what} did not finish within {BUDGET:?} — the registry lock is almost \
         certainly held across the audit write"
    );
}

/// A denied start fails the audit write, and must still return.
#[test]
fn a_failing_audit_on_start_does_not_deadlock() {
    within("start of a denied job", || {
        let reg = governed();
        // Denied by seed rule: terminal at start, so it audits immediately.
        let _ = reg.start(JobRequest::new(["echo", "nope"], root()));
    });
}

/// Killing a job that is awaiting approval audits the withdrawal.
#[test]
fn a_failing_audit_on_kill_does_not_deadlock() {
    within("kill of a pending job", || {
        let reg = governed();
        // Unmatched argv: parks in `pending`, so a kill withdraws and audits.
        let Ok(id) = reg.start(JobRequest::new(["echo", "unlisted"], root())) else {
            return;
        };
        let _ = reg.kill(&id);
    });
}

/// Deciding against a pending job audits the rejection.
#[test]
fn a_failing_audit_on_decide_does_not_deadlock() {
    within("decide on a pending job", || {
        let reg = governed();
        let Ok(id) = reg.start(JobRequest::new(["echo", "unlisted"], root())) else {
            return;
        };
        let _ = reg.decide(&id, Decision::DenyOnce);
    });
}

/// The registry is still usable after an audit failure — the lock was released,
/// not poisoned or held. This is the part that matters: a deadlock does not stop
/// one call, it stops the daemon.
#[test]
fn the_registry_still_works_after_a_failed_audit() {
    within("a second call after an audit failure", || {
        let reg = governed();
        let _ = reg.start(JobRequest::new(["echo", "nope"], root()));
        // Any later call would block forever on a held lock.
        let _ = reg.list();
        let _ = reg.start(JobRequest::new(["echo", "nope"], root()));
    });
}
