//! The registry's durable state: one record per job, on disk.
//!
//! This is what makes a job outlive the daemon that started it. Everything the
//! supervisor needs to pick a job back up after a restart lives here — its
//! identity, what ran, where its output is, and the `(pid, start-time)` pair
//! that says whether it is still the same process.
//!
//! Records are written **atomically** (temp + rename), so a daemon dying
//! mid-write leaves either the old record or the new one, never a torn one.
//!
//! implements: registry-is-node-state-on-disk

use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use facet::Facet;

use crate::audit::{Verdict, escape_field, unescape_field};
use crate::error::JobError;
use crate::job::{JobId, JobStatus};
use crate::lifecycle::{Lifetime, RestartPolicy};
use crate::scope::Scope;

/// Suffix of a committed record. Anything else in the directory is ignored, so
/// a half-written temp file is never mistaken for state.
const RECORD_EXT: &str = "job";

/// Process-global sequence, so two registries in one process minting ids in the
/// same millisecond still differ before the exclusive-create check.
static MINT_SEQ: AtomicU64 = AtomicU64::new(0);

/// One job's durable state.
///
/// `pid`/`pid_start` are `0` when the job never spawned (denied, pending, or
/// failed to start) — a real child never has pid 0, so the sentinel is
/// unambiguous and keeps the record a flat Styx line.
#[derive(Facet, Debug, Clone, PartialEq, Eq)]
pub struct JobRecord {
    pub id: String,
    pub argv: Vec<String>,
    pub cwd: String,
    pub verdict: Verdict,
    pub status: JobStatus,
    /// Who owns this job. Persisted, so ownership survives the daemon that
    /// recorded it along with everything else about the job.
    pub scope: Scope,
    /// The manifest name it was declared under; empty for an ad-hoc job. A
    /// sentinel rather than an option, to keep the record one flat Styx line.
    pub name: String,
    pub lifetime: Lifetime,
    pub restart: RestartPolicy,
    /// The port this job holds; 0 for none. Persisted so a re-adopting daemon
    /// keeps it and a peer daemon sees it in its scan.
    pub port: u16,
    /// How many times the supervisor has brought this job back.
    pub restarts: u64,
    /// The child's pid, or 0 if it never spawned.
    pub pid: u32,
    /// The kernel start time of `pid`. Compared on every probe so a **reused**
    /// pid is never mistaken for the original process. 0 when unknown.
    pub pid_start: u64,
    /// Absolute path of the file its stdout+stderr are captured to. The
    /// scrollback *is* this file, which is why `tail` survives re-adoption.
    pub capture: String,
    /// Execution deadline in milliseconds; 0 means unbounded.
    pub deadline_ms: u64,
    /// Whether its terminal record has already reached the audit.
    pub audited: bool,
    /// The declared machine-lens sidecar, relative to the job's directory.
    pub machine_file: String,
    /// What is in it.
    pub machine_format: String,
    /// Bytes discarded from the front of the capture by compaction.
    ///
    /// Persisted, so a re-adopting daemon keeps handing out logical offsets
    /// that mean the same thing as the ones it handed out before the restart.
    pub dropped: u64,
    /// True when this job was **handed over** rather than started here.
    ///
    /// Persisted, so re-adoption preserves it: "we were given this" and "we
    /// started this and detached" are different facts, and only the first means
    /// something else may own its lifecycle.
    pub external: bool,
}

impl JobRecord {
    /// Escape the free-text fields so a record is always exactly one line.
    ///
    /// Same reasoning as the audit log: an untrusted argv containing newlines
    /// would otherwise render as a multi-line Styx heredoc and make the record
    /// unreadable.
    fn escaped(&self) -> Self {
        Self {
            argv: self.argv.iter().map(|a| escape_field(a)).collect(),
            cwd: escape_field(&self.cwd),
            capture: escape_field(&self.capture),
            ..self.clone()
        }
    }

    fn unescaped(self) -> Self {
        Self {
            argv: self.argv.iter().map(|a| unescape_field(a)).collect(),
            cwd: unescape_field(&self.cwd),
            capture: unescape_field(&self.capture),
            ..self
        }
    }
}

