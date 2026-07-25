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

/// Process-global sequence, used to name the per-registry capture directory.
static CAPTURE_SEQ: AtomicU64 = AtomicU64::new(0);

/// The live bookkeeping for one job.
struct JobHandle {
    /// The running child, taken once the job reaches a terminal state.
    child: Option<Child>,
    /// Path of the file its stdout+stderr are captured to. Kept for cleanup;
    /// output is always read and written through the handles below, never by
    /// re-opening this path (which would be a swap race).
    capture: PathBuf,
    /// Write handle handed to the child. Held open from job creation so a spawn
    /// never has to re-open the capture by name.
    capture_write: File,
    /// Read handle used by `tail`, cloned and seeked per call.
    capture_read: File,
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
    /// The private 0700 directory holding this registry's capture files,
    /// created on first use.
    capture_dir: Option<PathBuf>,
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

impl Drop for Inner {
    fn drop(&mut self) {
        if let Some(dir) = self.capture_dir.take() {
            let _ = std::fs::remove_dir_all(dir);
        }
    }
}

/// Create this registry's private capture directory.
///
/// Captures must not live directly in a world-writable temp dir: a job running
/// as the daemon's own uid could pre-create a predictable capture path as a
/// symlink and have the daemon truncate and write through it. A fresh
/// owner-only directory plus `create_new` on every capture closes that.
fn make_capture_dir() -> Result<PathBuf, JobError> {
    let dir = std::env::temp_dir().join(format!(
        "moco-job-{}-{}",
        std::process::id(),
        CAPTURE_SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        // mode at creation, not a later chmod: no window where it is group/world readable.
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&dir)
            .map_err(JobError::Io)?;
    }
    #[cfg(not(unix))]
    {
        std::fs::create_dir(&dir).map_err(JobError::Io)?;
    }
    Ok(dir)
}

/// Resolve `cwd` to an absolute, symlink-resolved path confined within `root`.
///
/// Note this is the *starting* directory only — a spawned process may `chdir`
/// elsewhere. The rule-set, not the cwd, is the real containment.
///
/// implements: argv-not-shell (the cwd-confinement half)
fn confine(cwd: &Path, root: &Path) -> Result<PathBuf, String> {
    let root = root
        .canonicalize()
        .map_err(|e| format!("allowed root '{}' does not resolve: {e}", root.display()))?;
    let resolved = cwd
        .canonicalize()
        .map_err(|e| format!("cwd '{}' does not resolve: {e}", cwd.display()))?;
    // `Path::starts_with` compares whole components, so `/tmp/foobar` is NOT
    // inside `/tmp/foo`. Do not replace this with a string prefix test.
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

/// Resolve `program` the way the spawn will: a name containing a separator is
/// used as given (relative to the job's `cwd`), a bare name is searched on PATH.
/// Mirrors `execvp`, so preflight names the binary that would actually run.
fn resolve_program(program: &str, cwd: &Path) -> Option<PathBuf> {
    if program.contains('/') {
        let p = Path::new(program);
        let candidate = if p.is_absolute() {
            p.to_path_buf()
        } else {
            cwd.join(p)
        };
        return is_executable(&candidate).then_some(candidate);
    }
    std::env::split_paths(&std::env::var_os("PATH")?).find_map(|dir| {
        let candidate = dir.join(program);
        is_executable(&candidate).then_some(candidate)
    })
}

/// Spawn `argv` directly into an already-open, file-backed capture. Never goes
/// through a shell, and never re-opens the capture by path.
///
/// implements: argv-not-shell
/// implements: job-durability-both-kill-vectors (file-backed stdio half)
fn spawn_child(argv: &[String], cwd: &Path, capture: &File) -> Result<Child, JobError> {
    let (program, args) = argv.split_first().ok_or(JobError::EmptyArgv)?;

    let stdout = capture.try_clone().map_err(JobError::Io)?;
    let stderr = capture.try_clone().map_err(JobError::Io)?;

    Command::new(program)
        .args(args)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr))
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
pub struct JobRegistry {
    inner: Mutex<Inner>,
    /// The node's governance. `None` means an ungoverned registry: a trusted
    /// local caller (e.g. supervised dev processes) runs without a gate. When
    /// set, every `start` passes the gate first, and an argv matching no rule
    /// fails closed.
    ///
    /// Deliberately no `Default`/`new()`: an ungoverned registry must be asked
    /// for by name so it can never be produced by `..Default::default()`.
    policy: Option<NodePolicy>,
}

impl JobRegistry {
    /// An **ungoverned** registry — the bare substrate, no rule-set in front,
    /// for a trusted local caller (e.g. supervised dev processes).
    pub fn ungoverned() -> Self {
        Self {
            inner: Mutex::new(Inner::default()),
            policy: None,
        }
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

    /// The disposition this registry gives an argv.
    fn disposition(&self, argv: &[String]) -> Disposition {
        match &self.policy {
            Some(policy) => policy.rules.evaluate(argv),
            // Ungoverned: nothing gates the spawn.
            None => Disposition::Allow,
        }
    }

    /// Resolve what a real run *would* do — without queuing or running anything.
    ///
    /// Every field is reported, so one read-only call surfaces every problem at
    /// once instead of costing a human approval per round-trip.
    ///
    /// implements: agent-self-sufficiency
    pub fn preflight(&self, argv: &[String], cwd: &Path) -> Preflight {
        let (resolved_cwd, cwd_error) = match &self.policy {
            Some(policy) => match confine(cwd, &policy.allowed_root) {
                Ok(path) => (Some(path), None),
                Err(msg) => (None, Some(msg)),
            },
            None => (cwd.canonicalize().ok(), None),
        };

        // Resolve a relative program against the cwd the job would actually get.
        let resolve_base = resolved_cwd.clone().unwrap_or_else(|| cwd.to_path_buf());

        Preflight {
            disposition: self.disposition(argv),
            program: argv
                .first()
                .and_then(|p| resolve_program(p, &resolve_base)),
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
        let cwd = self.confined_cwd(&req.cwd)?;
        let disposition = self.disposition(&req.argv);

        let mut inner = self.locked();
        let id = JobId(inner.next_id);
        inner.next_id += 1;

        let (capture, capture_write, capture_read) = Self::create_capture(&mut inner)?;

        let request = JobRequest {
            argv: req.argv,
            cwd,
            deadline: req.deadline,
        };

        let mut handle = JobHandle {
            child: None,
            capture,
            capture_write,
            capture_read,
            status: JobStatus::PendingApproval,
            deadline: request.deadline,
            started: Instant::now(),
            killed: false,
            pending: None,
        };

        match disposition {
            Disposition::Allow => {
                handle.child = Some(spawn_child(
                    &request.argv,
                    &request.cwd,
                    &handle.capture_write,
                )?);
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

    /// Resolve a request's cwd under the node's confinement, if any.
    fn confined_cwd(&self, cwd: &Path) -> Result<PathBuf, JobError> {
        match &self.policy {
            Some(policy) => confine(cwd, &policy.allowed_root).map_err(|_| JobError::CwdEscape {
                cwd: cwd.display().to_string(),
                root: policy.allowed_root.display().to_string(),
            }),
            None => Ok(cwd.to_path_buf()),
        }
    }

    /// Create a capture file inside this registry's private directory, opened
    /// with `create_new` so an existing path (a planted symlink) is refused.
    fn create_capture(inner: &mut Inner) -> Result<(PathBuf, File, File), JobError> {
        let dir = match &inner.capture_dir {
            Some(dir) => dir.clone(),
            None => {
                let dir = make_capture_dir()?;
                inner.capture_dir = Some(dir.clone());
                dir
            }
        };
        let path = dir.join(format!("{}.log", inner.next_id));
        let write = File::options()
            .create_new(true)
            .append(true)
            .open(&path)
            .map_err(JobError::Io)?;
        let read = File::open(&path).map_err(JobError::Io)?;
        Ok((path, write, read))
    }

    /// Record a human's decision on a job awaiting approval, transitioning it to
    /// `running` (spawning it, with any corrected argv) or `denied`.
    ///
    /// **This is the console's capability, not the requesting agent's.** It is
    /// the human side of the gate; when the hub lane lands it must sit behind
    /// the operator's principal, never on the surface an agent can reach.
    ///
    /// implements: approval-is-a-job-state
    pub fn decide(&self, id: &JobId, decision: Decision) -> Result<(), JobError> {
        // An edited argv is a fresh proposal: re-evaluate it. A per-instance
        // approval must never override a node-owned standing *deny*.
        let edited_disposition = match &decision {
            Decision::AllowOnce {
                edited_argv: Some(argv),
            } => Some(self.disposition(argv)),
            _ => None,
        };
        let approval_timeout = self.policy.as_ref().map(|p| p.approval_timeout);

        let mut inner = self.locked();
        let handle = inner
            .jobs
            .get_mut(id)
            .ok_or_else(|| JobError::NotFound(id.clone()))?;

        if handle.status != JobStatus::PendingApproval {
            return Err(JobError::NotPending(id.clone()));
        }

        // The decision arrived too late: fail closed rather than spawn.
        if approval_timeout.is_some_and(|t| handle.started.elapsed() >= t) {
            handle.status = JobStatus::Denied(DeniedReason::NoApprover);
            handle.pending = None;
            return Ok(());
        }

        if edited_disposition == Some(Disposition::Deny) {
            handle.status = JobStatus::Denied(DeniedReason::Rule);
            handle.pending = None;
            return Ok(());
        }

        let request = handle
            .pending
            .take()
            .ok_or_else(|| JobError::NotPending(id.clone()))?;

        match decision {
            Decision::DenyOnce => {
                handle.status = JobStatus::Denied(DeniedReason::Decision);
            }
            Decision::AllowOnce { edited_argv } => {
                // An edited argv is the correction back-channel: it, not the
                // proposal, is what actually runs.
                let argv = edited_argv.unwrap_or(request.argv);
                // Re-confine immediately before spawning: the cwd was resolved
                // when the job was proposed, possibly long ago. This shrinks —
                // but cannot fully close — the window in which a path component
                // is swapped for a symlink out of the allowed root.
                let cwd = match self.confined_cwd(&request.cwd) {
                    Ok(cwd) => cwd,
                    Err(e) => {
                        handle.status = JobStatus::Denied(DeniedReason::Rule);
                        return Err(e);
                    }
                };
                match spawn_child(&argv, &cwd, &handle.capture_write) {
                    Ok(child) => {
                        handle.child = Some(child);
                        handle.status = JobStatus::Running;
                        // The execution deadline runs from the spawn, not from
                        // the moment the job entered the approval queue.
                        handle.started = Instant::now();
                    }
                    Err(e) => {
                        // Never leave the job pending with its decision consumed:
                        // that would strand it until the approval timeout and
                        // report the wrong reason.
                        handle.status = JobStatus::Denied(DeniedReason::Decision);
                        return Err(e);
                    }
                }
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

        // Read through the handle opened at job creation — never re-open by
        // path, which a swapped file could hijack.
        let mut file = handle.capture_read.try_clone().map_err(JobError::Io)?;
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
            // A child taken out under the lock and reaped after releasing it:
            // never block the whole registry on another process exiting.
            let mut expired: Option<Child> = None;
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
                        handle.status = JobStatus::TimedOut;
                        expired = handle.child.take();
                    }
                }
            }

            if let Some(mut child) = expired {
                let _ = child.kill();
                let _ = child.wait();
                return Ok(Outcome {
                    status: JobStatus::TimedOut,
                });
            }
            std::thread::sleep(POLL);
        }
    }

    /// Terminate a job. A running job is signalled and `wait` then reports it
    /// `Killed`; a job still awaiting approval is withdrawn rather than left
    /// pending until the approval timeout.
    pub fn kill(&self, id: &JobId) -> Result<(), JobError> {
        let mut inner = self.locked();
        let handle = inner
            .jobs
            .get_mut(id)
            .ok_or_else(|| JobError::NotFound(id.clone()))?;

        if handle.status == JobStatus::PendingApproval {
            handle.status = JobStatus::Denied(DeniedReason::Decision);
            handle.pending = None;
            return Ok(());
        }

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
