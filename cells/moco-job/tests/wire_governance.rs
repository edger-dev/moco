//! The gate over the wire: a human's verdict, a read-only preflight, and the
//! jobs waiting for a verdict.
//!
//! `decide` crosses the wire as take over does — the console's alone — so a
//! human away from the node (on a phone) can answer it.
//!
//! implements: intervention-is-the-only-human-write
//! implements: approval-is-a-job-state
//! implements: four-verdicts-exact-argv
//! implements: agent-self-sufficiency

use std::path::PathBuf;
use std::time::Duration;

use moco_job::wire::{
    self, DecideReply, DecideRequest, PendingReply, PendingRequest, PreflightReply,
    PreflightRequest, StartRequest, WaitRequest, WireCaller, WireVerdict,
};
use moco_job::{Disposition, JobRegistry, JobStatus, NodePolicy, RuleSet, SeedConfig, Verdict};

fn root() -> PathBuf {
    std::env::temp_dir()
}

fn argv(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|s| (*s).to_string()).collect()
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
    JobRegistry::with_policy(
        NodePolicy::new(rules, root()).with_approval_timeout(Duration::from_secs(5)),
    )
    .expect("registry")
}

#[allow(
    clippy::expect_used,
    reason = "test helper: a failure is a broken harness"
)]
fn start(reg: &JobRegistry, parts: &[&str]) -> String {
    let request = wire::encode(&StartRequest {
        argv: argv(parts),
        cwd: root().to_string_lossy().into_owned(),
        deadline_ms: 0,
        caller: WireCaller::Session {
            cwd: root().to_string_lossy().into_owned(),
        },
        env: vec![],
        human_view: moco_job::lens::HumanView::Logs,
    })
    .expect("encode start");
    let reply = wire::dispatch(reg, "start", &request).expect("start");
    let started: wire::StartReply = wire::decode(&reply).expect("decode start");
    started.id
}

#[allow(
    clippy::expect_used,
    reason = "test helper: a failure is a broken harness"
)]
fn wait(reg: &JobRegistry, id: &str) -> wire::WaitReply {
    let request = wire::encode(&WaitRequest { id: id.into() }).expect("encode wait");
    let reply = wire::dispatch(reg, "wait", &request).expect("wait");
    wire::decode(&reply).expect("decode wait")
}

fn decide(
    reg: &JobRegistry,
    id: &str,
    verdict: WireVerdict,
    edited: &[&str],
    caller: WireCaller,
) -> Result<DecideReply, wire::WireError> {
    let request = wire::encode(&DecideRequest {
        id: id.into(),
        verdict,
        edited_argv: argv(edited),
        caller,
    })
    .map_err(|detail| wire::WireError::Request {
        method: "decide".into(),
        detail,
    })?;
    let reply = wire::dispatch(reg, "decide", &request)?;
    wire::decode(&reply).map_err(|detail| wire::WireError::Reply {
        method: "decide".into(),
        detail,
    })
}

#[allow(
    clippy::expect_used,
    reason = "test helper: a failure is a broken harness"
)]
fn pending(reg: &JobRegistry) -> PendingReply {
    let request = wire::encode(&PendingRequest {}).expect("encode pending");
    let reply = wire::dispatch(reg, "pending", &request).expect("pending");
    wire::decode(&reply).expect("decode pending")
}

/// The console approves a pending job over the wire, and it runs.
#[test]
fn the_console_approves_over_the_wire() {
    let reg = governed();
    let id = start(&reg, &["echo", "unlisted"]);
    let reply = decide(&reg, &id, WireVerdict::AllowOnce, &[], WireCaller::Console).unwrap();
    assert_eq!(reply.id, id);
    let outcome = wait(&reg, &id);
    assert_eq!(outcome.status, JobStatus::Done { code: 0 });
    assert_eq!(outcome.verdict, Verdict::ApprovedOnce);
}

/// A session may not answer the gate — not even for a job of its own
/// workspace — and the job stays pending.
#[test]
fn a_session_cannot_decide() {
    let reg = governed();
    let id = start(&reg, &["echo", "unlisted"]);
    let refused = decide(
        &reg,
        &id,
        WireVerdict::AllowOnce,
        &[],
        WireCaller::Session {
            cwd: root().to_string_lossy().into_owned(),
        },
    );
    assert!(refused.is_err(), "a session's verdict must be refused");
    assert_eq!(pending(&reg).jobs.len(), 1, "the job must still be pending");
    decide(&reg, &id, WireVerdict::DenyOnce, &[], WireCaller::Console).unwrap();
}

