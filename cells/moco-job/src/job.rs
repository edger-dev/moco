use std::fmt;
use std::path::PathBuf;
use std::time::Duration;

use facet::Facet;

use crate::audit::Verdict;
use crate::lifecycle::{Lifetime, RestartPolicy};
use crate::scope::Scope;

/// A job's identity — registry-assigned, addressable across a disconnect, and
/// **node-unique**: stable across a restart of the daemon that created it, and
/// non-colliding between two daemons sharing one registry directory. A
/// per-process counter is not a job id.
///
/// The value is opaque; only its uniqueness is contracted. It is minted by the
/// registry, which resolves any collision by exclusive file creation, so
/// uniqueness does not rest on the clock or on pid non-reuse alone.
///
/// implements: job-is-the-unit-not-rpc
/// implements: registry-is-node-state-on-disk
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct JobId(pub String);

impl JobId {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for JobId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Why a job was denied.
///
/// implements: approval-is-a-job-state
#[derive(Facet, Debug, Clone, PartialEq, Eq)]
#[repr(u8)]
pub enum DeniedReason {
    /// A deny rule matched this exact argv.
    Rule,
    /// A human decided against it.
    Decision,
    /// Nobody decided in time — the fail-closed default.
    NoApprover,
    /// Its cwd did not resolve inside the node's allowed root.
    CwdEscape,
}

/// The lifecycle state of a job.
///
/// Approval is modeled as job *states*, not a side feature:
/// `pending-approval → running → done | denied | killed | timed-out`.
///
/// implements: job-is-the-unit-not-rpc
/// implements: approval-is-a-job-state
#[derive(Facet, Debug, Clone, PartialEq, Eq)]
#[repr(u8)]
pub enum JobStatus {
    /// Registered but not spawned: awaiting a human decision.
    PendingApproval,
    Running,
    Done {
        code: i32,
    },
    Killed,
    TimedOut,
    /// Permitted, but it could not be started at all.
    Failed {
        error: String,
    },
    /// Never ran, and never will.
    ///
    /// A struct variant, not a tuple one: a tuple variant wrapping another enum
    /// does not round-trip through Styx (`@Denied@Rule` is not re-readable).
    Denied {
        reason: DeniedReason,
    },
    /// The job ended, but the daemon that owns it now cannot say how.
    ///
    /// This is the honest terminal state of a **re-adopted** job: it is no
    /// longer our child, so there is no exit code to reap — only the fact that
    /// its pid is gone. Reported truthfully rather than fabricating a code or
    /// losing the job. The degraded mode is transient: one restart makes the job
    /// our child again and full fidelity returns.
    ///
    /// implements: registry-is-node-state-on-disk
    /// implements: job-durability-both-kill-vectors
    OutcomeUnknown,
}

impl JobStatus {
    /// A terminal state is any state the job will not leave. A pending job is
    /// *not* terminal — it is waiting on a decision.
    pub fn is_terminal(&self) -> bool {
        !matches!(self, JobStatus::Running | JobStatus::PendingApproval)
    }
}

/// What to run, as an **argument vector** — never a shell string. The daemon
/// spawns the program directly, so shell metacharacters are inert data. This is
/// the structural security decision; it holds regardless of any rule-set.
///
/// implements: argv-not-shell
#[derive(Debug, Clone)]
pub struct JobRequest {
    pub argv: Vec<String>,
    pub cwd: PathBuf,
    /// Who owns this job. `None` means "resolve it from `cwd`", which is the
    /// right default for an ad-hoc job: it belongs to the workspace it runs in.
    /// A caller that knows better — a session starting work on behalf of its own
    /// workspace while the job runs elsewhere — says so.
    pub scope: Option<Scope>,
    /// The manifest name this job is declared under, if any.
    pub name: Option<String>,
    /// Whether this is expected to end or to keep running.
    pub lifetime: Lifetime,
    /// What happens when it exits. Meaningful only for a service.
    pub restart: RestartPolicy,
    /// Optional execution deadline. A job still running past it lands
    /// `TimedOut` when it is next awaited (`wait` / `run`). v1 has no background
    /// reaper, so a job that is never awaited is not force-expired — a full
    /// job-owned deadline waits for the durability phase.
    pub deadline: Option<Duration>,
}

impl JobRequest {
    pub fn new<I, S>(argv: I, cwd: impl Into<PathBuf>) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self {
            argv: argv.into_iter().map(Into::into).collect(),
            cwd: cwd.into(),
            deadline: None,
            scope: None,
            name: None,
            lifetime: Lifetime::OneShot,
            restart: RestartPolicy::Never,
        }
    }

    pub fn with_deadline(mut self, deadline: Duration) -> Self {
        self.deadline = Some(deadline);
        self
    }

    /// Declare which workspace owns this job.
    pub fn in_scope(mut self, scope: Scope) -> Self {
        self.scope = Some(scope);
        self
    }

    /// Record the manifest name this job was declared under.
    pub fn named(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }

    /// Declare what kind of job this is and what happens when it exits.
    pub fn with_lifecycle(mut self, lifetime: Lifetime, restart: RestartPolicy) -> Self {
        self.lifetime = lifetime;
        self.restart = restart;
        self
    }
}

/// An incremental read of a job's output plus its live status — the second
/// property of a job (observable while it runs).
///
/// implements: job-is-the-unit-not-rpc
#[derive(Debug, Clone)]
pub struct Tail {
    pub bytes: Vec<u8>,
    pub next_offset: u64,
    pub status: JobStatus,
}

/// A job's terminal record — the fourth property of a job, and **self-
/// describing**: it says where it ran and under what authority, so the agent
/// never has to guess whether a relative cwd landed where it meant or whether an
/// unmatched command was auto-allowed or approved once.
///
/// `verdict` here is the *same* value written to the audit record — one source
/// of truth, not a parallel one.
///
/// implements: job-is-the-unit-not-rpc
/// implements: agent-self-sufficiency
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outcome {
    pub status: JobStatus,
    /// The authority this job resolved under.
    pub verdict: Verdict,
    /// The absolute, symlink-resolved directory it was confined to.
    pub resolved_cwd: PathBuf,
}

impl Outcome {
    /// The exit code, if the job exited normally.
    pub fn code(&self) -> Option<i32> {
        match self.status {
            JobStatus::Done { code } => Some(code),
            _ => None,
        }
    }
}
