use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

use crate::error::JobError;
use crate::job::{DeniedReason, JobId, JobRequest, JobStatus, Outcome, Tail};
use crate::preflight::Preflight;
use crate::rules::{Decision, Disposition, NodePolicy};

/// How often `wait` polls a running child for exit / deadline / decision.
const POLL: Duration = Duration::from_millis(10);

/// Process-global sequence for capture-file names. A `JobId` is only unique
/// within its registry, so it cannot name the file — two registries in one
/// process would collide on `job 0`. This never resets.
static CAPTURE_SEQ: AtomicU64 = AtomicU64::new(0);

/// The live bookkeeping for one job.
struct JobHandle {
    /// The running child, taken once the job reaches a terminal state.
    child: Option<Child>,
    /// Path of the file its stdout+stderr are captured to (the tail source).
    capture: PathBuf,
    status: JobStatus,
    deadline: Option<Duration>,
    /// When the current phase began: entry to `pending` for a job awaiting a
    /// decision, or the spawn instant once it is running.
    started: Instant,
    /// Set when the job was explicitly killed, so `wait` reports `Killed`
    /// rather than an exit code.
    killed: bool,
    /// The request held back while the job awaits approval. `Some` exactly when
    /// the status is `PendingApproval`; this is what a decision spawns.
    pending: Option<JobRequest>,
}

#[derive(Default)]
struct Inner {
    next_id: u64,
    // v1: jobs are retained for the registry's lifetime (no eviction yet). Their
    // capture files and any unreaped children are released on JobHandle drop.
    jobs: HashMap<JobId, JobHandle>,
}

impl Drop for JobHandle {
    fn drop(&mut self) {
        // Reap a still-running child so teardown leaves no zombie, then remove
        // the file-backed capture. For a job already waited to a terminal state
        // `child` is None, so this just deletes the file.
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        let _ = std::fs::remove_file(&self.capture);
    }
}

/// Resolve `cwd` to an absolute, symlink-resolved path confined within `root`.
///
/// implements: argv-not-shell (the cwd-confinement half)
fn confine(cwd: &Path, root: &Path) -> Result<PathBuf, String> {
    let root = root
        .canonicalize()
        .map_err(|e| format!("allowed root '{}' does not resolve: {e}", root.display()))?;
    let resolved = cwd
        .canonicalize()
        .map_err(|e| format!("cwd '{}' does not resolve: {e}", cwd.display()))?;
    if resolved.starts_with(&root) {
        Ok(resolved)
    } else {
        Err(format!(
            "cwd '{}' resolves outside allowed root '{}'",
            resolved.display(),
            root.display()
        ))
    }
}

/// Is this path an executable file?
#[cfg(unix)]
fn is_executable(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(p)
        .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable(p: &Path) -> bool {
    p.is_file()
}

/// The PATH the daemon would spawn with.
fn effective_path() -> String {
    std::env::var("PATH").unwrap_or_default()
}

/// Resolve `program` the way a spawn would: a path with a separator is taken
/// as-is, a bare name is searched on PATH.
fn resolve_program(program: &str) -> Option<PathBuf> {
    let p = Path::new(program);
    if p.components().count() > 1 {
        return is_executable(p).then(|| p.to_path_buf());
    }
    std::env::split_paths(&std::env::var_os("PATH")?).find_map(|dir| {
        let candidate = dir.join(program);
        is_executable(&candidate).then_some(candidate)
    })
}

/// Spawn `argv` directly into a file-backed capture. Never goes through a shell.
///
/// implements: argv-not-shell
/// implements: job-durability-both-kill-vectors (file-backed stdio half)
fn spawn_child(argv: &[String], cwd: &Path, capture: &Path) -> Result<Child, JobError> {
    let (program, args) = argv.split_first().ok_or(JobError::EmptyArgv)?;

    let file = File::options()
        .append(true)
        .open(capture)
        .map_err(JobError::Io)?;
    // stdout and stderr share one file description (via try_clone / dup), so
    // their writes advance a single offset and interleave in causal order.
    // Do NOT split this into two separately-opened files — that races the
    // offset and corrupts the capture.
    let stdout = file.try_clone().map_err(JobError::Io)?;

    Command::new(program)
        .args(args)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(file))
        .spawn()
        .map_err(|source| JobError::Spawn {
            program: program.clone(),
            source,
        })
}

