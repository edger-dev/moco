//! The screen lens: what a person would see if they attached right now.
//!
//! implements: the-screen-is-a-live-fold-not-a-replay

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use moco_job::lens::ScreenSource;
use moco_job::scope::{Caller, Scope};
use moco_job::{Caller as _C, JobRegistry, MANIFEST_FILE};

static SEQ: AtomicU64 = AtomicU64::new(0);

#[allow(
    clippy::expect_used,
    reason = "test helper: a failure is a broken harness"
)]
fn workspace(name: &str, manifest: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "moco-screen-{}-{}-{name}",
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

/// Give the pump a moment to drain the pty and feed the parser.
fn settle() {
    std::thread::sleep(Duration::from_millis(300));
}

/// **The point of the whole lens.** A progress bar that rewrites one line with
/// carriage returns is a wall of noise in scrollback and a single line on a
/// screen. `tail` cannot answer "what does it look like now"; this can.
#[test]
#[allow(clippy::expect_used, reason = "a failure here is a broken harness")]
fn a_redrawn_line_reads_as_its_final_state_not_every_frame() {
    let ws = workspace(
        "redraw",
        r#"proc ({name bar,
                argv (sh -c "printf 'step 1/3\rstep 2/3\rstep 3/3'; sleep 5"),
                human_view @Terminal})"#,
    );
    let reg = registry();
    let id = reg
        .start_named("bar", &Caller::Scoped(Scope::resolve(&ws)))
        .expect("start");
    settle();

    let view = reg.screen(&id).expect("screen");
    assert_eq!(view.source, ScreenSource::Live);
    assert!(
        view.text.contains("step 3/3"),
        "the screen shows the current frame, got:\n{}",
        view.text
    );
    assert!(
        !view.text.contains("step 1/3"),
        "superseded frames are not on the screen — that is what makes this \
         cheaper than scrollback, got:\n{}",
        view.text
    );

    let _ = reg.kill(&id, &_C::Console);
    let _ = std::fs::remove_dir_all(&ws);
}

/// Cursor positioning is honoured, so a TUI that draws out of order reads back
/// in the order a person sees it.
#[test]
#[allow(clippy::expect_used, reason = "a failure here is a broken harness")]
fn absolute_cursor_moves_land_where_they_were_addressed() {
    let ws = workspace(
        "cursor",
        r#"proc ({name tui,
                argv (sh -c "printf '\033[2J\033[3;5Hmiddle\033[1;1Htop'; sleep 5"),
                human_view @Terminal})"#,
    );
    let reg = registry();
    let id = reg
        .start_named("tui", &Caller::Scoped(Scope::resolve(&ws)))
        .expect("start");
    settle();

    let view = reg.screen(&id).expect("screen");
    let lines: Vec<&str> = view.text.lines().collect();
    assert!(lines[0].starts_with("top"), "got:\n{}", view.text);
    assert!(
        lines[2].trim_start().starts_with("middle"),
        "row 3 column 5, as addressed, got:\n{}",
        view.text
    );

    let _ = reg.kill(&id, &_C::Console);
    let _ = std::fs::remove_dir_all(&ws);
}

/// The job is told the same size the screen is rendered at. A parser grid that
/// disagreed with the job's own `TIOCGWINSZ` would wrap every line in a place
/// the job never wrapped it.
#[test]
#[allow(clippy::expect_used, reason = "a failure here is a broken harness")]
fn the_job_and_the_screen_agree_on_how_big_the_terminal_is() {
    let ws = workspace(
        "size",
        r#"proc ({name size, argv (sh -c "stty size; sleep 5"), human_view @Terminal})"#,
    );
    let reg = registry();
    let id = reg
        .start_named("size", &Caller::Scoped(Scope::resolve(&ws)))
        .expect("start");
    settle();

    let view = reg.screen(&id).expect("screen");
    assert!(
        view.text.contains(&format!("{} {}", view.rows, view.cols)),
        "the job's own `stty size` must match the grid we render, got:\n{}",
        view.text
    );

    let _ = reg.kill(&id, &_C::Console);
    let _ = std::fs::remove_dir_all(&ws);
}

