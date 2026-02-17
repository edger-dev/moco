use std::fmt;

#[derive(Debug)]
pub enum TtyError {
    PtyAllocation(nix::Error),
    SpawnFailure(String),
    ProcessNotFound(u32),
    ProcessNotRunning(u32),
    Io(std::io::Error),
}

impl fmt::Display for TtyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TtyError::PtyAllocation(e) => write!(f, "failed to allocate PTY: {e}"),
            TtyError::SpawnFailure(msg) => write!(f, "failed to spawn process: {msg}"),
            TtyError::ProcessNotFound(pid) => write!(f, "process not found: {pid}"),
            TtyError::ProcessNotRunning(pid) => write!(f, "process not running: {pid}"),
            TtyError::Io(e) => write!(f, "I/O error: {e}"),
        }
    }
}

impl std::error::Error for TtyError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            TtyError::PtyAllocation(e) => Some(e),
            TtyError::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<nix::Error> for TtyError {
    fn from(e: nix::Error) -> Self {
        TtyError::PtyAllocation(e)
    }
}

impl From<std::io::Error> for TtyError {
    fn from(e: std::io::Error) -> Self {
        TtyError::Io(e)
    }
}