/// The registry every job lives on: it owns their ids, live output, control
/// handles, and terminal records. A governed command is an ordinary job on this
/// registry with a rule-set evaluated before it spawns — that is the whole point
/// (a command *is* a process with a gate in front, not a special RPC).
///
/// v1 is single-node and in-process. The surface below is transport-agnostic so
/// the hub lane can drop in later without changing callers.
///
/// implements: governed-command-is-a-job
/// implements: job-is-the-unit-not-rpc
#[derive(Default)]
pub struct JobRegistry {
    inner: Mutex<Inner>,
    /// The node's governance. `None` means an ungoverned registry: a trusted
    /// local caller (e.g. supervised dev processes) runs without a gate. When
    /// set, every `start` passes the gate first, and an argv matching no rule
    /// fails closed.
    policy: Option<NodePolicy>,
}

impl JobRegistry {
    /// An ungoverned registry — the bare substrate, for a trusted local caller.
    pub fn new() -> Self {
        Self::default()
    }

    /// A registry governed by `policy`: every `start` is gated before it spawns.
    ///
    /// implements: governed-command-is-a-job
    pub fn with_policy(policy: NodePolicy) -> Self {
        Self {
            inner: Mutex::new(Inner::default()),
            policy: Some(policy),
        }
    }

    /// The node's governance, for read-only introspection. The agent may see
    /// what a human already granted; that is not the power to grant.
    ///
    /// implements: agent-self-sufficiency
    pub fn policy(&self) -> Option<&NodePolicy> {
        self.policy.as_ref()
    }

    /// Lock the inner state, recovering the guard if a holder panicked (a
    /// poisoned mutex is not a reason to panic here).
    fn locked(&self) -> MutexGuard<'_, Inner> {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Resolve what a real run *would* do — without queuing or running anything.
    ///
    /// Every field is reported, so one read-only call surfaces every problem at
    /// once instead of costing a human approval per round-trip.
    ///
    /// implements: agent-self-sufficiency
    pub fn preflight(&self, argv: &[String], cwd: &Path) -> Preflight {
        let disposition = match &self.policy {
            Some(policy) => policy.rules.evaluate(argv),
            // Ungoverned: nothing gates the spawn.
            None => Disposition::Allow,
        };

        let (resolved_cwd, cwd_error) = match &self.policy {
            Some(policy) => match confine(cwd, &policy.allowed_root) {
                Ok(path) => (Some(path), None),
                Err(msg) => (None, Some(msg)),
            },
            None => (cwd.canonicalize().ok(), None),
        };

        Preflight {
            disposition,
            program: argv.first().and_then(|p| resolve_program(p)),
            effective_path: effective_path(),
            resolved_cwd,
            cwd_error,
        }
    }

    /// Evaluate the gate and, if permitted, spawn `req` as a detached,
    /// file-backed job; return its id immediately either way.
    ///
    /// A denied argv is registered as a job in `Denied` state and never spawns;
    /// an argv matching no rule is registered `PendingApproval` and returns its
    /// id *without* spawning. A malformed request (empty argv, a cwd escaping
    /// the node's allowed root) is an error rather than a job.
    ///
    /// implements: job-is-the-unit-not-rpc
    /// implements: argv-not-shell
    /// implements: governed-command-is-a-job
    /// implements: approval-is-a-job-state
    pub fn start(&self, req: JobRequest) -> Result<JobId, JobError> {
        if req.argv.is_empty() {
            return Err(JobError::EmptyArgv);
        }

        // Confine the cwd before anything else: a request that cannot name a
        // legal working directory never becomes a job.
        let cwd = match &self.policy {
            Some(policy) => confine(&req.cwd, &policy.allowed_root).map_err(|_| {
                JobError::CwdEscape {
                    cwd: req.cwd.display().to_string(),
                    root: policy.allowed_root.display().to_string(),
                }
            })?,
            None => req.cwd.clone(),
        };

        let disposition = match &self.policy {
            Some(policy) => policy.rules.evaluate(&req.argv),
            None => Disposition::Allow,
        };

        let mut inner = self.locked();
        let id = JobId(inner.next_id);
        inner.next_id += 1;

        let seq = CAPTURE_SEQ.fetch_add(1, Ordering::Relaxed);
        let capture =
            std::env::temp_dir().join(format!("moco-job-{}-{}.log", std::process::id(), seq));
        // Create the capture up front so `tail` works uniformly, including for a
        // job that is pending or denied and so has produced nothing.
        File::create(&capture).map_err(JobError::Io)?;

        let request = JobRequest {
            argv: req.argv,
            cwd,
            deadline: req.deadline,
        };

        let mut handle = JobHandle {
            child: None,
            capture,
            status: JobStatus::PendingApproval,
            deadline: request.deadline,
            started: Instant::now(),
            killed: false,
            pending: None,
        };

        match disposition {
            Disposition::Allow => {
                handle.child = Some(spawn_child(&request.argv, &request.cwd, &handle.capture)?);
                handle.status = JobStatus::Running;
                handle.started = Instant::now();
            }
            Disposition::Deny => {
                handle.status = JobStatus::Denied(DeniedReason::Rule);
            }
            Disposition::NeedsApproval => {
                handle.status = JobStatus::PendingApproval;
                handle.pending = Some(request);
            }
        }

        inner.jobs.insert(id.clone(), handle);
        Ok(id)
    }

