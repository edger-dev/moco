//! A start the hosting daemon vouches for: composed by the daemon from values
//! it checked, so no rule and no human admits it — and the audit says so.
//!
//! implements: a-hosting-daemon-may-vouch-for-a-start

use std::path::PathBuf;
use std::time::Duration;

use moco_job::{
    JobError, JobRegistry, JobRequest, JobStatus, NodePolicy, RuleSet, SeedConfig, Verdict, wire,
};

fn root() -> PathBuf {
    std::env::temp_dir()
}

#[allow(
    clippy::expect_used,
    reason = "test helper: a failure is a broken harness"
)]
fn governed() -> JobRegistry {
    JobRegistry::with_policy(
        NodePolicy::new(RuleSet::from_seed(SeedConfig::default()), root())
            .with_approval_timeout(Duration::from_secs(5)),
    )
    .expect("registry")
}

/// An argv no rule allows runs at once, with no pending state, audited as
/// vouched.
#[test]
fn a_vouched_start_runs_without_the_gate() {
    let reg = governed();
    let id = reg
        .start_vouched(JobRequest::new(["echo", "composed"], root()))
        .unwrap();
    let outcome = reg.wait(&id).unwrap();
    assert_eq!(outcome.status, JobStatus::Done { code: 0 });
    assert_eq!(outcome.verdict, Verdict::Vouched);
}

/// The environment the daemon composed reaches the job on a governed node.
#[test]
fn a_vouched_start_carries_its_environment() {
    let reg = governed();
    let job = JobRequest::new(["sh", "-c", "echo $KUDO_PROBE"], root())
        .with_env(vec![("KUDO_PROBE".into(), "vouched-env".into())]);
    let id = reg.start_vouched(job).unwrap();
    reg.wait(&id).unwrap();
    let tail = reg.tail(&id, 0).unwrap();
    assert!(String::from_utf8_lossy(&tail.bytes).contains("vouched-env"));
}

/// The gated start still refuses the same request: vouching is the only way
/// past, and only in-process.
#[test]
fn the_gated_start_still_refuses_an_environment() {
    let reg = governed();
    let job =
        JobRequest::new(["echo", "x"], root()).with_env(vec![("KUDO_PROBE".into(), "x".into())]);
    assert!(matches!(reg.start(job), Err(JobError::EnvUnderGovernance)));
}

/// Vouching skips the gate, not the confinement.
#[test]
fn a_vouched_start_is_still_confined() {
    let reg = JobRegistry::with_policy(NodePolicy::new(
        RuleSet::from_seed(SeedConfig::default()),
        root().join("moco-vouched-confined-nowhere"),
    ))
    .unwrap();
    assert!(
        reg.start_vouched(JobRequest::new(["echo", "x"], "/"))
            .is_err()
    );
}

/// No remote caller can vouch: it is not on the connector.
#[test]
fn vouching_is_not_on_the_wire() {
    assert!(!wire::METHODS.iter().any(|m| m.contains("vouch")));
    let reg = governed();
    assert!(matches!(
        wire::dispatch(&reg, "start_vouched", b"{}"),
        Err(wire::WireError::UnknownMethod(_))
    ));
}
