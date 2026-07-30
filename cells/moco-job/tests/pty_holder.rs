//! The terminal outlives the supervisor.
//!
//! implements: pty-holder-owns-the-terminal

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use moco_job::scope::{Caller, Scope};
use moco_job::{JobRegistry, MANIFEST_FILE, ScreenSource};

static SEQ: AtomicU64 = AtomicU64::new(0);

/// The holder binary this workspace builds.
fn holder() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_moco-pty-holder"))
}

#[allow(
    clippy::expect_used,
    reason = "test helper: a failure is a broken harness"
)]
fn dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!(
        "moco-holder-{}-{}-{name}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).expect("create");
    d.canonicalize().expect("canonicalize")
}

#[allow(
    clippy::expect_used,
    reason = "test helper: a failure is a broken harness"
)]
fn workspace(name: &str, manifest: &str) -> PathBuf {
    let d = dir(name);
    std::fs::create_dir_all(d.join(".git")).expect("repo");
    std::fs::write(d.join(MANIFEST_FILE), manifest).expect("manifest");
    d
}

#[allow(
    clippy::expect_used,
    reason = "test helper: a failure is a broken harness"
)]
fn daemon(state: &PathBuf) -> JobRegistry {
    JobRegistry::ungoverned()
        .expect("registry")
        .with_dir(state)
        .expect("with_dir")
        .with_pty_holder(holder())
}

/// Jobs here deliberately outlive the daemon that started them, so a test that
/// panics before its cleanup leaves one running for as long as it was told to
/// sleep. Kept short for that reason.
fn until(mut f: impl FnMut() -> bool) -> bool {
    for _ in 0..100 {
        if f() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    false
}

fn alive(pid: u32) -> bool {
    let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else {
        return false;
    };
    let Some((_, after)) = stat.rsplit_once(')') else {
        return false;
    };
    !matches!(after.split_whitespace().next(), Some("Z") | None)
}

/// **The whole point.** A terminal job and its screen both survive the daemon
/// that started them.
#[test]
#[allow(clippy::expect_used, reason = "a failure here is a broken harness")]
fn a_terminal_job_and_its_screen_outlive_the_daemon() {
    let ws = workspace(
        "outlive",
        r#"proc ({name tui,
                argv (sh -c "printf '\033[2J\033[1;1HSTILL-HERE'; sleep 20"),
                human_view @Terminal})"#,
    );
    let state = dir("outlive-state");
    let caller = Caller::Scoped(Scope::resolve(&ws));

    let job_pid = {
        let reg = daemon(&state);
        let id = reg.start_named("tui", &caller).expect("start");
        // Wait for the **holder's** screen specifically. Waiting for any screen
        // containing the text would be satisfied instantly by the replay path,
        // which is exactly the mechanism this test exists to prove unnecessary.
        assert!(
            until(|| reg
                .screen(&id)
                .is_ok_and(|v| v.source == ScreenSource::Live && v.text.contains("STILL-HERE"))),
            "the holder should be rendering the job's screen"
        );
        reg.job_pid_of(&id).expect("the job's own pid")
        // The daemon goes away here.
    };

    assert!(
        alive(job_pid),
        "the job must not die with the supervisor — that is the bug this fixes"
    );

    // A new daemon over the same state directory re-adopts everything.
    let reborn = daemon(&state);
    let (id, _) = reborn
        .declared(&Scope::workspace(&ws), "tui")
        .expect("re-adopted");

    let view = reborn.screen(&id).expect("screen");
    assert_eq!(
        view.source,
        ScreenSource::Live,
        "the holder is still running, so this is observed rather than reconstructed"
    );
    assert!(
        view.text.contains("STILL-HERE"),
        "the screen was drawn before the restart and must still be there, got:\n{}",
        view.text
    );

    let _ = reborn.kill(&id, &Caller::Console);
}

/// Stopping the job stops the holder with it. A holder outliving its job would
/// be a leaked process per terminal job, forever.
#[test]
#[allow(clippy::expect_used, reason = "a failure here is a broken harness")]
fn stopping_the_job_takes_the_holder_with_it() {
    let ws = workspace(
        "stop",
        r#"proc ({name tui, argv (sleep 20), human_view @Terminal})"#,
    );
    let state = dir("stop-state");
    let reg = daemon(&state);
    let id = reg
        .start_named("tui", &Caller::Scoped(Scope::resolve(&ws)))
        .expect("start");

    let holder_pid = reg.pid_of(&id).expect("holder pid");
    // `job_pid_of` falls back to the holder's pid until the holder has
    // published, so waiting for `is_some()` would wait for nothing.
    assert!(until(|| reg.job_pid_of(&id) != Some(holder_pid)));
    let job_pid = reg.job_pid_of(&id).expect("job pid");
    assert_ne!(holder_pid, job_pid, "the holder is not the job");

    reg.kill(&id, &Caller::Console).expect("kill");

    assert!(until(|| !alive(job_pid)), "the job should be stopped");
    assert!(
        until(|| !alive(holder_pid)),
        "a holder that outlives its job is a leak, one per terminal job"
    );
}

/// Resource samples describe the **job**, not the holder.
///
/// The holder is a read loop that consumes almost nothing, so sampling it would
/// report every terminal job as idle no matter what it was doing — the exact
/// question resource sampling exists to answer, answered wrongly.
#[test]
#[allow(clippy::expect_used, reason = "a failure here is a broken harness")]
fn samples_describe_the_job_and_not_its_holder() {
    let ws = workspace(
        "sampling",
        r#"proc ({name spin,
                argv (sh -c "while :; do :; done"),
                human_view @Terminal})"#,
    );
    let state = dir("sampling-state");
    let reg = daemon(&state);
    let id = reg
        .start_named("spin", &Caller::Scoped(Scope::resolve(&ws)))
        .expect("start");

    let holder_pid = reg.pid_of(&id).expect("holder pid");
    assert!(until(|| reg.job_pid_of(&id) != Some(holder_pid)));
    reg.sample_all();
    std::thread::sleep(Duration::from_millis(400));
    reg.sample_all();

    let stats = reg.stats(&id).expect("stats");
    let latest = stats.samples.last().expect("a sample");
    assert!(
        latest.cpu_pct > 20,
        "the spinning job's CPU must be reported, not the holder's idle loop \
         — got {}%",
        latest.cpu_pct
    );

    let _ = reg.kill(&id, &Caller::Console);
}

/// Without a holder configured, a terminal job behaves exactly as before.
/// Durability is opt-in, so a deployment that has not installed the binary is
/// not broken by its absence.
#[test]
#[allow(clippy::expect_used, reason = "a failure here is a broken harness")]
fn a_daemon_with_no_holder_still_runs_terminal_jobs() {
    let ws = workspace(
        "noholder",
        r#"proc ({name tui, argv (sh -c "printf 'DREW'; sleep 30"), human_view @Terminal})"#,
    );
    let state = dir("noholder-state");
    let reg = JobRegistry::ungoverned()
        .expect("registry")
        .with_dir(&state)
        .expect("with_dir");

    let id = reg
        .start_named("tui", &Caller::Scoped(Scope::resolve(&ws)))
        .expect("start");
    assert!(until(|| reg
        .screen(&id)
        .is_ok_and(|v| v.text.contains("DREW"))));

    let _ = reg.kill(&id, &Caller::Console);
}
