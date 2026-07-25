use std::fmt;

/// Errors from the job substrate.
///
/// v1 scaffold: only `NotImplemented` exists — the registry methods are stubs
/// that return it (rule `jig::rust::no-todo-committed` forbids `todo!()`, so a
/// not-yet-built path returns a real error instead). Spawn / not-found / I/O
/// variants land alongside the implementation that constructs them.
#[derive(Debug)]
pub enum JobError {
    /// The operation is scaffolded but not yet implemented.
    NotImplemented,
}

impl fmt::Display for JobError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            JobError::NotImplemented => write!(f, "not implemented"),
        }
    }
}

impl std::error::Error for JobError {}
