//! The machine lens: a cheap, declared view for an agent to read.
//!
//! implements: dual-lens-human-and-machine

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use moco_job::lens::LensSource;
use moco_job::scope::{Caller, Scope};
use moco_job::{JobRegistry, MANIFEST_FILE};

static SEQ: AtomicU64 = AtomicU64::new(0);

#[allow(
    clippy::expect_used,
    reason = "test helper: a failure is a broken harness"
)]
fn workspace(name: &str, manifest: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "moco-lens-{}-{}-{name}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join(".git")).expect("repo");
    std::fs::write(dir.join(MANIFEST_FILE), manifest).expect("manifest");
    dir.canonicalize().expect("canonicalize")
}

#[allow(
    clippy::expect_used,
    reason = "test helper: a failure is a broken harness"
)]
fn registry() -> JobRegistry {
    JobRegistry::ungoverned().expect("registry")
}

/// **The point of the whole thing.** A job that writes a structured sidecar is
/// read through it, not through its scrollback — which is what makes an agent
/// read cheap enough to be worth doing.
#[test]
fn a_declared_machine_view_is_read_instead_of_scrollback() {
    let ws = workspace(
        "declared",
        r#"proc ({name check,
                 argv (sh -c "echo '{\"errors\":0}' > .diagnostics; echo LOTS OF NOISE"),
                 machine_file ".diagnostics", machine_format "json"})"#,
    );
    let reg = registry();
    let caller = Caller::Scoped(Scope::resolve(&ws));

    let id = reg.start_named("check", &caller).expect("start");
    reg.wait(&id).expect("wait");

    let read = reg.machine(&id, 0).expect("machine read");
    assert_eq!(read.source, LensSource::Machine);
    assert_eq!(read.format, "json");
    let text = String::from_utf8_lossy(&read.bytes);
    assert!(text.contains("errors"), "the sidecar's content: {text:?}");
    assert!(
        !text.contains("NOISE"),
        "the scrollback is exactly what we are avoiding: {text:?}"
    );

    let _ = std::fs::remove_dir_all(&ws);
}

/// **Absent, so fall back — and say so.** A job with no machine lens still
/// answers, with scrollback and an honest label. Silently returning scrollback
/// as if it were structured would be worse than returning nothing.
#[test]
fn without_a_declaration_it_falls_back_and_labels_the_source() {
    let ws = workspace(
        "fallback",
        r#"proc ({name plain, argv (echo just-scrollback)})"#,
    );
    let reg = registry();
    let caller = Caller::Scoped(Scope::resolve(&ws));

    let id = reg.start_named("plain", &caller).expect("start");
    reg.wait(&id).expect("wait");

    let read = reg.machine(&id, 0).expect("machine read");
    assert_eq!(
        read.source,
        LensSource::Scrollback,
        "the caller must be able to tell what it got"
    );
    assert!(
        read.format.is_empty(),
        "there is no declared format to report"
    );
    assert!(String::from_utf8_lossy(&read.bytes).contains("just-scrollback"));

    let _ = std::fs::remove_dir_all(&ws);
}

/// A declared sidecar the process has not written yet reads as empty, **not**
/// as a fallback to scrollback: the lens is declared, so that is the channel,
/// and "nothing yet" is a real answer.
#[test]
fn a_declared_but_missing_sidecar_reads_empty_rather_than_falling_back() {
    let ws = workspace(
        "notyet",
        r#"proc ({name check, argv (echo noise), machine_file ".diagnostics", machine_format "json"})"#,
    );
    let reg = registry();
    let caller = Caller::Scoped(Scope::resolve(&ws));

    let id = reg.start_named("check", &caller).expect("start");
    reg.wait(&id).expect("wait");

    let read = reg.machine(&id, 0).expect("machine read");
    assert_eq!(read.source, LensSource::Machine, "declared is declared");
    assert!(read.bytes.is_empty(), "it simply has not been written yet");
    assert!(
        !String::from_utf8_lossy(&read.bytes).contains("noise"),
        "and must not quietly become the scrollback"
    );

    let _ = std::fs::remove_dir_all(&ws);
}

/// The machine lens resumes from an offset like the human one, so polling a
/// growing sidecar costs the new tail rather than the whole file.
#[test]
fn the_machine_lens_resumes_from_an_offset() {
    let ws = workspace(
        "offset",
        r#"proc ({name check, argv (sh -c "printf 'one\ntwo\n' > .diagnostics"), machine_file ".diagnostics", machine_format "lines"})"#,
    );
    let reg = registry();
    let caller = Caller::Scoped(Scope::resolve(&ws));

    let id = reg.start_named("check", &caller).expect("start");
    reg.wait(&id).expect("wait");

    let first = reg.machine(&id, 0).expect("read");
    assert!(!first.bytes.is_empty());
    let second = reg.machine(&id, first.next_offset).expect("resume");
    assert!(
        second.bytes.is_empty(),
        "resuming must not re-deliver what was already read"
    );

    let _ = std::fs::remove_dir_all(&ws);
}

