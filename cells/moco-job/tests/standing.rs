//! The standing verdicts — *allow always* and *deny always*.
//!
//! Each writes an exact-argv rule to the node's own overlay, on top of the
//! committed seed, then acts as its *once* twin. The overlay is node state on
//! disk: a restarted registry reads it back.
//!
//! implements: four-verdicts-exact-argv
//! implements: rule-set-target-owned-human-mutated
//! implements: registry-is-node-state-on-disk

use moco_job::{
    Decision, DeniedReason, JobRegistry, JobRequest, JobStatus, NodePolicy, RuleSet, SeedConfig,
    Verdict,
};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

static SEQ: AtomicUsize = AtomicUsize::new(0);

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
    NodePolicy::new(rules, root()).with_approval_timeout(Duration::from_secs(5))
}

/// A fresh registry directory, so a restart can be simulated by opening it
/// again.
fn state_dir() -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "moco-standing-{}-{}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

#[allow(
    clippy::unwrap_used,
    reason = "test helper: a failure is a broken harness"
)]
fn governed_in(dir: &PathBuf) -> JobRegistry {
    JobRegistry::with_policy(policy())
        .unwrap()
        .with_dir(dir)
        .unwrap()
}

/// Allow always runs this job, and the same exact argv later runs with no
/// pending state, under the standing rule.
#[test]
fn allow_always_runs_and_admits_the_same_argv_next_time() {
    let reg = governed_in(&state_dir());
    let id = reg
        .start(JobRequest::new(["echo", "recurring"], root()))
        .unwrap();
    reg.decide(&id, Decision::allow_always()).unwrap();
    let first = reg.wait(&id).unwrap();
    assert_eq!(first.status, JobStatus::Done { code: 0 });
    assert_eq!(first.verdict, Verdict::ApprovedAlways);

    let again = reg
        .start(JobRequest::new(["echo", "recurring"], root()))
        .unwrap();
    let second = reg.wait(&again).unwrap();
    assert_eq!(second.status, JobStatus::Done { code: 0 });
    assert_eq!(second.verdict, Verdict::StandingAllow);
}

/// A standing rule is exact: another argument still asks.
#[test]
fn a_standing_allow_is_exact_argv() {
    let reg = governed_in(&state_dir());
    let id = reg
        .start(JobRequest::new(["echo", "recurring"], root()))
        .unwrap();
    reg.decide(&id, Decision::allow_always()).unwrap();
    reg.wait(&id).unwrap();

    let other = reg
        .start(JobRequest::new(["echo", "recurring", "more"], root()))
        .unwrap();
    assert_eq!(
        reg.tail(&other, 0).unwrap().status,
        JobStatus::PendingApproval
    );
    reg.decide(&other, Decision::DenyOnce).unwrap();
}

/// Deny always rejects this job, and the same exact argv later never spawns.
#[test]
fn deny_always_rejects_and_refuses_the_same_argv_next_time() {
    let reg = governed_in(&state_dir());
    let id = reg
        .start(JobRequest::new(["echo", "never"], root()))
        .unwrap();
    reg.decide(&id, Decision::DenyAlways).unwrap();
    let first = reg.wait(&id).unwrap();
    assert_eq!(
        first.status,
        JobStatus::Denied {
            reason: DeniedReason::Decision
        }
    );
    assert_eq!(first.verdict, Verdict::RejectedAlways);

    let again = reg
        .start(JobRequest::new(["echo", "never"], root()))
        .unwrap();
    let second = reg.wait(&again).unwrap();
    assert_eq!(
        second.status,
        JobStatus::Denied {
            reason: DeniedReason::Rule
        }
    );
    assert_eq!(second.verdict, Verdict::StandingDeny);
}

