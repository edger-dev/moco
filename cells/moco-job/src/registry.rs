use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::process::{Child, Command, Stdio};
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

use crate::error::JobError;
use crate::job::{JobId, JobRequest, JobStatus, Outcome, Tail};

/// How often `wait` polls a running child for exit / deadline.
const POLL: Duration = Duration::from_millis(10);

/// The live bookkeeping for one job.
struct JobHandle {
    /// The running child, taken once the job reaches a terminal state.
    child: Option<Child>,
    /// Path of the file its stdout+stderr are captured to (the tail source).
    capture: std::path::PathBuf,
    status: JobStatus,
    deadline: Option<Duration>,
    started: Instant,
    /// Set when the job was explicitly killed, so `wait` reports `Killed`
    /// rather than an exit code.
    killed: bool,
}

#[derive(Default)]
struct Inner {
    next_id: u64,
    jobs: HashMap<JobId, JobHandle>,
}

/// The registry every job lives on: it owns their ids, live output, control
/// handles, and terminal records. A command run through it is an ordinary job
/// on this registry — that is the whole point (a command *is* a process, not a
/// special RPC).
///
/// v1 is single-node and in-process. The surface below is transport-agnostic so
/// the hub lane can drop in later without changing callers.
///
/// implements: governed-command-is-a-job
/// implements: job-is-the-unit-not-rpc
#[derive(Default)]
pub struct JobRegistry {
    inner: Mutex<Inner>,
}

impl JobRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Lock the inner state, recovering the guard if a holder panicked (a
    /// poisoned mutex is not a reason to panic here).
    fn locked(&self) -> MutexGuard<'_, Inner> {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Spawn `req` as a detached, file-backed job and return its id immediately.
    /// The program is executed directly from its argv — never via a shell — and
    /// its output is captured to a file (stdin from `/dev/null`), so the child
    /// never dies of a broken pipe to us.
    ///
    /// implements: job-is-the-unit-not-rpc
    /// implements: argv-not-shell
    /// implements: job-durability-both-kill-vectors (file-backed stdio half)
    pub fn start(&self, req: JobRequest) -> Result<JobId, JobError> {
        let JobRequest {
            argv,
            cwd,
            deadline,
        } = req;
        let (program, args) = argv.split_first().ok_or(JobError::EmptyArgv)?;

        let mut inner = self.locked();
        let id = JobId(inner.next_id);
        inner.next_id += 1;

        let capture =
            std::env::temp_dir().join(format!("moco-job-{}-{}.log", std::process::id(), id.0));
        let file = File::create(&capture).map_err(JobError::Io)?;
        let stdout = file.try_clone().map_err(JobError::Io)?;

        let child = Command::new(program)
            .args(args)
            .current_dir(&cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::from(stdout))
            .stderr(Stdio::from(file))
            .spawn()
            .map_err(|source| JobError::Spawn {
                program: program.clone(),
                source,
            })?;

        inner.jobs.insert(
            id.clone(),
            JobHandle {
                child: Some(child),
                capture,
                status: JobStatus::Running,
                deadline,
                started: Instant::now(),
                killed: false,
            },
        );
        Ok(id)
    }

    /// Read output incrementally from `offset`, with the job's live status.
    pub fn tail(&self, id: &JobId, offset: u64) -> Result<Tail, JobError> {
        let inner = self.locked();
        let handle = inner
            .jobs
            .get(id)
            .ok_or_else(|| JobError::NotFound(id.clone()))?;

        let mut file = File::open(&handle.capture).map_err(JobError::Io)?;
        file.seek(SeekFrom::Start(offset)).map_err(JobError::Io)?;
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes).map_err(JobError::Io)?;

        Ok(Tail {
            next_offset: offset + bytes.len() as u64,
            bytes,
            status: handle.status.clone(),
        })
    }

    /// Block until the job is terminal and return its outcome. Resumable after a
    /// disconnect by id — the efficient collect, not a busy-poll on the caller's
    /// side.
    pub fn wait(&self, id: &JobId) -> Result<Outcome, JobError> {
        loop {
            {
                let mut inner = self.locked();
                let handle = inner
                    .jobs
                    .get_mut(id)
                    .ok_or_else(|| JobError::NotFound(id.clone()))?;

                if handle.status.is_terminal() {
                    return Ok(Outcome {
                        status: handle.status.clone(),
                    });
                }

                if let Some(child) = handle.child.as_mut() {
                    if handle
                        .deadline
                        .is_some_and(|dl| handle.started.elapsed() >= dl)
                    {
                        let _ = child.kill();
                        let _ = child.wait();
                        handle.status = JobStatus::TimedOut;
                        handle.child = None;
                        return Ok(Outcome {
                            status: JobStatus::TimedOut,
                        });
                    }

                    if let Some(exit) = child.try_wait().map_err(JobError::Io)? {
                        handle.status = if handle.killed {
                            JobStatus::Killed
                        } else {
                            JobStatus::Done {
                                code: exit.code().unwrap_or(-1),
                            }
                        };
                        handle.child = None;
                        return Ok(Outcome {
                            status: handle.status.clone(),
                        });
                    }
                }
            }
            std::thread::sleep(POLL);
        }
    }

    /// Terminate a running job; `wait` will then report it `Killed`.
    pub fn kill(&self, id: &JobId) -> Result<(), JobError> {
        let mut inner = self.locked();
        let handle = inner
            .jobs
            .get_mut(id)
            .ok_or_else(|| JobError::NotFound(id.clone()))?;

        if let Some(child) = handle.child.as_mut()
            && child.kill().is_ok()
        {
            handle.killed = true;
        }
        Ok(())
    }

    /// Blocking sugar over `start` + `wait`, for callers who want a simple
    /// result shape. A job id still exists underneath, so a dropped connection
    /// is recovered by `wait(id)`.
    pub fn run(&self, req: JobRequest) -> Result<Outcome, JobError> {
        let id = self.start(req)?;
        self.wait(&id)
    }
}