    /// Record a human's decision on a job awaiting approval, transitioning it to
    /// `running` (spawning it, with any corrected argv) or `denied`.
    ///
    /// implements: approval-is-a-job-state
    pub fn decide(&self, id: &JobId, decision: Decision) -> Result<(), JobError> {
        let mut inner = self.locked();
        let handle = inner
            .jobs
            .get_mut(id)
            .ok_or_else(|| JobError::NotFound(id.clone()))?;

        if handle.status != JobStatus::PendingApproval {
            return Err(JobError::NotPending(id.clone()));
        }
        let request = handle.pending.take().ok_or(JobError::NotPending(id.clone()))?;

        match decision {
            Decision::DenyOnce => {
                handle.status = JobStatus::Denied(DeniedReason::Decision);
            }
            Decision::AllowOnce { edited_argv } => {
                // An edited argv is the correction back-channel: it, not the
                // proposal, is what actually runs.
                let argv = edited_argv.unwrap_or(request.argv);
                handle.child = Some(spawn_child(&argv, &request.cwd, &handle.capture)?);
                handle.status = JobStatus::Running;
                // The execution deadline runs from the spawn, not from the
                // moment the job entered the approval queue.
                handle.started = Instant::now();
            }
        }
        Ok(())
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
    ///
    /// A job awaiting approval blocks here until it is decided, or until the
    /// node's approval timeout elapses — at which point it is **denied**. It is
    /// never run without a decision, and never hangs forever.
    ///
    /// implements: rule-set-target-owned-human-mutated (the fail-closed half)
    pub fn wait(&self, id: &JobId) -> Result<Outcome, JobError> {
        loop {
            {
                let mut inner = self.locked();
                let approval_timeout = self.policy.as_ref().map(|p| p.approval_timeout);
                let handle = inner
                    .jobs
                    .get_mut(id)
                    .ok_or_else(|| JobError::NotFound(id.clone()))?;

                if handle.status.is_terminal() {
                    return Ok(Outcome {
                        status: handle.status.clone(),
                    });
                }

                // Fail closed: nobody decided in time.
                if handle.status == JobStatus::PendingApproval
                    && approval_timeout.is_some_and(|t| handle.started.elapsed() >= t)
                {
                    handle.status = JobStatus::Denied(DeniedReason::NoApprover);
                    handle.pending = None;
                    return Ok(Outcome {
                        status: handle.status.clone(),
                    });
                }

                if let Some(child) = handle.child.as_mut() {
                    // Check for a real exit first: a job that finished on its own
                    // reports its true outcome even if it also crossed its
                    // deadline in the same tick.
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

        // Record operator intent regardless of the signal's result: `killed`
        // is why `wait` reports `Killed` rather than an exit code. A terminal
        // job has `child == None`, so killing it is a no-op.
        if let Some(child) = handle.child.as_mut() {
            handle.killed = true;
            let _ = child.kill();
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
