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
    let confined = root().join("moco-vouched-confined");
    std::fs::create_dir_all(&confined).unwrap();
    let reg = JobRegistry::with_policy(NodePolicy::new(
        RuleSet::from_seed(SeedConfig::default()),
        &confined,
    ))
    .unwrap();
    assert!(
        reg.start_vouched(JobRequest::new(["echo", "x"], &confined))
            .is_ok()
    );
    assert!(matches!(
        reg.start_vouched(JobRequest::new(["echo", "x"], "/")),
        Err(JobError::CwdEscape { .. })
    ));
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

/// The wire's own start stays gated: an environment is still refused there.
#[test]
fn the_wire_start_still_refuses_an_environment() {
    let reg = governed();
    let request = wire::encode(&wire::StartRequest {
        argv: vec!["echo".into(), "x".into()],
        cwd: root().to_string_lossy().into_owned(),
        deadline_ms: 0,
        caller: wire::WireCaller::Console,
        env: vec![wire::EnvVar {
            name: "KUDO_PROBE".into(),
            value: "x".into(),
        }],
        human_view: moco_job::lens::HumanView::Logs,
    })
    .unwrap();
    assert!(matches!(
        wire::dispatch(&reg, "start", &request),
        Err(wire::WireError::Job(JobError::EnvUnderGovernance))
    ));
}

/// A vouched start may not ask for a name or a restart policy: the engine's
/// respawn would rebuild it through the gate, without its environment.
#[test]
fn a_vouched_start_carries_no_declaration() {
    let reg = governed();
    let named = JobRequest::new(["echo", "x"], root()).named("pane");
    assert!(matches!(
        reg.start_vouched(named),
        Err(JobError::VouchedDeclaration)
    ));
    let restarting = JobRequest::new(["echo", "x"], root()).with_lifecycle(
        moco_job::Lifetime::default(),
        moco_job::RestartPolicy::Always,
    );
    assert!(matches!(
        reg.start_vouched(restarting),
        Err(JobError::VouchedDeclaration)
    ));
}
