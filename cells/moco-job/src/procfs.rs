//! Liveness for a process the daemon no longer owns.
//!
//! A re-adopted job is not our child, so `try_wait` cannot reap it and there is
//! no exit code to collect. All we can ask is whether *the same* process is
//! still there — and "same" is the whole problem, because pids are reused. The
//! guard is the pid's **start time**: recorded when the job is first spawned and
//! compared on every probe, so a recycled pid running something else is never
//! mistaken for the original job.
//!
//! implements: registry-is-node-state-on-disk

/// What a probe can say about a pid.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Liveness {
    /// The pid exists and its start time matches the one recorded: same process.
    Alive,
    /// The pid is gone, or it exists but started at a different time — which
    /// means the original process ended and this pid was reused.
    Dead,
    /// This platform has no supported probe, so nothing can be asserted either
    /// way. Callers must not treat it as alive.
    Unsupported,
}

/// The process state character and start time from `/proc/<pid>/stat`.
///
/// Both fields are positional, and the process name (field 2) may itself contain
/// spaces and parentheses — so the scan starts after the **last** `')'` rather
/// than splitting the whole line. From there, field 3 (state) is index 0 and
/// field 22 (starttime) is index 19.
#[cfg(target_os = "linux")]
fn stat_fields(pid: u32) -> Option<(char, u64)> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let after_comm = stat.get(stat.rfind(')').map(|i| i + 1)?..)?;
    let mut fields = after_comm.split_whitespace();
    let state = fields.next()?.chars().next()?;
    let start = fields.nth(18)?.parse().ok()?;
    Some((state, start))
}

#[cfg(not(target_os = "linux"))]
fn stat_fields(_pid: u32) -> Option<(char, u64)> {
    None
}

/// The kernel's start time for `pid`, in clock ticks since boot.
pub fn start_time(pid: u32) -> Option<u64> {
    stat_fields(pid).map(|(_, start)| start)
}

/// Whether the process recorded as `(pid, recorded_start)` is still running.
///
/// On a platform with no probe this is `Unsupported`, never `Alive`: a job whose
/// liveness cannot be established settles as `OutcomeUnknown` rather than being
/// reported as running forever.
pub fn liveness(pid: u32, recorded_start: u64) -> Liveness {
    if !PROBE_SUPPORTED {
        return Liveness::Unsupported;
    }
    match stat_fields(pid) {
        // A **zombie** has exited; its entry lingers only until someone reaps
        // it, and `/proc/<pid>/stat` is still perfectly readable meanwhile. It
        // is dead, and reporting it alive would leave a `wait` polling forever.
        Some(('Z', _)) => Liveness::Dead,
        Some((_, now)) if now == recorded_start => Liveness::Alive,
        // Either the pid is gone, or it is a different process wearing it.
        _ => Liveness::Dead,
    }
}

/// Whether this platform can answer [`liveness`] at all.
pub const PROBE_SUPPORTED: bool = cfg!(target_os = "linux");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn our_own_pid_is_alive_against_its_recorded_start() {
        if !PROBE_SUPPORTED {
            return;
        }
        let pid = std::process::id();
        let start = start_time(pid).expect("own start time is readable");
        assert_eq!(liveness(pid, start), Liveness::Alive);
    }

    #[test]
    fn a_mismatched_start_time_is_dead_not_alive() {
        if !PROBE_SUPPORTED {
            return;
        }
        let pid = std::process::id();
        let start = start_time(pid).expect("own start time is readable");
        // Same pid, different start: exactly the pid-reuse case.
        assert_eq!(liveness(pid, start.wrapping_add(1)), Liveness::Dead);
    }
}
