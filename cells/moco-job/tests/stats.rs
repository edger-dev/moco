//! Resource sampling: advisory, and never enforcement.
//!
//! implements: resource-limits-report-never-enforce

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use moco_job::{JobRegistry, JobRequest, Limits};

static SEQ: AtomicU64 = AtomicU64::new(0);

#[allow(
    clippy::expect_used,
    reason = "test helper: a failure is a broken harness"
)]
fn registry(name: &str) -> (JobRegistry, PathBuf) {
    let d = std::env::temp_dir().join(format!(
        "moco-stats-{}-{}-{name}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).expect("create");
    let d = d.canonicalize().expect("canonicalize");
    let reg = JobRegistry::ungoverned()
        .expect("registry")
        .with_dir(&d)
        .expect("with_dir");
    (reg, d)
}

fn spinner(cwd: &PathBuf) -> JobRequest {
    JobRequest::new(["sh", "-c", "while :; do :; done"].map(String::from), cwd)
}

fn sleeper(cwd: &PathBuf) -> JobRequest {
    JobRequest::new(["sleep", "30"].map(String::from), cwd)
}

#[test]
#[allow(clippy::expect_used, reason = "a failure here is a broken harness")]
fn a_running_job_reports_the_memory_it_occupies() {
    let (reg, cwd) = registry("rss");
    let id = reg.start(sleeper(&cwd)).expect("start");
    reg.sample_all();

    let stats = reg.stats(&id).expect("stats");
    let latest = stats.samples.last().expect("one sample");
    // Any real process occupies *something*; zero would mean the read failed
    // and was silently reported as a fact.
    assert!(
        latest.rss_bytes > 0,
        "a live process should report non-zero RSS, got {latest:?}"
    );
    let _ = reg.kill(&id, &moco_job::Caller::Console);
}

#[test]
#[allow(clippy::expect_used, reason = "a failure here is a broken harness")]
fn the_first_sample_claims_no_cpu_rate_because_a_rate_needs_two_points() {
    let (reg, cwd) = registry("first");
    let id = reg.start(spinner(&cwd)).expect("start");
    reg.sample_all();

    let stats = reg.stats(&id).expect("stats");
    assert_eq!(stats.samples.len(), 1);
    assert_eq!(
        stats.samples[0].cpu_pct, 0,
        "one reading is a total, not a rate — it must not be reported as load"
    );
    let _ = reg.kill(&id, &moco_job::Caller::Console);
}

#[test]
#[allow(clippy::expect_used, reason = "a failure here is a broken harness")]
fn a_busy_job_shows_cpu_once_there_are_two_points_to_compare() {
    let (reg, cwd) = registry("busy");
    let id = reg.start(spinner(&cwd)).expect("start");
    reg.sample_all();
    std::thread::sleep(Duration::from_millis(400));
    reg.sample_all();

    let stats = reg.stats(&id).expect("stats");
    let latest = stats.samples.last().expect("two samples");
    assert!(
        latest.cpu_pct > 20,
        "a spin loop should show substantial CPU, got {}%",
        latest.cpu_pct
    );
    let _ = reg.kill(&id, &moco_job::Caller::Console);
}

#[test]
#[allow(clippy::expect_used, reason = "a failure here is a broken harness")]
fn a_breached_limit_is_reported_and_the_job_keeps_running() {
    let (reg, cwd) = registry("breach");
    // A limit this job cannot help but cross.
    let id = reg
        .start(spinner(&cwd).with_limits(Limits {
            cpu_pct: 1,
            mem_mb: 0,
        }))
        .expect("start");
    reg.sample_all();
    std::thread::sleep(Duration::from_millis(400));
    reg.sample_all();

    let stats = reg.stats(&id).expect("stats");
    assert!(stats.breach.cpu, "the declared cpu limit was crossed");
    assert!(!stats.breach.memory, "no memory limit was declared");

    // **The whole contract**: crossing a limit reports, it does not act. The
    // job must still be running, unkilled, after the breach.
    reg.supervise();
    assert!(
        reg.status_of(&id).expect("status").is_running(),
        "an advisory limit must never stop a job — that is a different feature"
    );
    let _ = reg.kill(&id, &moco_job::Caller::Console);
}

#[test]
#[allow(clippy::expect_used, reason = "a failure here is a broken harness")]
fn a_job_that_declares_no_limits_can_never_breach() {
    let (reg, cwd) = registry("unset");
    let id = reg.start(spinner(&cwd)).expect("start");
    reg.sample_all();
    std::thread::sleep(Duration::from_millis(300));
    reg.sample_all();

    let stats = reg.stats(&id).expect("stats");
    assert!(stats.limits.is_unset());
    assert!(
        !stats.breach.any(),
        "nothing was declared, so nothing broke"
    );
    let _ = reg.kill(&id, &moco_job::Caller::Console);
}

#[test]
#[allow(clippy::expect_used, reason = "a failure here is a broken harness")]
fn history_is_bounded_so_a_long_lived_job_does_not_grow_without_end() {
    let (reg, cwd) = registry("ring");
    let id = reg.start(sleeper(&cwd)).expect("start");
    for _ in 0..(moco_job::SAMPLE_HISTORY + 20) {
        reg.sample_all();
    }

    let stats = reg.stats(&id).expect("stats");
    assert_eq!(
        stats.samples.len(),
        moco_job::SAMPLE_HISTORY,
        "a supervisor keeps a recent window, not a metrics store"
    );
    let _ = reg.kill(&id, &moco_job::Caller::Console);
}

#[test]
#[allow(clippy::expect_used, reason = "a failure here is a broken harness")]
fn a_finished_job_is_sampled_without_error_and_simply_stops_accruing() {
    let (reg, cwd) = registry("done");
    let id = reg
        .start(JobRequest::new(["true"].map(String::from), &cwd))
        .expect("start");
    reg.wait(&id).expect("wait");

    // Sampling a job with no live process is not an error — the registry is
    // node-global and always contains dead jobs.
    reg.sample_all();
    let stats = reg.stats(&id).expect("stats");
    assert!(
        stats.samples.is_empty(),
        "a job with nothing running has nothing to report, not a zero reading"
    );
}
