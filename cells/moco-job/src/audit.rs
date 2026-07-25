use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use facet::Facet;

use crate::error::JobError;
use crate::job::JobStatus;

/// **Under what authority** a job reached its terminal state.
///
/// The value returned to the caller on an `Outcome` is the *same* value written
/// to the audit — one source of truth, so the agent never has to guess whether
/// an unmatched command was auto-allowed or approved once.
///
/// implements: agent-self-sufficiency
/// implements: audit-every-attempt
#[derive(Facet, Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Verdict {
    /// No policy gated this registry.
    Ungoverned,
    /// A committed seed allow rule permitted this exact argv.
    SeedAllow,
    /// A human approved this instance (possibly with a corrected argv).
    ApprovedOnce,
    /// A committed seed deny rule forbade this exact argv.
    SeedDeny,
    /// A human rejected this instance.
    RejectedOnce,
    /// Nobody decided within the approval window — the fail-closed default.
    NoApprover,
}

impl Verdict {
    /// Did this verdict permit the job to run?
    pub fn permitted(&self) -> bool {
        matches!(
            self,
            Verdict::Ungoverned | Verdict::SeedAllow | Verdict::ApprovedOnce
        )
    }
}

/// One durable record of one attempt — including every denied one.
///
/// implements: audit-every-attempt
#[derive(Facet, Debug, Clone, PartialEq, Eq)]
pub struct AuditRecord {
    /// The registry-local job id.
    pub job: u64,
    /// Exactly the argv that ran (or would have run) — never a shell string, so
    /// the record is unambiguous.
    pub argv: Vec<String>,
    /// The absolute, symlink-resolved directory it was confined to.
    pub cwd: String,
    /// The authority it resolved under.
    pub verdict: Verdict,
    /// Its terminal state.
    pub status: JobStatus,
    /// When the record was written, in milliseconds since the unix epoch.
    pub at_unix_ms: u64,
}

impl AuditRecord {
    pub fn new(
        job: u64,
        argv: Vec<String>,
        cwd: PathBuf,
        verdict: Verdict,
        status: JobStatus,
    ) -> Self {
        Self {
            job,
            argv,
            cwd: cwd.display().to_string(),
            verdict,
            status,
            at_unix_ms: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0),
        }
    }
}

/// Where audit records are appended.
///
/// Append-only by contract: there is no update or delete. v1 ships an in-memory
/// and a local file sink; the encrypted never-delete backup pipeline slots in
/// behind this same trait without touching the registry.
///
/// implements: audit-every-attempt
pub trait AuditSink: Send + Sync {
    /// Append one record. Called at the moment a job reaches a terminal state.
    fn append(&self, record: AuditRecord) -> Result<(), JobError>;

    /// Read the history back — read-only introspection, so the agent can learn
    /// what worked and what got denied.
    ///
    /// implements: agent-self-sufficiency
    fn records(&self) -> Result<Vec<AuditRecord>, JobError>;
}

/// An in-process audit log. The default sink: always present, so every registry
/// has a history even when no durable sink is configured.
#[derive(Debug, Default)]
pub struct MemoryAuditLog {
    records: Mutex<Vec<AuditRecord>>,
}

impl MemoryAuditLog {
    pub fn new() -> Self {
        Self::default()
    }

    fn locked(&self) -> std::sync::MutexGuard<'_, Vec<AuditRecord>> {
        self.records
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

impl AuditSink for MemoryAuditLog {
    fn append(&self, record: AuditRecord) -> Result<(), JobError> {
        self.locked().push(record);
        Ok(())
    }

    fn records(&self) -> Result<Vec<AuditRecord>, JobError> {
        Ok(self.locked().clone())
    }
}

/// A durable, append-only audit log: one Styx record per line, opened in append
/// mode so existing history can never be rewritten.
///
/// implements: audit-every-attempt
#[derive(Debug)]
pub struct FileAuditLog {
    path: PathBuf,
    /// Serializes appends so two records can never interleave mid-line.
    write_lock: Mutex<()>,
}

impl FileAuditLog {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            write_lock: Mutex::new(()),
        }
    }

    pub fn path(&self) -> &std::path::Path {
        &self.path
    }
}

impl AuditSink for FileAuditLog {
    fn append(&self, record: AuditRecord) -> Result<(), JobError> {
        use std::io::Write;

        let line = facet_styx::to_string_compact(&record)
            .map_err(|e| JobError::Audit(format!("failed to encode audit record: {e}")))?;

        let _guard = self
            .write_lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut file = std::fs::File::options()
            .create(true)
            .append(true)
            .open(&self.path)
            .map_err(JobError::Io)?;
        writeln!(file, "{line}").map_err(JobError::Io)?;
        Ok(())
    }

    fn records(&self) -> Result<Vec<AuditRecord>, JobError> {
        let text = match std::fs::read_to_string(&self.path) {
            Ok(text) => text,
            // No file yet simply means no history.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(JobError::Io(e)),
        };
        text.lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| {
                facet_styx::from_str(line)
                    .map_err(|e| JobError::Audit(format!("failed to decode audit record: {e}")))
            })
            .collect()
    }
}
