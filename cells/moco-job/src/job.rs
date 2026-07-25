use std::fmt;
use std::path::PathBuf;
use std::time::Duration;

use facet::Facet;

use crate::audit::Verdict;

/// A job's identity — registry-assigned, unique, and addressable across a
/// disconnect. The load-bearing first property of a job.
///
/// implements: job-is-the-unit-not-rpc
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct JobId(pub u64);

impl fmt::Display for JobId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "job-{}", self.0)
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
    /// Never ran, and never will.
    ///
    /// A struct variant, not a tuple one: a tuple variant wrapping another enum
    /// does not round-trip through Styx (`@Denied@Rule` is not re-readable).
    Denied {
        reason: DeniedReason,
    },
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
        }
    }

    pub fn with_deadline(mut self, deadline: Duration) -> Self {
        self.deadline = Some(deadline);
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
