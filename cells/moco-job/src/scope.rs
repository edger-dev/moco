//! Whose job is this?
//!
//! Ownership is the question every later supervision feature assumes an answer
//! to — lenses, restart policies, ports and admission gates all attach to *a
//! job belonging to someone* — so it is settled first, and settled so that there
//! is always an answer and never a null.
//!
//! implements: workspace-is-the-owner-not-session

use std::fmt;
use std::path::Path;

use facet::Facet;

/// Who owns a job.
///
/// A **session does not**: sessions are ephemeral clients that attach, and a
/// session restart must never orphan or kill a dev server. The owner is the
/// place the work belongs to.
#[derive(Facet, Debug, Clone, PartialEq, Eq)]
#[repr(u8)]
pub enum Scope {
    /// A git worktree root — or, outside a repo, the directory itself.
    Workspace { root: String },
    /// Node-level and cwd-independent: infrastructure that belongs to no repo.
    ///
    /// A node daemon lives here. Keying it to a synthetic repo workspace would
    /// couple it to a checkout that may not exist, and would block running a
    /// prebuilt binary with no working tree at all.
    System,
}

impl Scope {
    /// A workspace at `root`.
    pub fn workspace(root: impl AsRef<Path>) -> Self {
        Scope::Workspace {
            root: root.as_ref().to_string_lossy().into_owned(),
        }
    }

    /// Resolve the workspace containing `dir`.
    ///
    /// Walks up for a `.git` entry and takes the directory holding it — in
    /// **either** form. A main working tree's `.git` is a directory and a linked
    /// worktree's is a file, and *both* are workspace roots: treating only the
    /// directory form as a root would silently attribute a linked worktree's
    /// jobs to whatever ancestor happened to have one, or to nothing at all.
    ///
    /// Outside a repo the answer is the directory itself, canonicalized. There
    /// is deliberately no "no workspace" case for callers to handle.
    pub fn resolve(dir: impl AsRef<Path>) -> Self {
        let dir = dir.as_ref();
        let start = dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf());
        let mut cursor = start.as_path();
        loop {
            if cursor.join(".git").exists() {
                return Scope::workspace(cursor);
            }
            match cursor.parent() {
                Some(parent) => cursor = parent,
                None => return Scope::workspace(&start),
            }
        }
    }

    /// The workspace root, if this is a workspace.
    pub fn root(&self) -> Option<&str> {
        match self {
            Scope::Workspace { root } => Some(root),
            Scope::System => None,
        }
    }
}

impl fmt::Display for Scope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Scope::Workspace { root } => f.write_str(root),
            Scope::System => f.write_str("system"),
        }
    }
}

/// Who is asking, for the purpose of **writes**.
///
/// Reads are node-global and need none of this: any session may ask what is
/// running on the machine, which is the question a machine-global supervisor
/// exists to answer.
///
/// implements: reads-global-writes-own-workspace
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Caller {
    /// A session in a workspace. It may write to that workspace's jobs, and to
    /// no others.
    Scoped(Scope),
    /// The human console — the deliberate carve-out for global action, such as
    /// hand-killing a job belonging to another checkout.
    ///
    /// Spelled out as its own variant rather than an absent value: global
    /// authority is something a caller **states**, not something it acquires by
    /// leaving a field off.
    Console,
}

impl Caller {
    /// May this caller write to a job owned by `owner`?
    pub fn may_write(&self, owner: &Scope) -> bool {
        match self {
            Caller::Console => true,
            Caller::Scoped(scope) => scope == owner,
        }
    }
}

impl fmt::Display for Caller {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Caller::Scoped(scope) => write!(f, "{scope}"),
            Caller::Console => f.write_str("the console"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_console_may_write_anywhere_and_a_workspace_only_to_itself() {
        let a = Scope::workspace("/a");
        let b = Scope::workspace("/b");

        assert!(Caller::Console.may_write(&a));
        assert!(Caller::Scoped(a.clone()).may_write(&a));
        assert!(!Caller::Scoped(a).may_write(&b));
    }

    #[test]
    fn system_scope_is_not_any_workspace() {
        assert!(!Caller::Scoped(Scope::workspace("/a")).may_write(&Scope::System));
        assert!(Caller::Console.may_write(&Scope::System));
    }
}
