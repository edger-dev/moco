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
        Err(JobError::InputRefused { .. })
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

/// Typing is the human's intervention: a session may not type, even into a
/// job of its own workspace — it could type past the approval its argv got.
#[test]
fn a_session_may_not_type_even_into_its_own_job() {
    let reg = JobRegistry::ungoverned().unwrap();
    let ws = dir("own");
    let id = reg
        .start(echo_one_line(ws.clone()).in_scope(Scope::resolve(&ws)))
        .unwrap();
    let owner = Caller::Scoped(Scope::resolve(&ws));
    assert!(matches!(
        reg.input(&id, b"x\n", &owner),
        Err(JobError::InputRefused { .. })
    ));
    let _ = reg.kill(&id, &Caller::Console);
}

#[test]
fn an_input_larger_than_one_write_is_refused() {
    let reg = JobRegistry::ungoverned().unwrap();
    let id = reg.start(echo_one_line(dir("large"))).unwrap();
    let big = vec![b'x'; moco_job::MAX_INPUT + 1];
    assert!(matches!(
        reg.input(&id, &big, &Caller::Console),
        Err(JobError::InputRefused { .. })
    ));
    let _ = reg.kill(&id, &Caller::Console);
}

/// A job that is not reading its input (a TUI in raw mode, a paused program)
/// fills its terminal; input is then refused promptly, and nothing else on
/// the node waits on it.
#[test]
fn a_job_not_reading_its_input_never_freezes_the_registry() {
    let reg = std::sync::Arc::new(JobRegistry::ungoverned().unwrap());
    let id = reg
        .start(
            JobRequest::new(["sh", "-c", "stty raw -echo; sleep 8"], dir("raw"))
                .with_human_view(HumanView::Terminal),
        )
        .unwrap();
    std::thread::sleep(Duration::from_millis(200));
    let chunk = vec![b'y'; moco_job::MAX_INPUT];
    let started = std::time::Instant::now();
    let mut refused = false;
    for _ in 0..256 {
        if let Err(JobError::InputRefused { .. }) = reg.input(&id, &chunk, &Caller::Console) {
            refused = true;
            break;
        }
    }
    assert!(
        refused,
        "a terminal nobody reads fills, and input is then refused"
    );
    assert!(
        started.elapsed() < Duration::from_secs(4),
        "{:?}",
        started.elapsed()
    );
    let lister = reg.clone();
    let listed = std::thread::spawn(move || lister.list().len());
    std::thread::sleep(Duration::from_millis(500));
    assert!(
        listed.is_finished(),
        "the registry answers while input is pending"
    );
    let _ = reg.kill(&id, &Caller::Console);
}

/// The holder outlives the daemon, and so does the way in: a held job takes
/// input after a restart.
#[test]
fn a_held_job_takes_input_after_a_daemon_restart() {
    let state = dir("restart-state");
    let id = {
        let reg = JobRegistry::ungoverned()
            .unwrap()
            .with_dir(&state)
            .unwrap()
            .with_pty_holder(holder());
        let id = reg.start(echo_one_line(dir("restart"))).unwrap();
        assert!(until(|| reg.screen(&id).is_ok()));
        id
    };
    let reborn = JobRegistry::ungoverned()
        .unwrap()
        .with_dir(&state)
        .unwrap()
        .with_pty_holder(holder());
    reborn
        .input(&id, b"after the restart\n", &Caller::Console)
        .unwrap();
    assert!(
        screen_shows(&reborn, &id, "got:after the restart"),
        "{:?}",
        reborn.screen(&id).map(|s| s.text)
    );
    let _ = reborn.kill(&id, &Caller::Console);
}

/// Only a FIFO the holder made is a way in: a plain file at that path is
/// refused, never appended to.
#[test]
fn a_plain_file_where_the_fifo_should_be_is_refused() {
    let state = dir("fake-state");
    let reg = JobRegistry::ungoverned()
        .unwrap()
        .with_dir(&state)
        .unwrap()
        .with_pty_holder(holder());
    let id = reg.start(echo_one_line(dir("fake"))).unwrap();
    assert!(until(|| reg.screen(&id).is_ok()));
    let fifo = std::fs::read_dir(&state)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .find(|p| p.extension().is_some_and(|x| x == "input"))
        .expect("the holder's FIFO");
    std::fs::remove_file(&fifo).unwrap();
    std::fs::write(&fifo, b"").unwrap();
    assert!(matches!(
        reg.input(&id, b"x\n", &Caller::Console),
        Err(JobError::InputRefused { .. })
    ));
    assert_eq!(std::fs::read(&fifo).unwrap(), b"", "nothing appended");
    let _ = reg.kill(&id, &Caller::Console);
}

/// Every input is accounted for in the job's audit record — how many and how
/// many bytes, never what was typed.
#[test]
fn the_audit_record_counts_a_jobs_inputs() {
    let reg = JobRegistry::ungoverned().unwrap();
    let id = reg.start(echo_one_line(dir("audited"))).unwrap();
    reg.input(&id, b"counted\n", &Caller::Console).unwrap();
    assert!(until(
        || matches!(reg.wait(&id), Ok(o) if matches!(o.status, JobStatus::Done { .. }))
    ));
    let record = reg
        .audit()
        .records()
        .unwrap()
        .into_iter()
        .rfind(|r| r.job == id.0)
        .expect("an audit record");
    assert_eq!(record.inputs, 1);
    assert_eq!(record.input_bytes, 8);
}