/// An edited allow-always grants the corrected argv — what actually ran — and
/// not the proposal.
#[test]
fn edited_allow_always_grants_the_correction() {
    let reg = governed_in(&state_dir());
    let id = reg
        .start(JobRequest::new(["echo", "wrong"], root()))
        .unwrap();
    reg.decide(&id, Decision::allow_always_edited(["echo", "right"]))
        .unwrap();
    reg.wait(&id).unwrap();

    let right = reg
        .start(JobRequest::new(["echo", "right"], root()))
        .unwrap();
    assert_eq!(reg.wait(&right).unwrap().verdict, Verdict::StandingAllow);

    let wrong = reg
        .start(JobRequest::new(["echo", "wrong"], root()))
        .unwrap();
    assert_eq!(
        reg.tail(&wrong, 0).unwrap().status,
        JobStatus::PendingApproval
    );
    reg.decide(&wrong, Decision::DenyOnce).unwrap();
}

/// An allow-always edited into a seed-denied argv is denied, and writes no
/// rule: a standing grant never launders a standing prohibition.
#[test]
fn allow_always_never_launders_a_seed_deny() {
    let reg = governed_in(&state_dir());
    let id = reg
        .start(JobRequest::new(["echo", "unlisted"], root()))
        .unwrap();
    reg.decide(&id, Decision::allow_always_edited(["echo", "nope"]))
        .unwrap();
    assert_eq!(
        reg.wait(&id).unwrap().status,
        JobStatus::Denied {
            reason: DeniedReason::Rule
        }
    );
    assert!(
        !reg.standing().allow_rules().contains(&argv(&["echo", "nope"])),
        "no allow rule may be written for a seed-denied argv"
    );
}

/// The overlay is node state: a registry opened on the same directory after a
/// restart applies the standing rules without asking.
#[test]
fn standing_rules_survive_a_restart() {
    let dir = state_dir();
    {
        let reg = governed_in(&dir);
        let allow = reg
            .start(JobRequest::new(["echo", "kept"], root()))
            .unwrap();
        reg.decide(&allow, Decision::allow_always()).unwrap();
        reg.wait(&allow).unwrap();
        let deny = reg
            .start(JobRequest::new(["echo", "barred"], root()))
            .unwrap();
        reg.decide(&deny, Decision::DenyAlways).unwrap();
        reg.wait(&deny).unwrap();
    }

    let reg = governed_in(&dir);
    let kept = reg
        .start(JobRequest::new(["echo", "kept"], root()))
        .unwrap();
    assert_eq!(reg.wait(&kept).unwrap().verdict, Verdict::StandingAllow);
    let barred = reg
        .start(JobRequest::new(["echo", "barred"], root()))
        .unwrap();
    assert_eq!(reg.wait(&barred).unwrap().verdict, Verdict::StandingDeny);
}

/// An argv element that spells a boolean round-trips through the overlay
/// file (the format reads a bare `true` as a bool).
#[test]
fn an_argv_spelling_a_boolean_round_trips() {
    let dir = state_dir();
    {
        let reg = governed_in(&dir);
        let id = reg
            .start(JobRequest::new(["/bin/true", "true"], root()))
            .unwrap();
        reg.decide(&id, Decision::allow_always()).unwrap();
        reg.wait(&id).unwrap();
    }
    let reg = governed_in(&dir);
    assert!(
        reg.standing()
            .allow_rules()
            .contains(&argv(&["/bin/true", "true"])),
        "got {:?}",
        reg.standing().allow_rules()
    );
}

/// The overlay file is owner-only.
#[test]
fn the_overlay_file_is_owner_only() {
    use std::os::unix::fs::PermissionsExt;
    let dir = state_dir();
    let reg = governed_in(&dir);
    let id = reg
        .start(JobRequest::new(["echo", "private"], root()))
        .unwrap();
    reg.decide(&id, Decision::allow_always()).unwrap();
    reg.wait(&id).unwrap();

    let mode = std::fs::metadata(dir.join("standing-rules.styx"))
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o077, 0, "mode {mode:o}");
}

/// A standing rule written by one verdict leaves the seed untouched: the
/// policy's committed rules read exactly as configured.
#[test]
fn the_seed_is_never_written() {
    let reg = governed_in(&state_dir());
    let id = reg
        .start(JobRequest::new(["echo", "recurring"], root()))
        .unwrap();
    reg.decide(&id, Decision::allow_always()).unwrap();
    reg.wait(&id).unwrap();
    assert_eq!(
        reg.policy().map(|p| p.rules().allow_rules().to_vec()),
        Some(vec![argv(&["echo", "ok"])])
    );
}
