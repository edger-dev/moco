use crate::error::JobError;
use crate::job::{JobId, JobRequest, Outcome, Tail};

/// The registry every job lives on: it owns their ids, live output, control
/// handles, and terminal records. A command run through it is an ordinary job
/// on this registry — that is the whole point (a command *is* a process with a
/// governance gate in front, not a special RPC).
///
/// v1 is single-node and in-process. The surface below is transport-agnostic so
/// the hub lane can drop in later without changing callers.
///
/// implements: governed-command-is-a-job
/// implements: job-is-the-unit-not-rpc
#[derive(Default)]
pub struct JobRegistry {
    // v1 scaffold: the in-process job table lands with the implementation.
}

impl JobRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Spawn `req` as a detached, file-backed job and return its id immediately.
    /// The program is executed directly from its argv — never via a shell.
    ///
    /// implements: job-is-the-unit-not-rpc
    /// implements: argv-not-shell
    pub fn start(&self, _req: JobRequest) -> Result<JobId, JobError> {
        Err(JobError::NotImplemented)
    }

    /// Read output incrementally from `offset`, with the job's live status.
    pub fn tail(&self, _id: &JobId, _offset: u64) -> Result<Tail, JobError> {
        Err(JobError::NotImplemented)
    }

    /// Block until the job is terminal and return its outcome. Resumable after a
    /// disconnect by id — the efficient collect, not a busy-poll.
    pub fn wait(&self, _id: &JobId) -> Result<Outcome, JobError> {
        Err(JobError::NotImplemented)
    }

    /// Terminate a running job.
    pub fn kill(&self, _id: &JobId) -> Result<(), JobError> {
        Err(JobError::NotImplemented)
    }

    /// Blocking sugar over `start` + `wait`, for callers who want a simple
    /// result shape. A job id still exists underneath, so a dropped connection
    /// is recovered by `wait(id)`.
    pub fn run(&self, req: JobRequest) -> Result<Outcome, JobError> {
        let id = self.start(req)?;
        self.wait(&id)
    }
}