/// A sidecar path is resolved **inside the job's directory** and cannot escape
/// it. The manifest is agent-editable, so a declared path is not a licence to
/// read anything on the machine.
#[test]
fn a_sidecar_path_cannot_escape_the_jobs_directory() {
    let ws = workspace(
        "escape",
        r#"proc ({name sneaky, argv (echo hi), machine_file "../../../etc/passwd", machine_format "text"})"#,
    );
    let reg = registry();
    let caller = Caller::Scoped(Scope::resolve(&ws));

    let id = reg.start_named("sneaky", &caller).expect("start");
    reg.wait(&id).expect("wait");

    let err = reg
        .machine(&id, 0)
        .expect_err("a path escaping the job's directory must be refused");
    assert!(
        err.to_string().contains("escape") || err.to_string().contains("outside"),
        "got: {err}"
    );

    let _ = std::fs::remove_dir_all(&ws);
}

/// **The point of the human lens.** Under a terminal view the job's stdio is a
/// pty, so `isatty` is true — and many tools emit something entirely different
/// when they think they are talking to a pipe.
#[test]
fn a_terminal_view_job_sees_a_tty() {
    let ws = workspace(
        "tty",
        r#"proc ({name check, argv (sh -c "test -t 1 && echo TTY || echo PIPE"), human_view @Terminal})"#,
    );
    let reg = registry();
    let caller = Caller::Scoped(Scope::resolve(&ws));

    let id = reg.start_named("check", &caller).expect("start");
    reg.wait(&id).expect("wait");

    // The pump is a thread; give it a moment to drain the master.
    let mut seen = String::new();
    for _ in 0..200 {
        seen = String::from_utf8_lossy(&reg.tail(&id, 0).expect("tail").bytes).into_owned();
        if seen.contains("TTY") || seen.contains("PIPE") {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(seen.contains("TTY"), "expected a tty, got: {seen:?}");

    let _ = std::fs::remove_dir_all(&ws);
}

/// The default is a log stream, and there `isatty` is false — so a tool takes
/// its non-interactive path unless a workspace asked for otherwise.
#[test]
fn the_default_view_is_not_a_tty() {
    let ws = workspace(
        "notty",
        r#"proc ({name check, argv (sh -c "test -t 1 && echo TTY || echo PIPE")})"#,
    );
    let reg = registry();
    let caller = Caller::Scoped(Scope::resolve(&ws));

    let id = reg.start_named("check", &caller).expect("start");
    reg.wait(&id).expect("wait");
    let seen = String::from_utf8_lossy(&reg.tail(&id, 0).expect("tail").bytes).into_owned();
    assert!(seen.contains("PIPE"), "expected a pipe, got: {seen:?}");

    let _ = std::fs::remove_dir_all(&ws);
}

/// **Scrollback stays file-backed under a pty.** The master is pumped into the
/// same capture file the log lens writes, which is what keeps `tail`, the
/// durable record and re-adoption working the same for both views.
#[test]
fn terminal_output_still_lands_in_the_capture_file() {
    let ws = workspace(
        "captured",
        r#"proc ({name check, argv (sh -c "echo drawn-under-a-pty"), human_view @Terminal})"#,
    );
    let reg = registry();
    let caller = Caller::Scoped(Scope::resolve(&ws));

    let id = reg.start_named("check", &caller).expect("start");
    reg.wait(&id).expect("wait");

    let mut seen = String::new();
    for _ in 0..200 {
        seen = String::from_utf8_lossy(&reg.tail(&id, 0).expect("tail").bytes).into_owned();
        if seen.contains("drawn-under-a-pty") {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(
        seen.contains("drawn-under-a-pty"),
        "a pty job's scrollback must be readable the same way: {seen:?}"
    );

    let _ = std::fs::remove_dir_all(&ws);
}

/// A terminal-view job with a machine view still answers through the machine
/// view: the two channels are independent, and the consumer picks per read.
#[test]
fn both_lenses_serve_one_job_independently() {
    let ws = workspace(
        "both",
        r#"proc ({name check,
                 argv (sh -c "echo NOISE; echo '{\"ok\":true}' > .diagnostics"),
                 human_view @Terminal, machine_file ".diagnostics", machine_format "json"})"#,
    );
    let reg = registry();
    let caller = Caller::Scoped(Scope::resolve(&ws));

    let id = reg.start_named("check", &caller).expect("start");
    reg.wait(&id).expect("wait");

    let read = reg.machine(&id, 0).expect("machine");
    assert_eq!(read.source, LensSource::Machine);
    assert!(String::from_utf8_lossy(&read.bytes).contains("\"ok\""));

    let mut human = String::new();
    for _ in 0..200 {
        human = String::from_utf8_lossy(&reg.tail(&id, 0).expect("tail").bytes).into_owned();
        if human.contains("NOISE") {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(human.contains("NOISE"), "the human channel is still there");

    let _ = std::fs::remove_dir_all(&ws);
}
