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

/// The declared verbs are reachable over the wire, and carry the caller so the
/// engine can scope the write.
#[test]
fn the_declared_verbs_round_trip_over_bytes() {
    let ws = std::env::temp_dir().join(format!("moco-wire-decl-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&ws);
    std::fs::create_dir_all(ws.join(".git")).expect("repo");
    std::fs::write(
        ws.join(moco_job::MANIFEST_FILE),
        r#"proc ({name greet, argv (echo hi), autostart @Session})"#,
    )
    .expect("manifest");
    let ws = ws.canonicalize().expect("canonicalize");

    let reg = registry();
    let caller = wire::WireCaller::Session {
        cwd: ws.to_string_lossy().into_owned(),
    };

    // ensure starts the session entry…
    let payload = wire::encode(&wire::EnsureRequest {
        caller: caller.clone(),
    })
    .expect("encode");
    let reply: wire::EnsureReply =
        wire::decode(&wire::dispatch(&reg, "ensure", &payload).expect("ensure")).expect("decode");
    assert_eq!(reply.started.len(), 1);

    // …the listing shows its declaration name…
    let listing: wire::ListReply =
        wire::decode(&wire::dispatch(&reg, "list", b"{}").expect("list")).expect("decode");
    assert!(listing.jobs.iter().any(|j| j.name == "greet"));

    // …and start_named runs it again by name.
    let payload = wire::encode(&wire::StartNamedRequest {
        name: "greet".into(),
        caller: caller.clone(),
    })
    .expect("encode");
    let started: wire::StartReply =
        wire::decode(&wire::dispatch(&reg, "start_named", &payload).expect("start_named"))
            .expect("decode");
    assert!(!started.id.is_empty());

    // clear takes the tombstones once they are terminal.
    for _ in 0..100 {
        let _ = wire::dispatch(&reg, "list", b"{}");
        std::thread::sleep(std::time::Duration::from_millis(10));
        let listing: wire::ListReply =
            wire::decode(&wire::dispatch(&reg, "list", b"{}").expect("list")).expect("decode");
        if listing.jobs.iter().all(|j| j.status.is_terminal()) {
            break;
        }
    }
    let payload = wire::encode(&wire::ClearRequest { caller }).expect("encode");
    let cleared: wire::ClearReply =
        wire::decode(&wire::dispatch(&reg, "clear", &payload).expect("clear")).expect("decode");
    assert!(cleared.removed >= 1, "tombstones should have been taken");

    let _ = std::fs::remove_dir_all(&ws);
}

/// `adopt` is not on the wire: handing the supervisor an arbitrary pid is a
/// node-level act, not something an agent asks for.
#[test]
fn adopt_is_not_reachable_over_the_wire() {
    let reg = registry();
    let err = wire::dispatch(&reg, "adopt", b"{}").expect_err("must not be dispatchable");
    assert!(err.to_string().contains("unknown method"), "got: {err}");
}

/// Resource readings cross the wire, and a caller can tell "not sampled yet"
/// from "idle" — the reply carries no samples rather than a fabricated zero.
#[test]
#[allow(clippy::expect_used, reason = "a failure here is a broken harness")]
fn stats_round_trip_over_bytes() {
    let reg = registry();

    let request = wire::encode(&StartRequest {
        argv: vec!["sleep".into(), "5".into()],
        cwd: root().to_string_lossy().into_owned(),
        deadline_ms: 0,
        caller: wire::WireCaller::Console,
    })
    .expect("encode start");
    let reply = wire::dispatch(&reg, "start", &request).expect("start dispatches");
    let started: wire::StartReply = wire::decode(&reply).expect("decode start reply");

    let ask = wire::encode(&wire::StatsRequest {
        id: started.id.clone(),
    })
    .expect("encode stats");

    // Before any sampling has happened there is nothing to report.
    let reply = wire::dispatch(&reg, "stats", &ask).expect("stats dispatches");
    let stats: wire::StatsReply = wire::decode(&reply).expect("decode stats reply");
    assert!(stats.samples.is_empty());
    assert!(!stats.breach.any());

    reg.sample_all();
    let reply = wire::dispatch(&reg, "stats", &ask).expect("stats dispatches");
    let stats: wire::StatsReply = wire::decode(&reply).expect("decode stats reply");
    let latest = stats.samples.last().expect("one sample after sampling");
    assert!(latest.rss_bytes > 0, "a live process occupies memory");

    let _ = reg.kill(&moco_job::JobId(started.id), &moco_job::Caller::Console);
}

/// **A multi-line payload survives the wire.**
///
/// A `String` containing newlines is encoded as a heredoc and comes back empty,
/// so the screen — which is newlines by definition — arrived blank while
/// reporting itself as observed. Every payload on this wire is bytes for this
/// reason; this test is here so the next one is too.
#[test]
#[allow(clippy::expect_used, reason = "a failure here is a broken harness")]
fn a_multi_line_screen_survives_the_wire() {
    let reply = wire::ScreenReply {
        source: moco_job::ScreenSource::Live,
        rows: 40,
        cols: 120,
        bytes: b"DASHBOARD\n\n  status: green".to_vec(),
    };

    let encoded = wire::encode(&reply).expect("encode");
    let back: wire::ScreenReply = wire::decode(&encoded).expect("decode");

    assert_eq!(
        back.bytes, reply.bytes,
        "the screen must arrive as it was rendered"
    );
}

/// The same hazard, driven through a real dispatch rather than a literal.
#[test]
#[allow(clippy::expect_used, reason = "a failure here is a broken harness")]
fn a_screen_read_over_the_wire_is_not_blank() {
    let reg = registry();

    let request = wire::encode(&StartRequest {
        argv: vec![
            "sh".into(),
            "-c".into(),
            "printf 'line-one\\nline-two'; sleep 5".into(),
        ],
        cwd: root().to_string_lossy().into_owned(),
        deadline_ms: 0,
        caller: wire::WireCaller::Console,
    })
    .expect("encode start");
    let reply = wire::dispatch(&reg, "start", &request).expect("start dispatches");
    let started: wire::StartReply = wire::decode(&reply).expect("decode start reply");

    let ask = wire::encode(&wire::ScreenRequest {
        id: started.id.clone(),
    })
    .expect("encode screen");

    let mut text = String::new();
    for _ in 0..100 {
        let reply = wire::dispatch(&reg, "screen", &ask).expect("screen dispatches");
        let screen: wire::ScreenReply = wire::decode(&reply).expect("decode screen reply");
        text = String::from_utf8_lossy(&screen.bytes).into_owned();
        if text.contains("line-two") {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }

    assert!(text.contains("line-one"), "got: {text:?}");
    assert!(
        text.contains("line-two"),
        "both lines must arrive, not just the first: {text:?}"
    );

    let _ = reg.kill(&moco_job::JobId(started.id), &moco_job::Caller::Console);
}