/// The on-disk home of one node's job records and their capture files.
///
/// A daemon is handed this directory; two daemons may share it, which is why id
/// minting resolves collisions by exclusive creation rather than by trusting a
/// counter.
#[derive(Debug, Clone)]
pub struct RecordStore {
    dir: PathBuf,
}

impl RecordStore {
    /// Open (creating if needed) the registry directory.
    ///
    /// Created owner-only **at creation time**, not by a later `chmod`: there
    /// must be no window in which another user can read a capture or plant a
    /// symlink where a record is about to be written.
    pub fn open(dir: impl Into<PathBuf>) -> Result<Self, JobError> {
        let dir = dir.into();
        if !dir.exists() {
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt;
                std::fs::DirBuilder::new()
                    .recursive(true)
                    .mode(0o700)
                    .create(&dir)
                    .map_err(JobError::Io)?;
            }
            #[cfg(not(unix))]
            std::fs::create_dir_all(&dir).map_err(JobError::Io)?;
        }
        Ok(Self { dir })
    }

    /// A fresh private directory under the node's runtime root.
    ///
    /// The default when no daemon has named a shared directory. It persists —
    /// nothing here is removed when a registry drops — but it is not shared, so
    /// it never re-adopts another registry's jobs.
    pub fn private() -> Result<Self, JobError> {
        let root = std::env::var_os("XDG_RUNTIME_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir)
            .join("moco")
            .join("jobs");
        let dir = root.join(format!(
            "{}-{}",
            std::process::id(),
            MINT_SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        Self::open(dir)
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn record_path(&self, id: &str) -> PathBuf {
        self.dir.join(format!("{id}.{RECORD_EXT}"))
    }

    /// Path of a job's capture file. Derived from the id, so re-adoption finds
    /// the scrollback without storing a second index.
    pub fn capture_path(&self, id: &str) -> PathBuf {
        self.dir.join(format!("{id}.log"))
    }

    /// Mint a node-unique job id.
    ///
    /// The candidate is time+pid+sequence derived, but uniqueness does **not**
    /// rest on that: the id is claimed by creating its record file with
    /// `create_new`, so two daemons racing on one directory cannot both win.
    /// Whoever loses simply takes the next candidate.
    pub fn mint(&self) -> Result<JobId, JobError> {
        for _ in 0..64 {
            let millis = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_millis())
                .unwrap_or_default();
            let candidate = format!(
                "{millis:013}-{}-{}",
                std::process::id(),
                MINT_SEQ.fetch_add(1, Ordering::Relaxed)
            );
            match File::options()
                .create_new(true)
                .write(true)
                .open(self.record_path(&candidate))
            {
                Ok(_) => return Ok(JobId(candidate)),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(JobError::Io(e)),
            }
        }
        Err(JobError::Audit(
            "could not mint a unique job id after 64 attempts".into(),
        ))
    }

    /// Write a record atomically: a full temp file, then a rename over the
    /// target. A crash mid-write leaves the previous record intact.
    pub fn put(&self, record: &JobRecord) -> Result<(), JobError> {
        use std::io::Write;

        let line = facet_styx::to_string_compact(&record.escaped())
            .map_err(|e| JobError::Audit(format!("failed to encode job record: {e}")))?;
        if line.contains('\n') {
            return Err(JobError::Audit(
                "refusing to write a multi-line job record".into(),
            ));
        }

        // Unique temp name: two daemons writing the same record concurrently
        // must not share a scratch path.
        let tmp = self.dir.join(format!(
            ".{}.{}.tmp",
            record.id,
            MINT_SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        let mut options = File::options();
        options.create(true).truncate(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&tmp).map_err(JobError::Io)?;
        file.write_all(line.as_bytes()).map_err(JobError::Io)?;
        file.sync_data().map_err(JobError::Io)?;
        drop(file);

        std::fs::rename(&tmp, self.record_path(&record.id)).map_err(|e| {
            let _ = std::fs::remove_file(&tmp);
            JobError::Io(e)
        })
    }

    /// Every committed record in the directory.
    ///
    /// A **missing directory** is simply no jobs — nothing is declared, and that
    /// is not an error. A record that **exists but cannot be decoded** is an
    /// error naming the file: records are written atomically, so there is no
    /// torn-write case to tolerate, and silently dropping one would report a
    /// job as gone when it is merely unreadable.
    ///
    /// (An id freshly claimed by `mint` is an empty file with no record yet;
    /// that is a claim in flight, not corruption, so it is skipped.)
    ///
    /// implements: config-failure-never-degrades-to-empty
    pub fn all(&self) -> Result<Vec<JobRecord>, JobError> {
        let entries = match std::fs::read_dir(&self.dir) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(JobError::Io(e)),
        };

        let mut out = Vec::new();
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some(RECORD_EXT) {
                continue;
            }
            let text = std::fs::read_to_string(&path).map_err(JobError::Io)?;
            let text = text.trim();
            // A freshly minted, not-yet-written id claim is an empty file.
            if text.is_empty() {
                continue;
            }
            // A record is a braced *expression*, not a document root, so it is
            // read back with `from_str_expr`.
            let record = facet_styx::from_str_expr::<JobRecord>(text).map_err(|e| {
                JobError::Audit(format!(
                    "job record '{}' could not be decoded: {e}",
                    path.display()
                ))
            })?;
            out.push(record.unescaped());
        }
        // Ids lead with a zero-padded millisecond stamp, so lexical order is
        // creation order — a stable listing without a second sort key.
        out.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(out)
    }

    /// Note that a declaration holds `port`, without there being a live job.
    ///
    /// Written as an ordinary terminal record so the *same* scan that computes
    /// the reserved set also finds it — a second index would be a second thing
    /// to keep true.
    pub fn remember_port(
        &self,
        scope: &crate::scope::Scope,
        name: &str,
        port: u16,
    ) -> Result<(), JobError> {
        let id = self.mint()?;
        self.put(&JobRecord {
            id: id.0,
            argv: Vec::new(),
            cwd: String::new(),
            verdict: Verdict::Ungoverned,
            status: JobStatus::Done { code: 0 },
            scope: scope.clone(),
            name: name.to_string(),
            lifetime: Lifetime::OneShot,
            restart: RestartPolicy::Never,
            restarts: 0,
            port,
            machine_file: String::new(),
            machine_format: String::new(),
            dropped: 0,
            external: false,
            pid: 0,
            pid_start: 0,
            capture: String::new(),
            deadline_ms: 0,
            audited: true,
        })
    }

    /// Forget a job entirely: its record and its capture.
    pub fn remove(&self, id: &str) -> Result<(), JobError> {
        let _ = std::fs::remove_file(self.capture_path(id));
        match std::fs::remove_file(self.record_path(id)) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(JobError::Io(e)),
        }
    }
}

/// Build the record for a job in a given state.
#[allow(clippy::too_many_arguments)]
pub(crate) fn record_of(
    id: &JobId,
    argv: &[String],
    cwd: &Path,
    verdict: Verdict,
    status: &JobStatus,
    scope: &Scope,
    name: Option<&str>,
    lifetime: Lifetime,
    restart: RestartPolicy,
    restarts: u64,
    port: u16,
    machine_file: &str,
    machine_format: &str,
    dropped: u64,
    external: bool,
    pid: u32,
    pid_start: u64,
    capture: &Path,
    deadline_ms: u64,
    audited: bool,
) -> JobRecord {
    JobRecord {
        id: id.0.clone(),
        argv: argv.to_vec(),
        cwd: cwd.to_string_lossy().into_owned(),
        verdict,
        status: status.clone(),
        scope: scope.clone(),
        name: name.unwrap_or_default().to_string(),
        lifetime,
        restart,
        restarts,
        port,
        machine_file: machine_file.to_string(),
        machine_format: machine_format.to_string(),
        dropped,
        external,
        pid,
        pid_start,
        capture: capture.to_string_lossy().into_owned(),
        deadline_ms,
        audited,
    }
}
