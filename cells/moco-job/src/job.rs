use std::fmt;
use std::path::PathBuf;
use std::time::Duration;

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

/// The lifecycle state of a job. v1 substrate states only; the governance
/// states (`pending-approval`, `denied`) arrive in Phase 2.
///
/// implements: job-is-the-unit-not-rpc
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JobStatus {
    Running,
    Done { code: i32 },
    Killed,
    TimedOut,
}

impl JobStatus {
    /// A terminal state is any state the job will not leave.
    pub fn is_terminal(&self) -> bool {
        !matches!(self, JobStatus::Running)
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
    /// Optional execution deadline; a job that outlives it lands `TimedOut`.
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

/// A job's terminal record — the fourth property of a job. In later phases this
/// grows into a self-describing outcome (the resolved cwd and the verdict it ran
/// under); v1 carries the terminal status.
///
/// implements: job-is-the-unit-not-rpc
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outcome {
    pub status: JobStatus,
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
