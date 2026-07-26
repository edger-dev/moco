//! The job connector's wire surface — byte dispatch owned by the engine.
//!
//! These tests drive the substrate the way a remote caller will: encode a
//! request to bytes, hand it to `dispatch`, decode the reply. No transport is
//! involved, which is the point — the engine's remote surface must be provable
//! without one.
//!
//! implements: job-connector-is-engine-owned-byte-dispatch

use std::path::PathBuf;

use moco_job::wire::{self, StartRequest, TailRequest, WaitRequest};
use moco_job::{JobRegistry, JobStatus};

fn root() -> PathBuf {
    std::env::temp_dir()
}

// A construction failure here means the test environment cannot create a
// registry directory at all — a broken harness, not a behaviour under test.
#[allow(
    clippy::expect_used,
    reason = "test helper: a failure is a broken harness"
)]
fn registry() -> JobRegistry {
    JobRegistry::ungoverned().expect("registry")
}

/// A start/wait round trip over bytes, with no transport in sight.
#[test]
fn start_and_wait_round_trip_over_bytes() {
    let reg = registry();

    let request = wire::encode(&StartRequest {
        argv: vec!["echo".into(), "hello".into()],
        cwd: root().to_string_lossy().into_owned(),
        deadline_ms: 0,
        caller: wire::WireCaller::Console,
    })
    .expect("encode start");

    let reply = wire::dispatch(&reg, "start", &request).expect("start dispatches");
    let started: wire::StartReply = wire::decode(&reply).expect("decode start reply");
    assert!(
        !started.id.is_empty(),
        "a started job must come back with an id"
    );

    let request = wire::encode(&WaitRequest {
        id: started.id.clone(),
    })
    .expect("encode wait");
    let reply = wire::dispatch(&reg, "wait", &request).expect("wait dispatches");
    let outcome: wire::WaitReply = wire::decode(&reply).expect("decode wait reply");

    assert_eq!(outcome.status, JobStatus::Done { code: 0 });
}

/// Output comes back byte-exact, so a caller reading scrollback sees what the
/// process actually wrote.
#[test]
fn tail_returns_the_bytes_the_process_wrote() {
    let reg = registry();

    let request = wire::encode(&StartRequest {
        argv: vec!["echo".into(), "hello".into()],
        cwd: root().to_string_lossy().into_owned(),
        deadline_ms: 0,
        caller: wire::WireCaller::Console,
    })
    .expect("encode");
    let started: wire::StartReply =
        wire::decode(&wire::dispatch(&reg, "start", &request).expect("start")).expect("decode");

    let wait = wire::encode(&WaitRequest {
        id: started.id.clone(),
    })
    .expect("encode wait");
    wire::dispatch(&reg, "wait", &wait).expect("wait");

    let request = wire::encode(&TailRequest {
        id: started.id.clone(),
        offset: 0,
    })
    .expect("encode tail");
    let reply = wire::dispatch(&reg, "tail", &request).expect("tail dispatches");
    let tail: wire::TailReply = wire::decode(&reply).expect("decode tail reply");

    assert_eq!(
        String::from_utf8_lossy(&tail.bytes).trim(),
        "hello",
        "tail must return exactly what was written"
    );
    assert_eq!(tail.next_offset, tail.bytes.len() as u64);
}

/// `list` answers without any job-specific argument — it is the "what is here"
/// call, and it must work on an empty registry too.
#[test]
fn list_answers_on_an_empty_registry() {
    let reg = registry();
    let reply = wire::dispatch(&reg, "list", b"{}").expect("list dispatches");
    let listing: wire::ListReply = wire::decode(&reply).expect("decode list reply");
    assert!(listing.jobs.is_empty(), "a fresh registry has no jobs");
}

/// **An unknown method is refused, never ignored.** A silent success would let a
/// caller believe work happened.
#[test]
fn an_unknown_method_is_refused_by_name() {
    let reg = registry();
    let err = wire::dispatch(&reg, "definitely-not-a-method", b"{}")
        .expect_err("an unknown method must be refused");
    let message = err.to_string();
    assert!(
        message.contains("definitely-not-a-method"),
        "the refusal must name the method, got: {message}"
    );
}

/// A malformed request is a decode failure that says so, rather than a default
/// request being run. "Absent" and "broken" must not look alike.
#[test]
fn a_malformed_request_is_refused_not_defaulted() {
    let reg = registry();
    let err = wire::dispatch(&reg, "start", b"this is not a styx record")
        .expect_err("a malformed payload must be refused");
    let message = err.to_string();
    assert!(
        message.to_lowercase().contains("decode") || message.to_lowercase().contains("request"),
        "the refusal must say the request could not be read, got: {message}"
    );
}

/// The engine's own refusals survive the trip: a denied or failed start comes
/// back as an error, not as a fabricated success.
#[test]
fn an_unstartable_program_fails_across_the_wire() {
    let reg = registry();
    let request = wire::encode(&StartRequest {
        argv: vec!["definitely-not-a-real-program-xyz".into()],
        cwd: root().to_string_lossy().into_owned(),
        deadline_ms: 0,
        caller: wire::WireCaller::Console,
    })
    .expect("encode");

    let err = wire::dispatch(&reg, "start", &request).expect_err("an unstartable job must fail");
    let message = err.to_string();
    assert!(
        message.contains("definitely-not-a-real-program-xyz"),
        "the failure must name the binary, got: {message}"
    );
}

/// **Polling must be able to observe completion.** A caller that only ever
/// tails — which is what a remote poller does — has to see a finished job stop
/// reporting `Running`, or it waits forever for news that never comes.
#[test]
fn tail_settles_a_finished_job_without_a_separate_wait() {
    let reg = registry();

    let request = wire::encode(&StartRequest {
        argv: vec!["echo".into(), "done".into()],
        cwd: root().to_string_lossy().into_owned(),
        deadline_ms: 0,
        caller: wire::WireCaller::Console,
    })
    .expect("encode");
    let started: wire::StartReply =
        wire::decode(&wire::dispatch(&reg, "start", &request).expect("start")).expect("decode");

    // Never call `wait`. Poll only, exactly as a remote caller would.
    let mut status = JobStatus::Running;
    for _ in 0..200 {
        let request = wire::encode(&TailRequest {
            id: started.id.clone(),
            offset: 0,
        })
        .expect("encode tail");
        let tail: wire::TailReply =
            wire::decode(&wire::dispatch(&reg, "tail", &request).expect("tail")).expect("decode");
        status = tail.status;
        if status != JobStatus::Running {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }

    assert_eq!(
        status,
        JobStatus::Done { code: 0 },
        "tail alone must be enough to see a job finish"
    );
}
