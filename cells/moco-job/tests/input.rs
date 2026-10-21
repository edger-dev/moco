//! A terminal job accepts input: what a person types reaches its pty, under
//! the PTY holder or not; anything else refuses it plainly.
//!
//! implements: a-terminal-job-accepts-input

#![allow(clippy::unwrap_used, clippy::expect_used, reason = "tests")]

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use moco_job::lens::HumanView;
use moco_job::scope::{Caller, Scope};
use moco_job::wire::{self, WireCaller};
use moco_job::{JobError, JobId, JobRegistry, JobRequest, JobStatus};

static SEQ: AtomicU64 = AtomicU64::new(0);

fn dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!(
        "moco-input-{}-{}-{name}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(d.join(".git")).unwrap();
    d.canonicalize().unwrap()
}

fn holder() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_moco-pty-holder"))
}

fn until(mut f: impl FnMut() -> bool) -> bool {
    for _ in 0..100 {
        if f() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    false
}

/// A job that echoes back one line it reads, then lingers.
fn echo_one_line(cwd: PathBuf) -> JobRequest {
    JobRequest::new(["sh", "-c", "read line; echo \"got:$line\"; sleep 2"], cwd)
        .with_human_view(HumanView::Terminal)
}

fn screen_shows(reg: &JobRegistry, id: &JobId, needle: &str) -> bool {
    until(|| {
        reg.screen(id)
            .map(|s| s.text.contains(needle))
            .unwrap_or(false)
    })
}

#[test]
fn typed_input_reaches_a_terminal_job() {
    let reg = JobRegistry::ungoverned().unwrap();
    let id = reg.start(echo_one_line(dir("direct"))).unwrap();
    reg.input(&id, b"hello\n", &Caller::Console).unwrap();
    assert!(
        screen_shows(&reg, &id, "got:hello"),
        "{:?}",
        reg.screen(&id).map(|s| s.text)
    );
}

#[test]
fn typed_input_reaches_a_held_terminal_job() {
    let state = dir("held-state");
    let reg = JobRegistry::ungoverned()
        .unwrap()
        .with_dir(&state)
        .unwrap()
        .with_pty_holder(holder());
    let id = reg.start(echo_one_line(dir("held"))).unwrap();
    reg.input(&id, b"through the holder\n", &Caller::Console)
        .unwrap();
    assert!(
        screen_shows(&reg, &id, "got:through the holder"),
        "{:?}",
        reg.screen(&id).map(|s| s.text)
    );
    let _ = reg.kill(&id, &Caller::Console);
}

#[test]
fn input_to_a_logs_job_is_refused() {
    let reg = JobRegistry::ungoverned().unwrap();
    let id = reg
        .start(JobRequest::new(["sleep", "2"], dir("logs")))
        .unwrap();
    assert!(matches!(
        reg.input(&id, b"x\n", &Caller::Console),
        Err(JobError::InputRefused { .. })
    ));
    let _ = reg.kill(&id, &Caller::Console);
}

#[test]
fn input_to_a_finished_job_is_refused() {
    let reg = JobRegistry::ungoverned().unwrap();
    let id = reg
        .start(JobRequest::new(["true"], dir("done")).with_human_view(HumanView::Terminal))
        .unwrap();
    assert!(until(
        || matches!(reg.wait(&id), Ok(o) if matches!(o.status, JobStatus::Done { .. }))
    ));
    assert!(matches!(
        reg.input(&id, b"x\n", &Caller::Console),
        Err(JobError::InputRefused { .. })
    ));
}

#[test]
fn input_to_another_workspaces_job_is_refused() {
    let reg = JobRegistry::ungoverned().unwrap();
    let theirs = dir("theirs");
    let id = reg
        .start(echo_one_line(theirs.clone()).in_scope(Scope::resolve(&theirs)))
        .unwrap();
    let mine = Caller::Scoped(Scope::resolve(dir("mine")));
    assert!(matches!(
        reg.input(&id, b"x\n", &mine),
        Err(JobError::ForeignWorkspace { .. })
    ));
    let _ = reg.kill(&id, &Caller::Console);
}

#[test]
fn input_crosses_the_wire() {
    let reg = JobRegistry::ungoverned().unwrap();
    let id = reg.start(echo_one_line(dir("wire"))).unwrap();
    let request = wire::encode(&wire::InputRequest {
        id: id.0.clone(),
        bytes: b"over the wire\n".to_vec(),
        caller: WireCaller::Console,
    })
    .unwrap();
    wire::dispatch(&reg, "input", &request).unwrap();
    assert!(screen_shows(&reg, &id, "got:over the wire"));
    assert!(wire::METHODS.contains(&"input"));
}
