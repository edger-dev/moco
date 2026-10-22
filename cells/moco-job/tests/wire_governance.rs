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
use moco_job::{
    DeniedReason, Disposition, JobRegistry, JobStatus, NodePolicy, RuleSet, SeedConfig, Verdict,
};

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
    assert!(
        matches!(
            refused,
            Err(wire::WireError::Job(
                moco_job::JobError::DecisionRefused { .. }
            ))
        ),
        "a session's verdict must be refused as such: {refused:?}"
    );
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

#[allow(
    clippy::expect_used,
    reason = "test helper: a failure is a broken harness"
)]
fn governed_with_timeout(timeout: Duration) -> JobRegistry {
    let rules = RuleSet::from_seed(SeedConfig {
        allow: vec![],
        deny: vec![argv(&["echo", "nope"])],
    });
    JobRegistry::with_policy(NodePolicy::new(rules, root()).with_approval_timeout(timeout))
        .expect("registry")
}

/// A verdict past the window is heard and runs nothing — and the reply says
/// so, rather than reading as an approval.
#[test]
fn a_late_verdict_says_it_ran_nothing() {
    let reg = governed_with_timeout(Duration::from_millis(100));
    let id = start(&reg, &["echo", "late"]);
    std::thread::sleep(Duration::from_millis(200));
    let reply = decide(&reg, &id, WireVerdict::AllowOnce, &[], WireCaller::Console).unwrap();
    assert_eq!(
        reply.status,
        JobStatus::Denied {
            reason: DeniedReason::NoApprover
        }
    );
    assert_eq!(reply.verdict, Verdict::NoApprover);
}

/// An edited allow a deny rule forbids is denied, and the reply says so.
#[test]
fn an_edit_into_a_deny_says_it_was_denied() {
    let reg = governed();
    let id = start(&reg, &["echo", "unlisted"]);
    let reply = decide(
        &reg,
        &id,
        WireVerdict::AllowOnce,
        &["echo", "nope"],
        WireCaller::Console,
    )
    .unwrap();
    assert_eq!(
        reply.status,
        JobStatus::Denied {
            reason: DeniedReason::Rule
        }
    );
    assert_eq!(reply.verdict, Verdict::SeedDeny);
}

/// An approval's reply says the job runs, under which verdict.
#[test]
fn an_approval_reply_names_its_verdict() {
    let reg = governed();
    let id = start(&reg, &["echo", "unlisted"]);
    let reply = decide(&reg, &id, WireVerdict::AllowOnce, &[], WireCaller::Console).unwrap();
    assert_eq!(reply.verdict, Verdict::ApprovedOnce);
    assert!(
        !matches!(
            reply.status,
            JobStatus::Denied { .. } | JobStatus::PendingApproval
        ),
        "{:?}",
        reply.status
    );
}

/// A deny with an edited argv is malformed, refused before the engine.
#[test]
fn a_deny_with_an_edit_is_refused() {
    let reg = governed();
    let id = start(&reg, &["echo", "unlisted"]);
    let refused = decide(
        &reg,
        &id,
        WireVerdict::DenyOnce,
        &["echo", "x"],
        WireCaller::Console,
    );
    assert!(
        matches!(refused, Err(wire::WireError::Request { .. })),
        "{refused:?}"
    );
    assert_eq!(pending(&reg).jobs.len(), 1);
}

/// A decide written without the optional edit still decodes.
#[test]
fn a_decide_without_an_edit_decodes() {
    let reg = governed();
    let id = start(&reg, &["echo", "unlisted"]);
    let bare = format!("{{id \"{id}\", verdict @DenyOnce, caller @Console}}");
    let reply = wire::dispatch(&reg, "decide", bare.as_bytes()).expect("a bare decide");
    let reply: DecideReply = wire::decode(&reply).unwrap();
    assert_eq!(reply.verdict, Verdict::RejectedOnce);
}

