use std::collections::{HashMap, VecDeque};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use crate::admission;
use crate::audit::{AuditRecord, AuditSink, MemoryAuditLog, Verdict};
use crate::error::JobError;
use crate::job::{DeniedReason, JobId, JobRequest, JobStatus, Outcome, Tail};
use crate::lens::{HumanView, LensSource, MachineRead, ScreenRead, ScreenSource};
use crate::lifecycle::{Lifetime, RestartPolicy};
use crate::manifest::Manifest;
use crate::port::{self, PortRange};
use crate::preflight::Preflight;
use crate::procfs::{self, Liveness};
use crate::record::{RecordStore, record_of};
use crate::rules::{Decision, Disposition, NodePolicy};
use crate::scope::{Caller, Scope};
use crate::stats::{Breach, Limits, Sample, Stats};

/// Discard the oldest part of a capture that has outgrown its cap.
///
/// Keeps the **most recent** bytes: the newest output is what someone is
/// looking at, and dropping the tail to preserve the beginning would throw away
/// exactly the part being watched.
///
/// Returns how many bytes went, which the caller adds to the job's `dropped`
/// count. That count is the whole difference between a *logical* offset — total
/// bytes ever written — and a position in the file, and keeping it is what stops
/// compaction from silently redirecting a reader's resume point onto different
/// content.
///
/// The capture is opened `O_APPEND`, so a child writing concurrently always
/// lands after whatever is there: rewriting the file underneath it is safe.
/// Bytes written between the read and the truncate are lost — a small race,
/// bounded by one compaction, on data already destined to be discarded.
fn compact(path: &Path, cap: u64) -> Result<u64, JobError> {
    let len = match std::fs::metadata(path) {
        Ok(meta) => meta.len(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(e) => return Err(JobError::Io(e)),
    };
    if len <= cap {
        return Ok(0);
    }

    let keep = (cap * KEEP_NUMERATOR / KEEP_DENOMINATOR).max(1);
    let drop_from_front = len.saturating_sub(keep);

    let mut file = File::open(path).map_err(JobError::Io)?;
    file.seek(SeekFrom::Start(drop_from_front))
        .map_err(JobError::Io)?;
    let mut kept = Vec::with_capacity(keep as usize);
    file.read_to_end(&mut kept).map_err(JobError::Io)?;
    drop(file);

    // Truncate and rewrite in place: the child's `O_APPEND` fd keeps working,
    // and its next write lands after what we just kept.
    let mut out = File::options()
        .write(true)
        .truncate(true)
        .open(path)
        .map_err(JobError::Io)?;
    use std::io::Write;
    out.write_all(&kept).map_err(JobError::Io)?;
    Ok(drop_from_front)
}

/// Write a job's current state to its durable record.
///
/// Called at every transition, so a daemon that dies at any point leaves a
/// record a successor can act on. Writes are atomic, so there is no torn state
/// to recover from.
///
/// implements: registry-is-node-state-on-disk
fn persist(store: &RecordStore, id: &JobId, handle: &JobHandle) -> Result<(), JobError> {
    store.put(&record_of(
        id,
        &handle.argv,
        &handle.resolved_cwd,
        handle.verdict,
        &handle.status,
        &handle.scope,
        handle.name.as_deref(),
        handle.lifetime,
        handle.restart,
        handle.restarts,
        handle.port,
        &handle.machine_file,
        &handle.machine_format,
        handle.dropped,
        handle.external,
        handle.pid,
        handle.pid_start,
        &handle.capture,
        handle.deadline.map(|d| d.as_millis() as u64).unwrap_or(0),
        handle.audited,
        handle.limits,
    ))
}

/// How many recent samples a job keeps.
///
/// Sixty, which at the daemon's one-second tick is the last minute. Deliberately
/// short: this answers *what is happening now*, and a supervisor that grew into
/// a metrics store would be keeping data nobody queries at a cost everybody
/// pays. Anything wanting history should scrape this and store it elsewhere.
pub const SAMPLE_HISTORY: usize = 60;

/// The grid a terminal-lens job is given, and the grid its screen renders at.
///
/// **These are one number, not two that happen to match.** A job asks the
/// kernel how wide its terminal is and wraps its own output accordingly; if the
/// parser's grid disagreed, every long line would break in a place the job
/// never broke it, and the screen would be a plausible-looking fiction.
///
/// Wider and taller than the traditional 80x24 because of what actually runs
/// here — compiler diagnostics and test output, which 80 columns folds into an
/// unreadable zigzag. A declaration-level override is the escape hatch when a
/// job needs a specific size.
pub const SCREEN_COLS: u16 = 120;
pub const SCREEN_ROWS: u16 = 40;

/// Drop trailing blank rows from a rendered screen.
///
/// A 40-row grid holding four lines of output is thirty-six empty lines of
/// padding, which costs an agent tokens to receive and tells it nothing.
/// Interior blank lines are kept — those are layout the job chose.
fn trim_blank_rows(text: &str) -> String {
    let mut lines: Vec<&str> = text.lines().collect();
    while lines.last().is_some_and(|l| l.trim().is_empty()) {
        lines.pop();
    }
    lines.join("\n")
}

/// How often `wait` polls a running child for exit / deadline / decision.
const POLL: Duration = Duration::from_millis(10);

/// How large a job's scrollback may grow before the oldest of it is discarded.
///
/// A redrawing TUI writes without bound — most of it superseded the instant it
/// lands — so an uncapped capture is a disk-filling bug waiting for a long
/// enough run.
pub const DEFAULT_CAPTURE_CAP: u64 = 4 * 1024 * 1024;

/// What a compaction keeps, as a fraction of the cap.
///
/// Well below the cap on purpose: compacting back to exactly the limit would
/// mean compacting again on the very next write, turning a bounded file into a
/// rewrite loop.
const KEEP_NUMERATOR: u64 = 3;
const KEEP_DENOMINATOR: u64 = 4;

/// The live bookkeeping for one job.
struct JobHandle {
    /// Advisory ceilings declared for this job.
    limits: Limits,
    /// The live screen fold for a terminal-lens job, fed by the same pump that
    /// writes the capture. `None` for a logs job, and for a **re-adopted**
    /// terminal job — its pty died with the previous daemon, so there is
    /// nothing left to fold and its screen has to be replayed instead.
    screen: Option<Arc<Mutex<vt100::Parser>>>,
    /// Recent resource readings, oldest first, bounded at [`SAMPLE_HISTORY`].
    samples: VecDeque<Sample>,
    /// The previous raw CPU total, so the next sample can be turned into a
    /// rate. `None` until the job has been sampled once.
    last_cpu: Option<(u64, Instant)>,
    /// The running child, taken once the job reaches a terminal state.
    ///
    /// `None` with a `Running` status means the job was **re-adopted**: it is
    /// alive but is no longer our child, so it is tracked by liveness probe
    /// rather than by `try_wait`.
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
    /// The authority this job resolved under.
    verdict: Verdict,
    /// The confined directory it runs in — echoed on the outcome and the record.
    resolved_cwd: PathBuf,
    /// Exactly the argv that ran (updated when an approval corrects it).
    argv: Vec<String>,
    /// Set once its terminal record has been written, so no attempt is audited
    /// twice however many times it is awaited.
    audited: bool,
    /// The child's pid, or 0 if it never spawned.
    pid: u32,
    /// The kernel start time of `pid`, so a **reused** pid is never mistaken for
    /// this job. 0 when unknown.
    pid_start: u64,
    /// Which workspace owns this job. Not the session that asked for it: a
    /// session restart must never orphan or kill a job.
    scope: Scope,
    /// The manifest name this job was declared under, if any. Ad-hoc jobs have
    /// none — a name is what a *declaration* gives you.
    name: Option<String>,
    lifetime: Lifetime,
    restart: RestartPolicy,
    /// How many times the supervisor has brought this job back.
    restarts: u64,
    /// The port the node allocated it, or 0 for none.
    port: u16,
    /// Whether a human watches this through a pty.
    human_view: HumanView,
    /// Bytes discarded from the front of the capture by compaction. The
    /// difference between a logical offset and a position in the file.
    dropped: u64,
    /// The declared machine-lens sidecar, relative to the job's directory.
    machine_file: String,
    /// What is in it.
    machine_format: String,
    /// Handed over rather than started here.
    external: bool,
    /// The environment variable the port arrives in.
    port_env: String,
    /// True when this handle was rebuilt from an on-disk record rather than
    /// spawned by this registry: we hold no child handle, so there is no exit
    /// code to reap and its end is reported `OutcomeUnknown`.
    adopted: bool,
}

struct Inner {
    // Jobs are retained for the registry's lifetime (no eviction yet); their
    // durable records and captures outlive it entirely.
    jobs: HashMap<JobId, JobHandle>,
    /// The on-disk home of this registry's records and captures.
    store: RecordStore,
}

impl JobHandle {
    /// The allocated port, if there is one.
    fn port_request(&self) -> Option<u16> {
        (self.port != 0).then_some(self.port)
    }

    /// Where the port is delivered.
    fn port_env_or_default(&self) -> &str {
        if self.port_env.is_empty() {
            crate::port::DEFAULT_PORT_ENV
        } else {
            &self.port_env
        }
    }
}

impl Drop for JobHandle {
    /// **A daemon going away must not kill a job.**
    ///
    /// Reap a child that has *already* exited so teardown leaves no zombie, but
    /// never signal a live one and never delete the capture: the whole point of
    /// the durable registry is that a job outlives the process that started it,
    /// and its scrollback is that file. Only an explicit stop signals.
    ///
    /// implements: registry-is-node-state-on-disk
    /// implements: job-durability-both-kill-vectors
    fn drop(&mut self) {
        if let Some(child) = self.child.as_mut() {
            let _ = child.try_wait();
        }
    }
}

/// Resolve `cwd` to an absolute, symlink-resolved path confined within `root`.
///
/// Note this is the *starting* directory only — a spawned process may `chdir`
/// elsewhere. The rule-set, not the cwd, is the real containment.
///
/// Marker distinguishing a non-UTF-8 cwd from a containment failure.
const NOT_UTF8: &str = "__moco_cwd_not_utf8__";

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
    if resolved.to_str().is_none() {
        // Guaranteed here so an audit record can name the directory exactly;
        // see JobError::CwdNotUtf8.
        return Err(NOT_UTF8.to_string());
    }
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

/// The self-describing terminal record of a job: where it ran and under what
/// authority, alongside its status.
///
/// implements: agent-self-sufficiency
fn outcome_of(handle: &JobHandle) -> Outcome {
    Outcome {
        status: handle.status.clone(),
        verdict: handle.verdict,
        resolved_cwd: handle.resolved_cwd.clone(),
    }
}

/// Spawn `argv` directly into an already-open, file-backed capture. Never goes
/// through a shell, and never re-opens the capture by path.
///
/// implements: argv-not-shell
/// implements: job-durability-both-kill-vectors (file-backed stdio half)
#[allow(clippy::too_many_arguments)]
fn spawn_child(
    argv: &[String],
    cwd: &Path,
    capture: &File,
    port: Option<u16>,
    port_env: &str,
    human_view: HumanView,
    screen: &mut Option<Arc<Mutex<vt100::Parser>>>,
) -> Result<Child, JobError> {
    let (program, args) = argv.split_first().ok_or(JobError::EmptyArgv)?;

    // Under a terminal lens the child's stdio is a **pty slave**, so `isatty` is
    // true and the job draws as it would for a person. The master is pumped into
    // the same capture file the log path uses, so scrollback stays file-backed —
    // which is what keeps `tail`, durability and re-adoption working unchanged,
    // and is why this does not reuse moco-tty's in-memory `ShellProcess`.
    //
    // Handing the slave to `Command` rather than forking by hand also keeps the
    // child a `std::process::Child`, so every reap, kill and settle path is the
    // same one the log lens uses. Two kinds of child in the registry would be
    // two of everything that touches one.
    let (stdin, stdout, stderr) = match human_view {
        HumanView::Logs => (
            Stdio::null(),
            Stdio::from(capture.try_clone().map_err(JobError::Io)?),
            Stdio::from(capture.try_clone().map_err(JobError::Io)?),
        ),
        HumanView::Terminal => {
            // **Tell the job how big its terminal is.** With no winsize the
            // kernel hands out zeros, and a job that asks gets 0x0 — so it
            // either refuses to draw or invents a size of its own, and either
            // way the screen we render would not be the screen it drew.
            let size = nix::pty::Winsize {
                ws_row: SCREEN_ROWS,
                ws_col: SCREEN_COLS,
                ws_xpixel: 0,
                ws_ypixel: 0,
            };
            let pty = nix::pty::openpty(&size, None)
                .map_err(|e| JobError::Audit(format!("could not allocate a pty: {e}")))?;
            let slave_err = pty.slave.try_clone().map_err(JobError::Io)?;
            let master = pty.master;
            let mut sink = capture.try_clone().map_err(JobError::Io)?;

            // **The screen is folded here, as the bytes go past.** The
            // alternative — replaying the capture on demand — would be wrong in
            // a way that is hard to see: the capture is bounded, so a job that
            // painted a banner once and went quiet would lose it the moment
            // compaction reached those bytes, and the screen would silently
            // disagree with the terminal. Folding live means the state outlives
            // the bytes it came from, and a read costs nothing.
            let parser = Arc::new(Mutex::new(vt100::Parser::new(SCREEN_ROWS, SCREEN_COLS, 0)));
            *screen = Some(parser.clone());

            // One pump per terminal job. It ends when the last slave fd closes,
            // which happens when the child and our copies are gone — so this
            // does not outlive the job it serves.
            std::thread::spawn(move || {
                use std::io::Write;
                let mut master = File::from(master);
                let mut buf = [0u8; 8192];
                loop {
                    match master.read(&mut buf) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => {
                            // The capture is written first: it is what
                            // durability, `tail` and re-adoption all rest on,
                            // and the screen is a convenience layered over it.
                            if sink.write_all(&buf[..n]).is_err() {
                                break;
                            }
                            if let Ok(mut parser) = parser.lock() {
                                parser.process(&buf[..n]);
                            }
                        }
                    }
                }
            });

            // **All three descriptors, not just the two that draw.** A program
            // asking whether it is on a terminal overwhelmingly asks about
            // stdin — `stty size` and most TUI setup do — so leaving stdin on
            // /dev/null would have the job querying a device that is not the
            // one it is drawing to, and getting a refusal.
            //
            // The consequence, stated rather than discovered: a job that reads
            // stdin now **blocks** instead of seeing EOF, exactly as it would
            // at a terminal nobody is typing at. There is no attach path yet to
            // type into it, so an interactive job waits forever — which is the
            // truthful behaviour for a terminal with no one at it.
            let slave_in = pty.slave.try_clone().map_err(JobError::Io)?;
            (
                Stdio::from(slave_in),
                Stdio::from(pty.slave),
                Stdio::from(slave_err),
            )
        }
    };

    let mut command = Command::new(program);
    if let Some(port) = port {
        // Delivery is env injection. `sh -c` is not involved, so nothing here
        // expands: a program that wants the port on its command line uses the
        // `@MOCO_PORT` argv token instead, substituted before we get here.
        command.env(port_env, port.to_string());
    }
    command
        .args(args)
        .current_dir(cwd)
        .stdin(stdin)
        .stdout(stdout)
        .stderr(stderr)
        .spawn()
        .map_err(|source| JobError::Spawn {
            program: program.clone(),
            searched_path: effective_path(),
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
    /// Where terminal records are appended. Always present, so every registry
    /// has a history even when no durable sink was configured.
    audit: Arc<dyn AuditSink>,
    /// How large a capture may grow before its oldest bytes are discarded.
    capture_cap: u64,
    /// What this node is called, for the host gate.
    ///
    /// **Injected, never read from the OS.** Matching `hostname` would make the
    /// engine call out to the system and drift from the mesh's own idea of what
    /// this node is named — which is the identity that actually routes.
    node: String,
}

impl JobRegistry {
    /// An **ungoverned** registry — the bare substrate, no rule-set in front,
    /// for a trusted local caller (e.g. supervised dev processes).
    ///
    /// Its state lands in a fresh private directory under the node's runtime
    /// root. Use [`JobRegistry::with_dir`] to point it at a shared node
    /// directory and re-adopt what is already there.
    pub fn ungoverned() -> Result<Self, JobError> {
        Ok(Self {
            inner: Mutex::new(Inner {
                jobs: HashMap::new(),
                store: RecordStore::private()?,
            }),
            policy: None,
            audit: Arc::new(MemoryAuditLog::new()),
            capture_cap: DEFAULT_CAPTURE_CAP,
            node: String::new(),
        })
    }

    /// A registry governed by `policy`: every `start` is gated before it spawns.
    ///
    /// implements: governed-command-is-a-job
    pub fn with_policy(policy: NodePolicy) -> Result<Self, JobError> {
        Ok(Self {
            inner: Mutex::new(Inner {
                jobs: HashMap::new(),
                store: RecordStore::private()?,
            }),
            policy: Some(policy),
            audit: Arc::new(MemoryAuditLog::new()),
            capture_cap: DEFAULT_CAPTURE_CAP,
            node: String::new(),
        })
    }

    /// Point this registry at a node's shared job directory, **re-adopting**
    /// every live job already recorded there.
    ///
    /// This is how a restarted daemon picks up the jobs its predecessor started:
    /// the records say what ran and with which `(pid, start-time)`, and a
    /// liveness probe decides which are still running. A job whose process is
    /// gone settles `OutcomeUnknown` — we are not its parent, so there is no
    /// exit code to collect and inventing one would be worse than saying so.
    ///
    /// implements: registry-is-node-state-on-disk
    pub fn with_dir(mut self, dir: impl Into<PathBuf>) -> Result<Self, JobError> {
        let store = RecordStore::open(dir)?;
        let mut jobs = HashMap::new();

        for record in store.all()? {
            let id = JobId(record.id.clone());
            let capture = PathBuf::from(&record.capture);
            // Re-open the capture so `tail` keeps working across the restart;
            // the scrollback *is* this file.
            let capture_write = File::options()
                .create(true)
                .append(true)
                .open(&capture)
                .map_err(JobError::Io)?;
            let capture_read = File::open(&capture).map_err(JobError::Io)?;

            // Only a job recorded as running can still be live; anything else
            // already reached its terminal state and keeps it.
            let status = if record.status == JobStatus::Running {
                match procfs::liveness(record.pid, record.pid_start) {
                    Liveness::Alive => JobStatus::Running,
                    // Gone, pid reused, or unprobeable: either way we cannot
                    // claim it is running and cannot know how it ended.
                    Liveness::Dead | Liveness::Unsupported => JobStatus::OutcomeUnknown,
                }
            } else {
                record.status.clone()
            };

            jobs.insert(
                id,
                JobHandle {
                    limits: record.limits,
                    screen: None,
                    samples: VecDeque::new(),
                    last_cpu: None,
                    child: None,
                    capture,
                    capture_write,
                    capture_read,
                    status,
                    deadline: (record.deadline_ms > 0)
                        .then(|| Duration::from_millis(record.deadline_ms)),
                    started: Instant::now(),
                    killed: false,
                    pending: None,
                    verdict: record.verdict,
                    resolved_cwd: PathBuf::from(&record.cwd),
                    argv: record.argv.clone(),
                    audited: record.audited,
                    scope: record.scope.clone(),
                    name: (!record.name.is_empty()).then(|| record.name.clone()),
                    lifetime: record.lifetime,
                    restart: record.restart,
                    restarts: record.restarts,
                    port: record.port,
                    port_env: String::new(),
                    human_view: HumanView::Logs,
                    dropped: record.dropped,
                    machine_file: record.machine_file.clone(),
                    machine_format: record.machine_format.clone(),
                    external: record.external,
                    pid: record.pid,
                    pid_start: record.pid_start,
                    adopted: true,
                },
            );
        }

        let mut inner = Inner { jobs, store };
        // Persist any status the probe just settled, so a second daemon opening
        // the same directory sees the resolved state rather than re-probing a
        // pid that is now free to be reused by something else.
        let ids: Vec<JobId> = inner.jobs.keys().cloned().collect();
        for id in ids {
            let _ = persist(&inner.store, &id, &inner.jobs[&id]);
        }
        inner.jobs.shrink_to_fit();

        // Field assignment rather than a struct update: `JobRegistry` implements
        // `Drop`, so it cannot be moved out of.
        self.inner = Mutex::new(inner);
        Ok(self)
    }

    /// Bound how large a job's scrollback may grow.
    pub fn with_capture_cap(mut self, bytes: u64) -> Self {
        self.capture_cap = bytes;
        self
    }

    /// Where a job's scrollback is kept.
    pub fn capture_path(&self, id: &JobId) -> Option<PathBuf> {
        self.locked().jobs.get(id).map(|h| h.capture.clone())
    }

    /// Name this node, for the host admission gate.
    pub fn with_node(mut self, node: impl Into<String>) -> Self {
        self.node = node.into();
        self
    }

    /// What this node is called.
    pub fn node(&self) -> &str {
        &self.node
    }

    /// Send this registry's audit records to a durable sink.
    pub fn with_audit(mut self, sink: Arc<dyn AuditSink>) -> Self {
        self.audit = sink;
        self
    }

    /// Which workspace owns a job.
    ///
    /// A **read**, so it is node-global like every other read: any session may
    /// ask who owns what.
    ///
    /// implements: reads-global-writes-own-workspace
    pub fn scope_of(&self, id: &JobId) -> Option<Scope> {
        self.locked().jobs.get(id).map(|h| h.scope.clone())
    }

    /// The manifest name a job was declared under, if it was declared.
    ///
    /// A read, so it is node-global like every other read.
    pub fn name_of(&self, id: &JobId) -> Option<String> {
        self.locked().jobs.get(id).and_then(|h| h.name.clone())
    }

    /// Exactly the argv a job ran (or would have run).
    pub fn argv_of(&self, id: &JobId) -> Option<Vec<String>> {
        self.locked().jobs.get(id).map(|h| h.argv.clone())
    }

    /// Start the job a workspace declares under `name`.
    ///
    /// The manifest is **re-read on every start**, so an edit takes effect
    /// without restarting anything. A config change that silently fails to apply
    /// is the kind of thing that costs a debugging session to notice.
    ///
    /// The entry is then started down the **same path as any other request** —
    /// including the node's gate. A declaration says what a workspace wants to
    /// run; it does not say the node agrees.
    ///
    /// implements: manifest-declares-node-authorizes
    pub fn start_named(&self, name: &str, caller: &Caller) -> Result<JobId, JobError> {
        // A name is only meaningful relative to a workspace's manifest, so the
        // console — which has no workspace — cannot use one.
        let Caller::Scoped(scope) = caller else {
            return Err(JobError::NameNeedsWorkspace {
                name: name.to_string(),
            });
        };
        let Some(root) = scope.root() else {
            return Err(JobError::NameNeedsWorkspace {
                name: name.to_string(),
            });
        };

        let manifest = Manifest::load(root)?;
        let entry = manifest.get(name).ok_or_else(|| JobError::Undeclared {
            name: name.to_string(),
            manifest: Manifest::path_in(root).display().to_string(),
            declared: manifest.names().iter().map(|n| n.to_string()).collect(),
        })?;

        // Policy gates first: a job that may not run *here* should not consume
        // a port, let alone reach the rule-set. Refusals name the gate, because
        // an explicit start deserves to know which one said no.
        admission::check(
            root,
            &self.node,
            entry.worktree,
            matches!(entry.port, port::PortRequest::Fixed { .. }),
            &entry.hosts,
        )
        .map_err(|refusal| JobError::Refused {
            name: name.to_string(),
            refusal: refusal.to_string(),
        })?;

        let cwd = if entry.cwd.is_empty() {
            PathBuf::from(root)
        } else {
            PathBuf::from(root).join(&entry.cwd)
        };

        // Allocated here, before the job exists but after the caller has been
        // resolved — and inside `start` the gate still runs first, so a refused
        // job never gets to hold one.
        let allocated = {
            let inner = self.locked();
            port::allocate(
                &inner.store,
                PortRange::from_env(),
                scope,
                Some(&entry.name),
                entry.port,
            )?
        };

        let mut request = JobRequest::new(entry.argv.clone(), cwd)
            .in_scope(scope.clone())
            .named(&entry.name)
            .with_lifecycle(entry.lifetime, entry.restart);
        if let Some(port) = allocated {
            request = request.with_port(port, Manifest::port_env_of(entry));
        }
        request = request.with_human_view(entry.human_view);
        request = request.with_limits(Limits {
            cpu_pct: entry.cpu_pct,
            mem_mb: entry.mem_mb,
        });
        if !entry.machine_file.is_empty() {
            request = request.with_machine_view(&entry.machine_file, &entry.machine_format);
        }
        if entry.deadline_ms > 0 {
            request = request.with_deadline(Duration::from_millis(entry.deadline_ms));
        }
        self.start(request)
    }

    /// The port the node allocated this job, if any.
    ///
    /// Visible rather than implicit: a port present only in the child's
    /// environment leaves nobody able to say which checkout's server is on
    /// which port, which is most of why allocation was centralized.
    pub fn port_of(&self, id: &JobId) -> Option<u16> {
        self.locked()
            .jobs
            .get(id)
            .map(|h| h.port)
            .filter(|p| *p != 0)
    }

    /// Was this job handed over rather than started here?
    pub fn is_external(&self, id: &JobId) -> bool {
        self.locked()
            .jobs
            .get(id)
            .map(|h| h.external)
            .unwrap_or(false)
    }

    /// Take an already-running process into the registry.
    ///
    /// **This is the re-adopt path, parameterized.** Re-adoption after a daemon
    /// restart and hand-over from outside are the same operation — read the
    /// pid's start time, build a detached entry with no child handle, persist,
    /// and let the ordinary liveness probe watch it — so they share one
    /// implementation rather than two that drift.
    ///
    /// With `command = None` the entry is **observe-only**: it is listed, its
    /// state comes from liveness, it settles when the pid dies, and the
    /// supervisor never respawns it. That last part matters — an observe-only
    /// entry cannot fight a human restarting that process by hand.
    ///
    /// It exists because of a bootstrap loop: the transport cannot be a
    /// supervisor-started job, since the management plane reaches the supervisor
    /// *through* it. The goal here is visibility, not supervision.
    ///
    /// implements: adopt-is-readopt-parameterized
    pub fn adopt(
        &self,
        scope: Scope,
        command: Option<Vec<String>>,
        pid: u32,
    ) -> Result<JobId, JobError> {
        // Nothing there is nothing to adopt: an entry for a process that was
        // never running would report a state it could not have.
        let Some(pid_start) = procfs::start_time(pid) else {
            return Err(JobError::NotRunning { pid });
        };

        let mut inner = self.locked();
        let id = inner.store.mint()?;
        let capture = inner.store.capture_path(&id.0);
        let capture_write = File::options()
            .create(true)
            .append(true)
            .open(&capture)
            .map_err(JobError::Io)?;
        let capture_read = File::open(&capture).map_err(JobError::Io)?;

        let handle = JobHandle {
            limits: Limits::default(),
            screen: None,
            samples: VecDeque::new(),
            last_cpu: None,
            child: None,
            capture,
            capture_write,
            capture_read,
            status: JobStatus::Running,
            deadline: None,
            started: Instant::now(),
            killed: false,
            pending: None,
            verdict: Verdict::Ungoverned,
            resolved_cwd: PathBuf::from("/"),
            argv: command.unwrap_or_default(),
            audited: false,
            scope,
            name: None,
            lifetime: Lifetime::Service,
            // Nothing to respawn from unless a command was captured, and even
            // then the declaration is what a restart re-reads.
            restart: RestartPolicy::Never,
            restarts: 0,
            port: 0,
            port_env: String::new(),
            human_view: HumanView::Logs,
            dropped: 0,
            machine_file: String::new(),
            machine_format: String::new(),
            external: true,
            pid,
            pid_start,
            // Not our child: tracked by liveness, like anything re-adopted.
            adopted: true,
        };

        persist(&inner.store, &id, &handle)?;
        inner.jobs.insert(id.clone(), handle);
        Ok(id)
    }

    /// A job's current status.
    pub fn status_of(&self, id: &JobId) -> Option<JobStatus> {
        self.locked().jobs.get(id).map(|h| h.status.clone())
    }

    /// How many times this *instance* has been brought back.
    ///
    /// Usually you want [`declared`](Self::declared) instead: a restart mints a
    /// new job id, so a count read from the id you started with stops moving the
    /// moment it is restarted.
    pub fn restarts_of(&self, id: &JobId) -> u64 {
        self.locked().jobs.get(id).map(|h| h.restarts).unwrap_or(0)
    }

    /// The current instance of a declared job, and how many times it has been
    /// brought back.
    ///
    /// **A service's durable identity is `workspace:name`, not its job id.**
    /// Each spawn is a new instance with a new id — that is what makes the
    /// record of the previous one survivable — so "how many times has `check`
    /// restarted?" is a question about the *declaration*, and this is where it
    /// is answered.
    ///
    /// implements: workspace-is-the-owner-not-session
    pub fn declared(&self, scope: &Scope, name: &str) -> Option<(JobId, u64)> {
        let inner = self.locked();
        inner
            .jobs
            .iter()
            .filter(|(_, h)| h.scope == *scope && h.name.as_deref() == Some(name))
            // Ids lead with a millisecond stamp, so the greatest is the newest.
            .max_by(|(a, _), (b, _)| a.cmp(b))
            .map(|(id, h)| (id.clone(), h.restarts))
    }

    /// Stop a declared job and start it again from its **current** declaration.
    ///
    /// The manifest is re-read, and the new declaration goes through the node's
    /// gate again. Re-reading without re-authorizing would be a hole rather than
    /// a convenience: the manifest lives in a checkout the agent can edit, so
    /// "edit the file, hit restart" would run a never-approved argv while the
    /// node never got a say.
    ///
    /// Only a **declared** job can be restarted. An ad-hoc one has nothing to
    /// re-read, and respawning a spec cached at first start is precisely the
    /// silently-stale behaviour this avoids.
    ///
    /// implements: manifest-declares-node-authorizes
    pub fn restart(&self, id: &JobId, caller: &Caller) -> Result<JobId, JobError> {
        let (owner, name) = {
            let inner = self.locked();
            let handle = inner
                .jobs
                .get(id)
                .ok_or_else(|| JobError::NotFound(id.clone()))?;
            (handle.scope.clone(), handle.name.clone())
        };

        if !caller.may_write(&owner) {
            return Err(JobError::ForeignWorkspace {
                job: id.clone(),
                owner: owner.to_string(),
                caller: caller.to_string(),
            });
        }
        let Some(name) = name else {
            return Err(JobError::NotDeclared { job: id.clone() });
        };

        // Stop the old instance first; a restart that left two running would be
        // a duplicate, not a restart.
        let _ = self.kill(id, caller);
        self.start_named(&name, caller)
    }

    /// Start this workspace's `session` entries that are not already running.
    ///
    /// **The agent's half of the two triggers.** `boot` belongs to the daemon,
    /// which reads the node-level manifest at startup; `session` belongs here,
    /// because the daemon does not know every repo on the machine and walking
    /// the filesystem looking for manifests would be unbounded and surprising.
    ///
    /// Idempotent by construction: it starts what is missing and **never stops
    /// or restarts anything**, so re-running it is always safe.
    ///
    /// A refused entry is **skipped silently**. This runs unprompted at session
    /// start, so a job that is not for this machine or this worktree is not a
    /// problem to report — it is simply not this session's business. An explicit
    /// start still says exactly why.
    ///
    /// implements: autostart-and-restart-are-orthogonal
    pub fn ensure(&self, caller: &Caller) -> Result<Vec<JobId>, JobError> {
        let Caller::Scoped(scope) = caller else {
            return Err(JobError::NameNeedsWorkspace {
                name: "<session autostart>".to_string(),
            });
        };
        let Some(root) = scope.root() else {
            return Err(JobError::NameNeedsWorkspace {
                name: "<session autostart>".to_string(),
            });
        };

        let manifest = Manifest::load(root)?;
        let mut started = Vec::new();
        for entry in &manifest.proc {
            if entry.autostart != crate::lifecycle::Autostart::Session {
                continue;
            }
            // Already running is the common case on a re-run, and is precisely
            // what "ensure" means: leave it be.
            if self
                .declared(scope, &entry.name)
                .and_then(|(id, _)| self.status_of(&id))
                .is_some_and(|status| !status.is_terminal())
            {
                continue;
            }
            match self.start_named(&entry.name, caller) {
                Ok(id) => started.push(id),
                // Silent on purpose — see above. A manifest problem is not
                // silent, though: `Manifest::load` already failed loudly if the
                // file could not be read at all.
                Err(_) => continue,
            }
        }
        Ok(started)
    }

    /// One supervision tick: apply each job's restart policy.
    ///
    /// Deliberately **driven by the caller** rather than by a thread this crate
    /// spawns. The engine links no runtime, so the daemon owns the interval —
    /// which also makes the policy testable without sleeping on a background
    /// task, and makes the tick rate the natural rate limit on a service that
    /// crashes immediately.
    ///
    /// implements: autostart-and-restart-are-orthogonal
    pub fn supervise(&self) -> Vec<JobId> {
        // A policy cannot act on a state nobody has observed.
        self.settle_all();
        // Sampling rides the tick that already exists rather than owning a
        // timer of its own. The interval between calls *is* the sampling
        // interval, which is why a rate is computed from observed elapsed time
        // rather than an assumed period.
        self.sample_all();

        let due: Vec<(JobId, Scope, String)> = {
            let inner = self.locked();
            inner
                .jobs
                .iter()
                .filter(|(_, h)| {
                    h.status.is_terminal()
                        && h.restart.should_restart(h.lifetime, &h.status, h.killed)
                })
                .filter_map(|(id, h)| {
                    h.name
                        .clone()
                        .map(|name| (id.clone(), h.scope.clone(), name))
                })
                .collect()
        };

        let mut restarted = Vec::new();
        for (id, scope, name) in due {
            let previous = self.locked().jobs.get(&id).map(|h| h.restarts).unwrap_or(0);
            // The supervisor acts on the workspace's own declaration, so it
            // asks as that workspace — not with global authority it has no
            // reason to hold.
            if let Ok(new_id) = self.start_named(&name, &Caller::Scoped(scope)) {
                if let Some(handle) = self.locked().jobs.get_mut(&new_id) {
                    handle.restarts = previous + 1;
                }
                // The old entry has been superseded; stop counting it as due.
                if let Some(old) = self.locked().jobs.get_mut(&id) {
                    old.restarts = previous + 1;
                    old.killed = true;
                }
                restarted.push(new_id);
            }
        }
        restarted
    }

    /// Remove terminal entries, returning how many went.
    ///
    /// **Only terminal ones, and it never signals anything.** A crashed job has
    /// to linger so you can see *that* it died and read its last output, so
    /// pruning is something a person asks for rather than something that
    /// happens. And a live job is left strictly alone: tombstone cleanup and
    /// kill-all are different verbs, and conflating them is how someone loses a
    /// dev server by tidying up. Stop it first if that is what you meant.
    ///
    /// Scope follows the same split as every other write — a session clears its
    /// own workspace, the console clears globally.
    ///
    /// implements: reads-global-writes-own-workspace
    pub fn clear(&self, caller: &Caller) -> Result<usize, JobError> {
        let mut inner = self.locked();
        let Inner { jobs, store } = &mut *inner;

        let doomed: Vec<JobId> = jobs
            .iter()
            .filter(|(_, h)| h.status.is_terminal() && caller.may_write(&h.scope))
            .map(|(id, _)| id.clone())
            .collect();

        for id in &doomed {
            jobs.remove(id);
            // The durable record goes too, or a re-adopting daemon would bring
            // back exactly what was just cleared.
            let _ = store.remove(&id.0);
        }
        Ok(doomed.len())
    }

    /// Bring every job's status up to date.
    ///
    /// A status is only current once someone has looked: a child is reaped when
    /// it is tailed, and an adopted process is probed then too. Any read that
    /// *reports* status therefore has to settle first, or it hands back a
    /// finished job wearing `Running` — which is what a poller acts on.
    fn settle_all(&self) {
        let ids: Vec<JobId> = self.locked().jobs.keys().cloned().collect();
        for id in ids {
            // Reading from the end: this is for the status, not the bytes.
            let _ = self.tail(&id, u64::MAX);
        }
    }

    /// Take one resource reading of every live job.
    ///
    /// Called from `supervise`, so an ordinary daemon needs no separate timer;
    /// exposed publicly so a caller wanting a reading *now* can force one
    /// rather than waiting out a tick.
    ///
    /// A job with nothing running contributes no sample. That is not the same
    /// as a zero reading, and conflating them would show a finished job sitting
    /// quietly at 0% as though it were idling rather than gone.
    pub fn sample_all(&self) {
        let now = Instant::now();
        let at_unix_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);

        let mut inner = self.locked();
        for handle in inner.jobs.values_mut() {
            if !handle.status.is_running() || handle.pid == 0 {
                continue;
            }
            let Some(usage) = procfs::usage(handle.pid) else {
                // The process went away between the status check and the read.
                // Nothing to report, and inventing a zero would be a lie about
                // a job that is simply gone.
                continue;
            };

            // A rate needs two points. The first reading establishes the
            // baseline and reports no load, because a total is not a rate and
            // presenting one as the other would show every freshly sampled job
            // spiking to whatever it had accumulated since it started.
            let cpu_pct = match handle.last_cpu {
                Some((previous_ticks, previous_at)) => {
                    let elapsed = now.duration_since(previous_at).as_secs_f64();
                    let ticks = usage.cpu_ticks.saturating_sub(previous_ticks) as f64;
                    if elapsed > 0.0 {
                        ((ticks / procfs::TICKS_PER_SECOND as f64) / elapsed * 100.0).round() as u32
                    } else {
                        0
                    }
                }
                None => 0,
            };
            handle.last_cpu = Some((usage.cpu_ticks, now));

            if handle.samples.len() == SAMPLE_HISTORY {
                handle.samples.pop_front();
            }
            handle.samples.push_back(Sample {
                at_unix_ms,
                cpu_pct,
                rss_bytes: usage.rss_bytes,
            });
        }
    }

    /// What this job has been consuming, and whether that crosses what was
    /// declared.
    ///
    /// **Reporting only.** A breach here has no effect on the job whatsoever:
    /// nothing throttles it, nothing kills it. Enforcement needs cgroup
    /// delegation this does not have, and shipping a half-enforcement that
    /// killed the job someone was mid-diagnosis on would be worse than none.
    pub fn stats(&self, id: &JobId) -> Result<Stats, JobError> {
        let inner = self.locked();
        let handle = inner
            .jobs
            .get(id)
            .ok_or_else(|| JobError::NotFound(id.clone()))?;
        let breach = handle
            .samples
            .back()
            .map(|s| handle.limits.breached_by(s))
            .unwrap_or(Breach {
                cpu: false,
                memory: false,
            });
        Ok(Stats {
            samples: handle.samples.iter().copied().collect(),
            limits: handle.limits,
            breach,
        })
    }

    /// What a person would see if they attached to this job right now.
    ///
    /// The third lens, and the one that makes a redrawing program legible: a
    /// progress bar that rewrites one line with carriage returns is thousands
    /// of superseded frames in `tail` and a single line here. Cheap for the
    /// same reason the machine lens is cheap — it is an *answer*, not a stream.
    ///
    /// **Not scrollback.** Only the visible grid comes back; history is what
    /// `tail` is for, and duplicating it here would undo the saving.
    ///
    /// implements: the-screen-is-a-live-fold-not-a-replay
    pub fn screen(&self, id: &JobId) -> Result<ScreenRead, JobError> {
        let (live, capture) = {
            let inner = self.locked();
            let handle = inner
                .jobs
                .get(id)
                .ok_or_else(|| JobError::NotFound(id.clone()))?;
            (handle.screen.clone(), handle.capture.clone())
        };

        // The live fold, where there is one. Read under its own lock rather
        // than the registry's: the pump holds it for the length of a `process`
        // call, and making a screen read wait on the registry lock would put a
        // job's output rate in the path of every other operation.
        if let Some(parser) = live {
            let parser = parser
                .lock()
                .map_err(|_| JobError::Audit("the screen parser is poisoned".to_string()))?;
            return Ok(ScreenRead {
                source: ScreenSource::Live,
                rows: SCREEN_ROWS,
                cols: SCREEN_COLS,
                text: trim_blank_rows(&parser.screen().contents()),
            });
        }

        // No live fold: a logs job, or a terminal job whose pty died with the
        // daemon that owned it. Replaying the **retained** capture is the best
        // available answer, and it is labelled `Replayed` so a caller knows it
        // is reconstructed rather than observed.
        let bytes = std::fs::read(&capture).map_err(JobError::Io)?;
        let mut parser = vt100::Parser::new(SCREEN_ROWS, SCREEN_COLS, 0);
        parser.process(&bytes);
        Ok(ScreenRead {
            source: ScreenSource::Replayed,
            rows: SCREEN_ROWS,
            cols: SCREEN_COLS,
            text: trim_blank_rows(&parser.screen().contents()),
        })
    }

    /// Every job this registry knows about, in creation order.
    ///
    /// Settles first, so a finished job is reported finished. Reads are
    /// node-global, so this spans every workspace.
    pub fn list(&self) -> Vec<(JobId, JobStatus)> {
        self.settle_all();
        let inner = self.locked();
        let mut out: Vec<(JobId, JobStatus)> = inner
            .jobs
            .iter()
            .map(|(id, handle)| (id.clone(), handle.status.clone()))
            .collect();
        out.sort_by(|a, b| a.0.cmp(&b.0));
        out
    }

    /// The directory this registry's durable state lives in.
    pub fn dir(&self) -> PathBuf {
        self.locked().store.dir().to_path_buf()
    }

    /// The audit history, for read-only introspection: what worked, what got
    /// denied.
    ///
    /// implements: agent-self-sufficiency
    /// implements: audit-every-attempt
    pub fn audit(&self) -> &Arc<dyn AuditSink> {
        &self.audit
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

    /// Claim the right to write this job's record, returning what to write.
    ///
    /// Pure bookkeeping — no I/O, so it is safe to call under the registry lock.
    /// Returns `None` if the record is already written or already claimed.
    ///
    /// **The claim must be [`flush`](Self::flush)ed after the lock is
    /// released.** There is deliberately no helper that does both: one existed,
    /// every caller under the lock used it, and `flush` re-locks on failure — so
    /// an audit-write failure on the `start`, `decide` or `kill` path deadlocked
    /// the registry. The two halves are separate so that the unlock between them
    /// has to be written out, where it can be seen.
    ///
    /// implements: audit-every-attempt
    #[must_use = "a claimed record must be flushed once the lock is released, \
                  or the attempt is silently lost"]
    fn claim(&self, id: &JobId, handle: &mut JobHandle) -> Option<AuditRecord> {
        if handle.audited {
            return None;
        }
        handle.audited = true;
        Some(AuditRecord::new(
            id.0.clone(),
            handle.argv.clone(),
            handle.resolved_cwd.clone(),
            handle.verdict,
            handle.status.clone(),
        ))
    }

    /// Write a claimed record. **Call with the registry lock released**: a sink
    /// can block on a hung filesystem, and holding the lock across that would
    /// freeze every other job's start/tail/wait/kill.
    ///
    /// On failure the claim is released, so the attempt can be recorded on a
    /// later call rather than being lost silently.
    fn flush(&self, id: &JobId, record: AuditRecord) -> Result<(), JobError> {
        match self.audit.append(record) {
            Ok(()) => Ok(()),
            Err(e) => {
                if let Some(handle) = self.locked().jobs.get_mut(id) {
                    handle.audited = false;
                }
                Err(e)
            }
        }
    }

    /// Record an attempt that was rejected before it could become a job.
    ///
    /// It has no `JobHandle` and never will, so it is appended directly. Best
    /// effort: the caller is already returning the rejection error, and losing
    /// that error to an audit failure would be worse than the missing line.
    ///
    /// implements: audit-every-attempt
    fn record_rejected(&self, argv: &[String], cwd: &Path, verdict: Verdict, status: JobStatus) {
        // Mint a real id even though no job survives: the audit's identifier
        // must mean the same thing on every line. If minting fails there is
        // nowhere to put the record, so the attempt is lost rather than
        // mislabelled — the caller is already returning the rejection error.
        let inner = self.locked();
        let Ok(id) = inner.store.mint() else {
            return;
        };
        // The rejection never becomes a job, so release the id claim rather than
        // leaving an empty record file behind for it. The audit keeps the id.
        let _ = inner.store.remove(&id.0);
        drop(inner);

        let _ = self.audit.append(AuditRecord::new(
            id.0,
            argv.to_vec(),
            cwd.to_path_buf(),
            verdict,
            status,
        ));
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
            program: argv.first().and_then(|p| resolve_program(p, &resolve_base)),
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
        // legal working directory never becomes a job. It is still an attempt,
        // so it is recorded — a probe at the confinement boundary is exactly the
        // kind of event the audit exists for.
        let cwd = match self.confined_cwd(&req.cwd) {
            Ok(cwd) => cwd,
            Err(e) => {
                self.record_rejected(
                    &req.argv,
                    &req.cwd,
                    Verdict::SeedDeny,
                    JobStatus::Denied {
                        reason: DeniedReason::CwdEscape,
                    },
                );
                return Err(e);
            }
        };
        // An ad-hoc job belongs to the workspace it runs in unless the caller
        // says otherwise. There is always an answer, so no later code path has
        // to cope with an unowned job.
        let scope = req
            .scope
            .clone()
            .unwrap_or_else(|| Scope::resolve(&req.cwd));
        let disposition = self.disposition(&req.argv);

        let mut inner = self.locked();
        // Minting claims the id on disk, so two daemons sharing this directory
        // can never hand out the same one.
        let id = inner.store.mint()?;

        let (capture, capture_write, capture_read) = Self::create_capture(&inner, &id)?;

        let request = JobRequest {
            argv: req.argv,
            cwd,
            deadline: req.deadline,
            scope: Some(scope.clone()),
            name: req.name.clone(),
            lifetime: req.lifetime,
            restart: req.restart,
            port: req.port,
            port_env: req.port_env.clone(),
            human_view: req.human_view,
            machine_file: req.machine_file.clone(),
            machine_format: req.machine_format.clone(),
            limits: req.limits,
        };

        let mut handle = JobHandle {
            limits: request.limits,
            screen: None,
            samples: VecDeque::new(),
            last_cpu: None,
            child: None,
            capture,
            capture_write,
            capture_read,
            status: JobStatus::PendingApproval,
            deadline: request.deadline,
            started: Instant::now(),
            killed: false,
            pending: None,
            // A pending job's default fate is the fail-closed one; a decision
            // overrides it.
            verdict: match disposition {
                Disposition::Allow if self.policy.is_none() => Verdict::Ungoverned,
                Disposition::Allow => Verdict::SeedAllow,
                Disposition::Deny => Verdict::SeedDeny,
                Disposition::NeedsApproval => Verdict::NoApprover,
            },
            resolved_cwd: request.cwd.clone(),
            argv: request.argv.clone(),
            audited: false,
            scope,
            name: req.name.clone(),
            lifetime: req.lifetime,
            restart: req.restart,
            restarts: 0,
            port: req.port.unwrap_or(0),
            port_env: req.port_env.clone(),
            human_view: req.human_view,
            dropped: 0,
            machine_file: req.machine_file.clone(),
            machine_format: req.machine_format.clone(),
            external: false,
            pid: 0,
            pid_start: 0,
            adopted: false,
        };

        match disposition {
            Disposition::Allow => {
                // Substitution happens **below the gate**: the rule-set matched
                // the argv as declared, token and all, and only now does the
                // node-supplied value go in.
                let spawn_argv = port::substitute(&request.argv, handle.port_request());
                // Filled in by the spawn for a terminal job, and left `None`
                // otherwise; moved onto the handle only once the spawn has
                // actually succeeded, so a failed start leaves no parser behind
                // for a process that never drew anything.
                let mut screen = None;
                match spawn_child(
                    &spawn_argv,
                    &request.cwd,
                    &handle.capture_write,
                    handle.port_request(),
                    handle.port_env_or_default(),
                    handle.human_view,
                    &mut screen,
                ) {
                    Ok(child) => {
                        handle.screen = screen;
                        handle.pid = child.id();
                        // Recorded at spawn: this is what makes a later probe
                        // able to tell this process from a reused pid.
                        handle.pid_start = procfs::start_time(handle.pid).unwrap_or(0);
                        handle.child = Some(child);
                        handle.status = JobStatus::Running;
                        handle.started = Instant::now();
                    }
                    Err(e) => {
                        // Permitted but unstartable — still an attempt, and the
                        // caller gets no id, so record it here or never.
                        handle.status = JobStatus::Failed {
                            error: e.to_string(),
                        };
                        let claimed = self.claim(&id, &mut handle);
                        // Leave a real record behind, not the empty file that
                        // minting the id created: an id claim with no record is
                        // state nobody can read.
                        let _ = persist(&inner.store, &id, &handle);
                        drop(inner);
                        if let Some(record) = claimed {
                            let _ = self.flush(&id, record);
                        }
                        return Err(e);
                    }
                }
            }
            Disposition::Deny => {
                handle.status = JobStatus::Denied {
                    reason: DeniedReason::Rule,
                };
            }
            Disposition::NeedsApproval => {
                handle.status = JobStatus::PendingApproval;
                handle.pending = Some(request);
            }
        }

        // A denial is history the moment it happens — never reconstructed, and
        // never dependent on someone calling wait().
        let claimed = if handle.status.is_terminal() {
            self.claim(&id, &mut handle)
        } else {
            None
        };

        // Durable before the id is returned: a caller holding an id must never
        // find that the daemon died before the job it names was written down.
        persist(&inner.store, &id, &handle)?;

        inner.jobs.insert(id.clone(), handle);
        // Release before writing the audit: the sink can block, and an audit
        // failure re-locks to release the claim.
        drop(inner);
        if let Some(record) = claimed {
            self.flush(&id, record)?;
        }
        Ok(id)
    }

    /// Resolve a request's cwd under the node's confinement, if any.
    fn confined_cwd(&self, cwd: &Path) -> Result<PathBuf, JobError> {
        match &self.policy {
            Some(policy) => confine(cwd, &policy.allowed_root).map_err(|e| {
                if e == NOT_UTF8 {
                    JobError::CwdNotUtf8 {
                        cwd: cwd.display().to_string(),
                    }
                } else {
                    JobError::CwdEscape {
                        cwd: cwd.display().to_string(),
                        root: policy.allowed_root.display().to_string(),
                    }
                }
            }),
            None if cwd.to_str().is_none() => Err(JobError::CwdNotUtf8 {
                cwd: cwd.display().to_string(),
            }),
            None => Ok(cwd.to_path_buf()),
        }
    }

    /// Create a capture file inside this registry's directory, opened with
    /// `create_new` so an existing path (a planted symlink) is refused.
    fn create_capture(inner: &Inner, job: &JobId) -> Result<(PathBuf, File, File), JobError> {
        let path = inner.store.capture_path(&job.0);
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

        // The whole decision runs under the lock and yields *what to audit*;
        // the write itself happens after the lock is released, below. Doing it
        // inline would hold the registry across a sink that can block, and
        // would deadlock outright when the sink fails — `flush` re-locks to
        // release the claim.
        let mut claimed: Option<AuditRecord> = None;
        let outcome = (|| -> Result<(), JobError> {
            let mut inner = self.locked();
            // Split the borrow so the durable record can be written while the
            // live handle is still held: they are disjoint fields of `Inner`.
            let Inner { jobs, store } = &mut *inner;
            let handle = jobs
                .get_mut(id)
                .ok_or_else(|| JobError::NotFound(id.clone()))?;

            if handle.status != JobStatus::PendingApproval {
                return Err(JobError::NotPending(id.clone()));
            }

            // The decision arrived too late: fail closed rather than spawn.
            if approval_timeout.is_some_and(|t| handle.started.elapsed() >= t) {
                handle.status = JobStatus::Denied {
                    reason: DeniedReason::NoApprover,
                };
                handle.verdict = Verdict::NoApprover;
                handle.pending = None;
                claimed = self.claim(id, handle);
                let _ = persist(store, id, handle);
                return Ok(());
            }

            if edited_disposition == Some(Disposition::Deny) {
                handle.status = JobStatus::Denied {
                    reason: DeniedReason::Rule,
                };
                handle.verdict = Verdict::SeedDeny;
                handle.pending = None;
                claimed = self.claim(id, handle);
                let _ = persist(store, id, handle);
                return Ok(());
            }

            let request = handle
                .pending
                .take()
                .ok_or_else(|| JobError::NotPending(id.clone()))?;

            match decision {
                Decision::DenyOnce => {
                    handle.status = JobStatus::Denied {
                        reason: DeniedReason::Decision,
                    };
                    handle.verdict = Verdict::RejectedOnce;
                    claimed = self.claim(id, handle);
                    let _ = persist(store, id, handle);
                    return Ok(());
                }
                Decision::AllowOnce { edited_argv } => {
                    // An edited argv is the correction back-channel: it, not
                    // the proposal, is what actually runs.
                    let argv = edited_argv.unwrap_or(request.argv);
                    // Re-confine immediately before spawning: the cwd was
                    // resolved when the job was proposed, possibly long ago.
                    // This shrinks — but cannot fully close — the window in
                    // which a path component is swapped for a symlink out of
                    // the allowed root.
                    let cwd = match self.confined_cwd(&request.cwd) {
                        Ok(cwd) => cwd,
                        Err(e) => {
                            // Terminal, so it needs both a verdict and a record
                            // — otherwise the job's status and verdict disagree
                            // and nothing is ever written.
                            handle.status = JobStatus::Denied {
                                reason: DeniedReason::CwdEscape,
                            };
                            handle.verdict = Verdict::SeedDeny;
                            claimed = self.claim(id, handle);
                            let _ = persist(store, id, handle);
                            return Err(e);
                        }
                    };
                    let spawn_argv = port::substitute(&argv, handle.port_request());
                    let mut screen = None;
                    match spawn_child(
                        &spawn_argv,
                        &cwd,
                        &handle.capture_write,
                        handle.port_request(),
                        handle.port_env_or_default(),
                        handle.human_view,
                        &mut screen,
                    ) {
                        Ok(child) => {
                            handle.screen = screen;
                            handle.pid = child.id();
                            handle.pid_start = procfs::start_time(handle.pid).unwrap_or(0);
                            handle.child = Some(child);
                            handle.status = JobStatus::Running;
                            // The approval is the authority, and the argv that
                            // ran is what the record must name.
                            handle.verdict = Verdict::ApprovedOnce;
                            handle.argv = argv;
                            handle.resolved_cwd = cwd;
                            // The execution deadline runs from the spawn, not
                            // from the moment the job entered the approval
                            // queue.
                            handle.started = Instant::now();
                        }
                        Err(e) => {
                            // Never leave the job pending with its decision
                            // consumed: that would strand it until the approval
                            // timeout and report the wrong reason.
                            handle.status = JobStatus::Denied {
                                reason: DeniedReason::Decision,
                            };
                            handle.verdict = Verdict::RejectedOnce;
                            claimed = self.claim(id, handle);
                            let _ = persist(store, id, handle);
                            return Err(e);
                        }
                    }
                }
            }
            let _ = persist(store, id, handle);
            Ok(())
        })();

        // Lock released. Write the audit now; an audit failure is the operation's
        // failure, because "every attempt is durable history" is a contract.
        if let Some(record) = claimed {
            self.flush(id, record)?;
        }
        outcome
    }

    /// Read output incrementally from `offset`, with the job's live status.
    pub fn tail(&self, id: &JobId, offset: u64) -> Result<Tail, JobError> {
        let cap = self.capture_cap;
        // **Settle the job first.** Without this, a caller that only ever tails
        // — which is exactly what a polling remote caller does — would watch a
        // finished job report `Running` forever, because nothing else had
        // reaped it. Status is part of every tail precisely so that polling is
        // a complete way to observe a job; that is only true if the status is
        // current.
        let (tail, record) = {
            let mut inner = self.locked();
            let Inner { jobs, store } = &mut *inner;
            let handle = jobs
                .get_mut(id)
                .ok_or_else(|| JobError::NotFound(id.clone()))?;

            let mut claimed = None;
            if !handle.status.is_terminal() {
                if let Some(child) = handle.child.as_mut() {
                    if let Some(exit) = child.try_wait().map_err(JobError::Io)? {
                        handle.status = if handle.killed {
                            JobStatus::Killed
                        } else {
                            JobStatus::Done {
                                code: exit.code().unwrap_or(-1),
                            }
                        };
                        handle.child = None;
                        let _ = persist(store, id, handle);
                        claimed = self.claim(id, handle);
                    }
                } else if handle.adopted {
                    // Not our child: liveness is all we have, and a dead one
                    // ends as `outcome-unknown` rather than a made-up code.
                    match procfs::liveness(handle.pid, handle.pid_start) {
                        Liveness::Alive => {}
                        Liveness::Dead | Liveness::Unsupported => {
                            handle.status = JobStatus::OutcomeUnknown;
                            let _ = persist(store, id, handle);
                            claimed = self.claim(id, handle);
                        }
                    }
                }
            }

            // Bound the scrollback before reading it, so a long-running job
            // cannot fill the disk between reads.
            if let Ok(discarded) = compact(&handle.capture, cap)
                && discarded > 0
            {
                handle.dropped += discarded;
                let _ = persist(store, id, handle);
            }

            // Offsets are **logical**: what the caller holds counts bytes ever
            // written, so it stays meaningful across a compaction. Translate it
            // into a position in the file that is actually there now.
            let dropped = handle.dropped;
            let skipped = dropped.saturating_sub(offset);
            let physical = offset.saturating_sub(dropped);

            // Read through the handle opened at job creation — never re-open by
            // path, which a swapped file could hijack.
            let mut file = handle.capture_read.try_clone().map_err(JobError::Io)?;

            // **Clamp to the end rather than seeking past it.** Asking to
            // resume beyond what exists is not an error — it is a caller that
            // is already up to date, and the honest answer is "nothing new".
            // Erroring instead is worse than useless here: `settle_all` reads
            // with `u64::MAX` precisely to mean *I want the status, not the
            // bytes*, and an offset that large is rejected outright by the
            // kernel, so the failure was being swallowed on every settle.
            let end = file.seek(SeekFrom::End(0)).map_err(JobError::Io)?;
            let physical = physical.min(end);
            file.seek(SeekFrom::Start(physical)).map_err(JobError::Io)?;
            let mut bytes = Vec::new();
            file.read_to_end(&mut bytes).map_err(JobError::Io)?;

            (
                Tail {
                    next_offset: dropped + physical + bytes.len() as u64,
                    skipped,
                    bytes,
                    status: handle.status.clone(),
                },
                claimed,
            )
        };

        // Flush with the lock released: a sink can block on a hung filesystem,
        // and holding the registry lock across that would freeze every other
        // job. (It is also why `flush` may re-lock safely from here.)
        if let Some(record) = record {
            self.flush(id, record)?;
        }
        Ok(tail)
    }

    /// Read a job through its **machine lens**.
    ///
    /// When a sidecar is declared, this reads that — a few hundred bytes of
    /// structured answer instead of megabytes of redraw, which is the whole
    /// reason the lens exists. When one is not, it falls back to scrollback and
    /// **says so**: handing back raw output labelled as though it were
    /// structured would be worse than handing back nothing, because a caller
    /// would try to parse it.
    ///
    /// A declared-but-not-yet-written sidecar reads **empty**, not as a
    /// fallback. The lens was declared, so that is the channel, and "nothing
    /// yet" is a real answer about it.
    ///
    /// implements: dual-lens-human-and-machine
    pub fn machine(&self, id: &JobId, offset: u64) -> Result<MachineRead, JobError> {
        let (file, format, cwd) = {
            let inner = self.locked();
            let handle = inner
                .jobs
                .get(id)
                .ok_or_else(|| JobError::NotFound(id.clone()))?;
            (
                handle.machine_file.clone(),
                handle.machine_format.clone(),
                handle.resolved_cwd.clone(),
            )
        };

        if file.is_empty() {
            let tail = self.tail(id, offset)?;
            return Ok(MachineRead {
                source: LensSource::Scrollback,
                format: String::new(),
                bytes: tail.bytes,
                next_offset: tail.next_offset,
            });
        }

        // The manifest is agent-editable, so a declared path is not a licence to
        // read anything on the machine: it is resolved within the job's own
        // directory and refused if it climbs out.
        let path = cwd.join(&file);
        let bytes = match path.canonicalize() {
            Ok(resolved) => {
                if !resolved.starts_with(&cwd) {
                    return Err(JobError::CwdEscape {
                        cwd: resolved.display().to_string(),
                        root: cwd.display().to_string(),
                    });
                }
                let mut handle = File::open(&resolved).map_err(JobError::Io)?;
                handle.seek(SeekFrom::Start(offset)).map_err(JobError::Io)?;
                let mut bytes = Vec::new();
                handle.read_to_end(&mut bytes).map_err(JobError::Io)?;
                bytes
            }
            // Not written yet. Declared is declared: this is an empty machine
            // read, not a silent switch to the other channel.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(e) => return Err(JobError::Io(e)),
        };

        Ok(MachineRead {
            source: LensSource::Machine,
            format,
            next_offset: offset + bytes.len() as u64,
            bytes,
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
            let mut expired: Option<(Child, Outcome)> = None;
            let mut settled: Option<(Outcome, Option<AuditRecord>)> = None;
            {
                let mut inner = self.locked();
                let approval_timeout = self.policy.as_ref().map(|p| p.approval_timeout);
                let Inner { jobs, store } = &mut *inner;
                let handle = jobs
                    .get_mut(id)
                    .ok_or_else(|| JobError::NotFound(id.clone()))?;

                if handle.status.is_terminal() {
                    settled = Some((outcome_of(handle), self.claim(id, handle)));
                }

                // A re-adopted job is not our child: `try_wait` cannot reap it,
                // so all we can do is ask whether the same process is still
                // there. When it is gone we cannot know *how* it ended.
                if !handle.status.is_terminal() && handle.adopted {
                    match procfs::liveness(handle.pid, handle.pid_start) {
                        Liveness::Alive => {}
                        Liveness::Dead | Liveness::Unsupported => {
                            handle.status = JobStatus::OutcomeUnknown;
                            let _ = persist(store, id, handle);
                            settled = Some((outcome_of(handle), self.claim(id, handle)));
                        }
                    }
                }

                // Fail closed: nobody decided in time.
                if handle.status == JobStatus::PendingApproval
                    && approval_timeout.is_some_and(|t| handle.started.elapsed() >= t)
                {
                    handle.status = JobStatus::Denied {
                        reason: DeniedReason::NoApprover,
                    };
                    handle.pending = None;
                    let _ = persist(store, id, handle);
                    settled = Some((outcome_of(handle), self.claim(id, handle)));
                } else if let Some(child) = handle.child.as_mut() {
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
                        let _ = persist(store, id, handle);
                        settled = Some((outcome_of(handle), self.claim(id, handle)));
                    } else if handle
                        .deadline
                        .is_some_and(|dl| handle.started.elapsed() >= dl)
                    {
                        handle.status = JobStatus::TimedOut;
                        let _ = persist(store, id, handle);
                        if let Some(child) = handle.child.take() {
                            expired = Some((child, outcome_of(handle)));
                        }
                    }
                }
            }

            if let Some((outcome, record)) = settled {
                if let Some(record) = record {
                    self.flush(id, record)?;
                }
                return Ok(outcome);
            }

            if let Some((mut child, outcome)) = expired {
                // Reap before recording: if the audit write fails we must not
                // drop the child un-reaped (its handle is already out of the
                // registry, so nothing else would ever collect it).
                let _ = child.kill();
                let _ = child.wait();
                let claimed = {
                    let mut inner = self.locked();
                    inner.jobs.get_mut(id).and_then(|h| self.claim(id, h))
                };
                if let Some(record) = claimed {
                    self.flush(id, record)?;
                }
                return Ok(outcome);
            }
            std::thread::sleep(POLL);
        }
    }

    /// Terminate a job. A running job is signalled and `wait` then reports it
    /// `Killed`; a job still awaiting approval is withdrawn rather than left
    /// pending until the approval timeout.
    pub fn kill(&self, id: &JobId, caller: &Caller) -> Result<(), JobError> {
        let claimed = {
            let mut inner = self.locked();
            let Inner { jobs, store } = &mut *inner;
            let handle = jobs
                .get_mut(id)
                .ok_or_else(|| JobError::NotFound(id.clone()))?;

            // **Refused, never silently retargeted and never silently ignored.**
            // A caller that asked to stop something and got a quiet success
            // would believe it had.
            if !caller.may_write(&handle.scope) {
                return Err(JobError::ForeignWorkspace {
                    job: id.clone(),
                    owner: handle.scope.to_string(),
                    caller: caller.to_string(),
                });
            }

            if handle.status == JobStatus::PendingApproval {
                handle.status = JobStatus::Denied {
                    reason: DeniedReason::Decision,
                };
                handle.verdict = Verdict::RejectedOnce;
                handle.pending = None;
                let claimed = self.claim(id, handle);
                let _ = persist(store, id, handle);
                claimed
            } else {
                // Record operator intent regardless of the signal's result:
                // `killed` is why `wait` reports `Killed` rather than an exit
                // code. A terminal job has `child == None`, so killing it is a
                // no-op.
                if let Some(child) = handle.child.as_mut() {
                    handle.killed = true;
                    let _ = child.kill();
                    let _ = persist(store, id, handle);
                }
                None
            }
        };

        // Lock released before the audit write: the sink can block, and on
        // failure `flush` re-locks to release the claim.
        if let Some(record) = claimed {
            self.flush(id, record)?;
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

impl Drop for JobRegistry {
    /// Flush a record for every job that never reached a caller.
    ///
    /// Recording otherwise happens when a job is awaited, so a job that is
    /// started and abandoned — completed, killed, or still running at shutdown —
    /// would leave no history at all. "Every attempt is durable history" cannot
    /// depend on someone calling `wait`.
    ///
    /// v1 has no background reaper, so this lands at shutdown rather than at the
    /// moment the child exits; a reaper thread is the real answer.
    ///
    /// implements: audit-every-attempt
    fn drop(&mut self) {
        let mut inner = self
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());

        let ids: Vec<JobId> = inner
            .jobs
            .iter()
            .filter(|(_, handle)| !handle.audited)
            .map(|(id, _)| id.clone())
            .collect();

        let Inner { jobs, store } = &mut *inner;
        for id in ids {
            let Some(handle) = jobs.get_mut(&id) else {
                continue;
            };
            if !handle.status.is_terminal() {
                // A job still running does **not** end because we are going
                // away — that is the whole point of the durable registry. Only
                // a child that has already exited on its own has a terminal
                // state to write; everything else stays `Running` on disk for a
                // successor to re-adopt.
                match handle
                    .child
                    .as_mut()
                    .and_then(|c| c.try_wait().ok())
                    .flatten()
                {
                    Some(exit) if !handle.killed => {
                        handle.status = JobStatus::Done {
                            code: exit.code().unwrap_or(-1),
                        };
                    }
                    Some(_) => handle.status = JobStatus::Killed,
                    None => {
                        // Alive and outliving us: leave the record as-is.
                        let _ = persist(store, &id, handle);
                        continue;
                    }
                }
            }
            let record = AuditRecord::new(
                id.0.clone(),
                handle.argv.clone(),
                handle.resolved_cwd.clone(),
                handle.verdict,
                handle.status.clone(),
            );
            if self.audit.append(record).is_ok() {
                handle.audited = true;
            }
            let _ = persist(store, &id, handle);
        }
    }
}
