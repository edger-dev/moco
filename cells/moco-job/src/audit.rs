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

/// Escape a field so it can never contain a raw newline.
///
/// **Load-bearing for the file format.** Styx renders any string containing two
/// or more newlines as a *heredoc*, which spans several lines — so an untrusted
/// argv like `["echo", "a\nb\nc"]` would otherwise split one record across five
/// lines and make the whole log unreadable (and, under a skip-tolerant reader,
/// let a caller forge records). Escaping before serialization keeps every record
/// exactly one line whatever the payload; `unescape_field` restores the value
/// byte-for-byte, so fidelity is preserved.
fn escape_field(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
}

/// Reverse `escape_field`, left to right so `\\n` reads back as a literal
/// backslash followed by `n`, not as a newline.
fn unescape_field(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('r') => out.push('\r'),
            Some('\\') => out.push('\\'),
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}

impl AuditRecord {
    fn escaped(&self) -> Self {
        Self {
            job: self.job,
            argv: self.argv.iter().map(|a| escape_field(a)).collect(),
            cwd: escape_field(&self.cwd),
            verdict: self.verdict,
            status: self.status.clone(),
            at_unix_ms: self.at_unix_ms,
        }
    }

    fn unescaped(self) -> Self {
        Self {
            argv: self.argv.iter().map(|a| unescape_field(a)).collect(),
            cwd: unescape_field(&self.cwd),
            ..self
        }
    }
}

impl AuditSink for FileAuditLog {
    fn append(&self, record: AuditRecord) -> Result<(), JobError> {
        use std::io::Write;

        let line = facet_styx::to_string_compact(&record.escaped())
            .map_err(|e| JobError::Audit(format!("failed to encode audit record: {e}")))?;
        // Belt and braces: the escaping above should make this impossible, but a
        // multi-line record would silently corrupt every later read, so refuse to
        // write one rather than trust the encoder.
        if line.contains('\n') {
            return Err(JobError::Audit(
                "refusing to write a multi-line audit record".into(),
            ));
        }

        let _guard = self
            .write_lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut options = std::fs::File::options();
        options.create(true).append(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            // Owner-only from creation: the history must not be world-readable.
            options.mode(0o600);
        }
        let mut file = options.open(&self.path).map_err(JobError::Io)?;

        // One write, not `writeln!`: `write_fmt` can issue the payload and the
        // newline as separate syscalls, which lets a second process interleave
        // between them. A single `write_all` under O_APPEND is atomic for a
        // local regular file.
        let mut buf = line.into_bytes();
        buf.push(b'\n');
        file.write_all(&buf).map_err(JobError::Io)?;
        // Reach the disk, not just the page cache — this log is the durable
        // history the whole design leans on.
        file.sync_data().map_err(JobError::Io)?;
        Ok(())
    }

    fn records(&self) -> Result<Vec<AuditRecord>, JobError> {
        let text = match std::fs::read_to_string(&self.path) {
            Ok(text) => text,
            // No file yet simply means no history.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(JobError::Io(e)),
        };
        // Skip any line that does not parse rather than failing the whole read:
        // a torn final line from a crash must not cost every prior record. This
        // is only safe because `append` guarantees one record per line, so a
        // caller cannot smuggle an extra line in through a payload.
        Ok(text
            .lines()
            .filter(|line| !line.trim().is_empty())
            .filter_map(|line| {
                // `to_string_compact` emits a braced expression (`{job 1, …}`),
                // which is an expression rather than a document root — so it is
                // read back with `from_str_expr`, not `from_str`.
                facet_styx::from_str_expr::<AuditRecord>(line).ok()
            })
            .map(AuditRecord::unescaped)
            .collect())
    }
}