/// A job past its window leaves the list — denied, not merely hidden — and
/// the list is oldest first.
#[test]
fn pending_is_oldest_first_and_drops_the_expired() {
    let reg = governed_with_timeout(Duration::from_millis(400));
    let old = start(&reg, &["echo", "old"]);
    std::thread::sleep(Duration::from_millis(50));
    let new = start(&reg, &["echo", "new"]);
    let ids: Vec<String> = pending(&reg).jobs.into_iter().map(|j| j.id).collect();
    assert_eq!(ids, vec![old.clone(), new.clone()]);

    std::thread::sleep(Duration::from_millis(500));
    assert!(pending(&reg).jobs.is_empty());
    assert_eq!(
        reg.tail(&moco_job::JobId(old), 0).unwrap().status,
        JobStatus::Denied {
            reason: DeniedReason::NoApprover
        }
    );
}

/// Preflight names a cwd outside the allowed root.
#[test]
fn preflight_names_a_refused_cwd() {
    let reg = JobRegistry::with_policy(NodePolicy::new(
        RuleSet::from_seed(SeedConfig::default()),
        root().join("moco-confined-nowhere"),
    ))
    .unwrap();
    let request = wire::encode(&PreflightRequest {
        argv: argv(&["echo", "x"]),
        cwd: "/".into(),
    })
    .unwrap();
    let reply: PreflightReply =
        wire::decode(&wire::dispatch(&reg, "preflight", &request).unwrap()).unwrap();
    assert!(reply.resolved_cwd.is_empty());
    assert!(!reply.cwd_error.is_empty());
}

/// A session withdrawing its own pending job is not a human's rejection.
#[test]
fn a_session_withdrawing_is_not_a_human_verdict() {
    let reg = governed();
    let id = start(&reg, &["echo", "unlisted"]);
    let session = moco_job::Caller::Scoped(moco_job::Scope::resolve(root()));
    reg.kill(&moco_job::JobId(id.clone()), &session).unwrap();
    let outcome = wait(&reg, &id);
    assert_eq!(
        outcome.status,
        JobStatus::Denied {
            reason: DeniedReason::Withdrawn
        }
    );
    assert_eq!(outcome.verdict, Verdict::Withdrawn);
}

/// A job pending when the daemon stopped is denied when it comes back: its
/// proposal is gone, so no verdict could run it.
#[test]
fn a_pending_job_is_denied_across_a_restart() {
    let dir = std::env::temp_dir().join(format!("moco-pending-restart-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let id = {
        let reg = governed().with_dir(&dir).unwrap();
        start(&reg, &["echo", "unlisted"])
    };
    let reg = governed().with_dir(&dir).unwrap();
    assert!(pending(&reg).jobs.is_empty());
    assert_eq!(
        reg.tail(&moco_job::JobId(id), 0).unwrap().status,
        JobStatus::Denied {
            reason: DeniedReason::NoApprover
        }
    );
}

#[allow(
    clippy::expect_used,
    reason = "test helper: a failure is a broken harness"
)]
fn listed(reg: &JobRegistry, id: &str) -> wire::JobSummary {
    let listing: wire::ListReply =
        wire::decode(&wire::dispatch(reg, "list", b"{}").expect("list")).expect("decode");
    listing
        .jobs
        .into_iter()
        .find(|j| j.id == id)
        .expect("the job is listed")
}

/// The listing says what each job runs: argv, directory, verdict and how long
/// its phase has lasted — one call for a node's view, not one per job.
///
/// implements: a-job-listing-says-what-each-job-runs
#[test]
fn a_listing_says_what_each_job_runs() {
    let reg = governed();
    let allowed = start(&reg, &["echo", "ok"]);
    let wait = wire::encode(&WaitRequest {
        id: allowed.clone(),
    })
    .expect("encode wait");
    wire::dispatch(&reg, "wait", &wait).expect("wait");
    let waiting = start(&reg, &["echo", "unlisted"]);
    std::thread::sleep(Duration::from_millis(1100));

    let ran = listed(&reg, &allowed);
    assert_eq!(ran.argv, argv(&["echo", "ok"]));
    assert_eq!(ran.verdict, Verdict::SeedAllow);
    assert!(!ran.cwd.is_empty(), "the directory is said");
    assert_eq!(ran.phase_s, 0, "an ended job's clock does not run on");

    let pending = listed(&reg, &waiting);
    assert_eq!(pending.status, JobStatus::PendingApproval);
    assert_eq!(pending.argv, argv(&["echo", "unlisted"]));
    assert!(
        pending.phase_s >= 1,
        "a job waiting a second says so, got {}",
        pending.phase_s
    );
}