/// A logs-view job has no pty and no live parser, so its screen is
/// **reconstructed** from the retained capture — and says so.
#[test]
#[allow(clippy::expect_used, reason = "a failure here is a broken harness")]
fn a_logs_job_gets_a_reconstructed_screen_and_is_told_so() {
    let ws = workspace(
        "logs",
        r#"proc ({name plain, argv (sh -c "printf 'a\rb'; sleep 5")})"#,
    );
    let reg = registry();
    let id = reg
        .start_named("plain", &Caller::Scoped(Scope::resolve(&ws)))
        .expect("start");
    settle();

    let view = reg.screen(&id).expect("screen");
    assert_eq!(
        view.source,
        ScreenSource::Replayed,
        "no pty means no live fold, and a caller must be able to tell"
    );
    // The parse still happens, so a carriage return still overwrites.
    assert!(view.text.starts_with('b'), "got:\n{}", view.text);

    let _ = reg.kill(&id, &_C::Console);
    let _ = std::fs::remove_dir_all(&ws);
}

/// **Why the fold is live, proven.** A banner drawn once survives its own bytes
/// being compacted away — because the parser holds the state, not the file.
///
/// The flood writes to a fixed row so it never scrolls, which is what a real
/// TUI does: the banner stays on screen while the bytes that drew it are long
/// gone from the capture. Replaying the retained capture could not produce this
/// screen, which is the whole argument for folding as the output goes past.
#[test]
#[allow(clippy::expect_used, reason = "a failure here is a broken harness")]
fn a_screen_survives_the_bytes_it_was_drawn_from_being_discarded() {
    let ws = workspace(
        "compact",
        r#"proc ({name banner,
                argv (sh -c "printf '\033[2J\033[1;1HBANNER'; i=0; while [ $i -lt 4000 ]; do printf '\033[20;1Hxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx'; i=$((i+1)); done; sleep 5"),
                human_view @Terminal})"#,
    );
    let reg = registry().with_capture_cap(32 * 1024);
    let id = reg
        .start_named("banner", &Caller::Scoped(Scope::resolve(&ws)))
        .expect("start");
    settle();

    // Force the compaction that `tail` performs, and confirm it really happened
    // — without that, this test proves nothing at all.
    let read = reg.tail(&id, 0).expect("tail");
    assert!(
        read.skipped > 0,
        "the capture must actually have been compacted for this to be a test"
    );

    let view = reg.screen(&id).expect("screen");
    assert_eq!(view.source, ScreenSource::Live);
    assert!(
        view.text.contains("BANNER"),
        "the banner was drawn before compaction discarded its bytes, and the \
         live fold still has it, got:\n{}",
        view.text
    );

    let _ = reg.kill(&id, &_C::Console);
    let _ = std::fs::remove_dir_all(&ws);
}

/// Resuming from beyond the end is **not an error** — it is a caller that is
/// already up to date, and the answer is "nothing new".
///
/// This is how `settle_all` reads: `u64::MAX` means *give me the status, not
/// the bytes*. An offset that large is rejected outright by the kernel, so
/// before the clamp every settle was quietly failing its read and only worked
/// because the status is updated before the seek.
#[test]
#[allow(clippy::expect_used, reason = "a failure here is a broken harness")]
fn an_offset_past_the_end_reads_empty_rather_than_failing() {
    let ws = workspace(
        "past-end",
        r#"proc ({name quick, argv (sh -c "echo hello")})"#,
    );
    let reg = registry();
    let id = reg
        .start_named("quick", &Caller::Scoped(Scope::resolve(&ws)))
        .expect("start");
    reg.wait(&id).expect("wait");

    let read = reg
        .tail(&id, u64::MAX)
        .expect("an offset past the end is a legitimate ask, not an IO error");
    assert!(read.bytes.is_empty());
    assert!(read.status.is_terminal(), "the status still comes back");

    let _ = std::fs::remove_dir_all(&ws);
}
