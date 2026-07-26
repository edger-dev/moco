//! Start-time gates, with informative refusals.
//!
//! Two axes, one shape. Both are checked when a **declared** job starts, both
//! refuse rather than fail obscurely later, and both name what refused — the
//! reusable part is the shape, not either axis.
//!
//! These are **policy** gates. They sit alongside the node's rule-set, never in
//! place of it: passing them says a job is allowed *here*, not that its argv is
//! allowed at all.
//!
//! implements: admission-gates-worktree-and-host

use std::path::Path;

use facet::Facet;

/// Where a declared job may run within its repo.
#[derive(Facet, Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(u8)]
pub enum WorktreePolicy {
    /// Not stated. Resolves to `MainOnly` when the job declares a **fixed**
    /// port and `Each` otherwise — an implication, not a rule, so an explicit
    /// value always wins.
    ///
    /// A third variant rather than an `Option` because the distinction that
    /// matters is *stated vs not*, and a type that can say so is better than a
    /// convention about which value means "unset".
    #[default]
    Unset,
    /// Every worktree of the repo runs its own instance.
    Each,
    /// Only the main working tree runs it.
    MainOnly,
}

impl WorktreePolicy {
    /// The effective policy, given whether a fixed port was declared.
    ///
    /// Two worktrees cannot bind one port, so a fixed port that ran everywhere
    /// would be a race with a confusing outcome rather than a feature.
    pub fn resolve(&self, fixed_port: bool) -> Self {
        match self {
            WorktreePolicy::Unset if fixed_port => WorktreePolicy::MainOnly,
            WorktreePolicy::Unset => WorktreePolicy::Each,
            other => *other,
        }
    }
}

/// Is this workspace root the repo's **main** working tree?
///
/// Reuses a distinction git already draws rather than inventing one: a main
/// tree's `.git` is a **directory**, a linked worktree's is a **file** pointing
/// back at it. (Workspace *identity* treats both alike — either one is a
/// workspace root — so this is the same fact serving a second, different
/// purpose.)
pub fn is_main_worktree(root: impl AsRef<Path>) -> bool {
    root.as_ref().join(".git").is_dir()
}

/// Why a start was refused before it began.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    /// Declared `main-only`, and this is a linked worktree.
    NotMainWorktree { implied_by_fixed_port: bool },
    /// This node is not on the job's allow-list.
    WrongHost { node: String, allowed: Vec<String> },
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Refusal::NotMainWorktree {
                implied_by_fixed_port: false,
            } => f.write_str(
                "it is declared worktree=main-only, and this is a linked worktree. \
                 Run it from the repo's main working tree, or declare worktree=each \
                 to give every worktree its own instance.",
            ),
            Refusal::NotMainWorktree {
                implied_by_fixed_port: true,
            } => f.write_str(
                "it declares a fixed port, which implies worktree=main-only because \
                 two worktrees cannot bind one port — and this is a linked worktree. \
                 Declare worktree=each explicitly if every worktree really should run \
                 it, or use port=auto so each gets its own.",
            ),
            Refusal::WrongHost { node, allowed } => write!(
                f,
                "this node is '{node}', and the job is declared to run only on: {}. \
                 One manifest is shared across machines, so this is how a \
                 machine-specific job stays on its machine.",
                allowed.join(", ")
            ),
        }
    }
}

/// Check both gates for a declared job.
///
/// Returns the **first** refusal, so the message names one concrete thing to
/// fix rather than a list.
pub fn check(
    root: impl AsRef<Path>,
    node: &str,
    worktree: WorktreePolicy,
    fixed_port: bool,
    hosts: &[String],
) -> Result<(), Refusal> {
    if worktree.resolve(fixed_port) == WorktreePolicy::MainOnly && !is_main_worktree(&root) {
        return Err(Refusal::NotMainWorktree {
            implied_by_fixed_port: worktree == WorktreePolicy::Unset,
        });
    }
    // Absent means anywhere: the permissive default, so a manifest says nothing
    // about hosts until it needs to.
    if !hosts.is_empty() && !hosts.iter().any(|h| h == node) {
        return Err(Refusal::WrongHost {
            node: node.to_string(),
            allowed: hosts.to_vec(),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fixed_port_only_implies_main_only_when_nothing_was_stated() {
        assert_eq!(
            WorktreePolicy::Unset.resolve(true),
            WorktreePolicy::MainOnly
        );
        assert_eq!(WorktreePolicy::Unset.resolve(false), WorktreePolicy::Each);
        // An explicit value is never overridden by the implication.
        assert_eq!(WorktreePolicy::Each.resolve(true), WorktreePolicy::Each);
    }

    #[test]
    fn an_empty_host_list_admits_every_node() {
        assert!(check("/nowhere", "anything", WorktreePolicy::Each, false, &[]).is_ok());
    }
}