/// The four verdicts all cross the wire; an "always" one admits the same argv
/// next time.
#[test]
fn allow_always_crosses_the_wire() {
    let reg = governed();
    let id = start(&reg, &["echo", "recurring"]);
    decide(
        &reg,
        &id,
        WireVerdict::AllowAlways,
        &[],
        WireCaller::Console,
    )
    .unwrap();
    assert_eq!(wait(&reg, &id).verdict, Verdict::ApprovedAlways);
    let again = start(&reg, &["echo", "recurring"]);
    assert_eq!(wait(&reg, &again).verdict, Verdict::StandingAllow);
}

#[test]
fn deny_always_crosses_the_wire() {
    let reg = governed();
    let id = start(&reg, &["echo", "never"]);
    decide(&reg, &id, WireVerdict::DenyAlways, &[], WireCaller::Console).unwrap();
    assert_eq!(wait(&reg, &id).verdict, Verdict::RejectedAlways);
    let again = start(&reg, &["echo", "never"]);
    assert_eq!(wait(&reg, &again).verdict, Verdict::StandingDeny);
}

/// Edit-then-allow over the wire runs the correction.
#[test]
fn an_edited_allow_crosses_the_wire() {
    let reg = governed();
    let id = start(&reg, &["echo", "wrong"]);
    decide(
        &reg,
        &id,
        WireVerdict::AllowOnce,
        &["echo", "right"],
        WireCaller::Console,
    )
    .unwrap();
    wait(&reg, &id);
    let tail = reg.tail(&moco_job::JobId(id), 0).unwrap();
    assert!(String::from_utf8_lossy(&tail.bytes).contains("right"));
}

/// The jobs waiting for a verdict, with what a human needs to answer: the
/// argv, where it runs, who asked, and how long is left.
#[test]
fn pending_lists_what_waits_for_a_verdict() {
    let reg = governed();
    assert!(pending(&reg).jobs.is_empty());
    let id = start(&reg, &["echo", "unlisted"]);
    let ran = start(&reg, &["echo", "ok"]);
    wait(&reg, &ran);

    let list = pending(&reg).jobs;
    assert_eq!(list.len(), 1, "only the unmatched job waits: {list:?}");
    let job = &list[0];
    assert_eq!(job.id, id);
    assert_eq!(job.argv, argv(&["echo", "unlisted"]));
    assert!(!job.cwd.is_empty());
    assert!(job.left_ms > 0 && job.left_ms <= 5_000, "{}", job.left_ms);

    decide(&reg, &id, WireVerdict::DenyOnce, &[], WireCaller::Console).unwrap();
    assert!(
        pending(&reg).jobs.is_empty(),
        "a decided job no longer waits"
    );
}

/// Preflight crosses the wire and runs nothing: it says what the gate would
/// do, where the program resolves, and where the job would run.
#[test]
fn preflight_crosses_the_wire() {
    let reg = governed();
    let ask = |parts: &[&str]| -> PreflightReply {
        let request = wire::encode(&PreflightRequest {
            argv: argv(parts),
            cwd: root().to_string_lossy().into_owned(),
        })
        .unwrap();
        wire::decode(&wire::dispatch(&reg, "preflight", &request).unwrap()).unwrap()
    };
    assert_eq!(ask(&["echo", "ok"]).disposition, Disposition::Allow);
    assert_eq!(ask(&["echo", "nope"]).disposition, Disposition::Deny);
    let unlisted = ask(&["echo", "unlisted"]);
    assert_eq!(unlisted.disposition, Disposition::NeedsApproval);
    assert!(unlisted.program.ends_with("echo"), "{}", unlisted.program);
    assert!(!unlisted.resolved_cwd.is_empty());
    assert!(unlisted.cwd_error.is_empty());
    assert!(pending(&reg).jobs.is_empty(), "preflight queues nothing");
}

/// The surface names its new methods.
#[test]
fn the_gate_methods_are_listed() {
    for method in ["decide", "preflight", "pending"] {
        assert!(wire::METHODS.contains(&method), "{method} missing");
    }
}
