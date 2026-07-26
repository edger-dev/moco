//! When a job first starts, and what happens when it ends.
//!
//! Two fields that are constantly conflated, kept apart because they answer
//! different questions **and are acted on by different components**. Naming the
//! actor is what makes the split obvious rather than pedantic.
//!
//! implements: autostart-and-restart-are-orthogonal
//! implements: job-lifetime-oneshot-or-service

use facet::Facet;

use crate::job::JobStatus;

/// What kind of thing this job is.
///
/// The class does not change what runs — it changes what an **exit means**, and
/// therefore whether a restart policy has anything to act on.
#[derive(Facet, Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(u8)]
pub enum Lifetime {
    /// Expected to end. Bounded by a deadline; exit 0 is success.
    #[default]
    OneShot,
    /// Expected to keep running. Unbounded, and carries a restart policy.
    Service,
}

impl Lifetime {
    /// Did this terminal state mean the job did its job?
    ///
    /// For a `OneShot`, `exited(0)` is success. For a `Service`, **any**
    /// un-requested exit is a failure regardless of code — a dev server that
    /// exits cleanly is still not serving. That single asymmetry is what
    /// `restart = on-failure` acts on.
    pub fn succeeded(&self, status: &JobStatus) -> bool {
        match self {
            Lifetime::OneShot => matches!(status, JobStatus::Done { code: 0 }),
            // A service reaching *any* terminal state has stopped serving.
            Lifetime::Service => false,
        }
    }
}

/// What happens when a job exits.
///
/// Meaningful only for a [`Lifetime::Service`]: a one-shot that ran is finished,
/// and respawning it would be running it twice.
#[derive(Facet, Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(u8)]
pub enum RestartPolicy {
    #[default]
    Never,
    /// Bring it back when it did not succeed.
    OnFailure,
    /// Bring it back whenever it ends on its own.
    Always,
}

impl RestartPolicy {
    /// Should a job of this lifetime, in this state, be brought back?
    ///
    /// A **requested** stop is never a restart trigger, whatever the policy:
    /// restarting something a caller just asked to stop would make stopping
    /// impossible, and `always` means "always when it ends on its own", not
    /// "resist being stopped".
    pub fn should_restart(&self, lifetime: Lifetime, status: &JobStatus, requested: bool) -> bool {
        if requested || lifetime == Lifetime::OneShot {
            return false;
        }
        match self {
            RestartPolicy::Never => false,
            RestartPolicy::Always => true,
            RestartPolicy::OnFailure => !lifetime.succeeded(status),
        }
    }
}

/// When a job is **first** started — a different question from what happens when
/// it exits, and answered by a different component.
///
/// - `Boot` is the **daemon's** job: on startup it reads the node-level manifest
///   and starts every boot entry.
/// - `Session` is the **agent's** job: an idempotent ensure at session start.
///   Workspace manifests are deliberately not scanned at boot — the daemon does
///   not know every repo on the machine, and walking the filesystem for
///   manifests is unbounded and surprising.
///
/// Overloading `restart = always` to also mean "start automatically" would
/// conflate first-start with crash policy, and would make "start once at boot,
/// never restart" inexpressible.
#[derive(Facet, Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(u8)]
pub enum Autostart {
    #[default]
    Manual,
    /// Started by the daemon at startup.
    Boot,
    /// Started by the agent at session start.
    Session,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_requested_stop_never_triggers_a_restart() {
        for policy in [
            RestartPolicy::Never,
            RestartPolicy::OnFailure,
            RestartPolicy::Always,
        ] {
            assert!(
                !policy.should_restart(Lifetime::Service, &JobStatus::Killed, true),
                "{policy:?} must not fight an explicit stop"
            );
        }
    }

    #[test]
    fn on_failure_ignores_a_one_shots_success_but_a_service_has_none() {
        assert!(!RestartPolicy::OnFailure.should_restart(
            Lifetime::OneShot,
            &JobStatus::Done { code: 1 },
            false
        ));
        assert!(RestartPolicy::OnFailure.should_restart(
            Lifetime::Service,
            &JobStatus::Done { code: 0 },
            false
        ));
    }
}
