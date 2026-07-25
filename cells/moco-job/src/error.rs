use std::fmt;

use crate::job::JobId;

/// Errors from the job substrate.
#[derive(Debug)]
pub enum JobError {
    /// A job was started with no program to run.
    EmptyArgv,
    /// The program could not be spawned. Names the binary **and** the PATH that
    /// was searched, so the fix is obvious rather than a guess.
    Spawn {
        program: String,
        searched_path: String,
        source: std::io::Error,
    },
    /// No job with this id is registered.
    NotFound(JobId),
    /// The job's cwd does not resolve inside the node's allowed root.
    CwdEscape { cwd: String, root: String },
    /// The job's cwd is not valid UTF-8, so an audit record could not name it
    /// faithfully. Rejected up front rather than recorded inaccurately.
    CwdNotUtf8 { cwd: String },
    /// A decision was offered for a job that is not awaiting one.
    NotPending(JobId),
    /// The node's committed rule seed could not be parsed.
    Seed(String),
    /// An audit record could not be written or read back.
    Audit(String),
    /// An I/O error handling a job's file-backed output.
    Io(std::io::Error),
}

impl fmt::Display for JobError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            JobError::EmptyArgv => write!(f, "empty argv: a job needs a program to run"),
            JobError::Spawn {
                program,
                searched_path,
                source,
            } => {
                write!(
                    f,
                    "failed to spawn '{program}': {source} (PATH searched: {searched_path})"
                )
            }
            JobError::NotFound(id) => write!(f, "no such job: {id}"),
            JobError::CwdEscape { cwd, root } => {
                write!(
                    f,
                    "cwd '{cwd}' does not resolve inside allowed root '{root}'"
                )
            }
            JobError::CwdNotUtf8 { cwd } => {
                write!(f, "cwd '{cwd}' is not valid UTF-8")
            }
            JobError::NotPending(id) => write!(f, "job is not awaiting approval: {id}"),
            JobError::Seed(msg) => write!(f, "{msg}"),
            JobError::Audit(msg) => write!(f, "{msg}"),
            JobError::Io(e) => write!(f, "I/O error: {e}"),
        }
    }
}

impl std::error::Error for JobError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            JobError::Spawn { source, .. } => Some(source),
            JobError::Io(e) => Some(e),
            _ => None,
        }
    }
}
