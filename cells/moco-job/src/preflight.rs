use std::path::PathBuf;

use crate::rules::Disposition;

/// What a real run *would* do, resolved without queuing or running anything.
///
/// Every field is diagnostic: preflight reports all of them at once rather than
/// short-circuiting on the first problem, so one read-only call kills most
/// trial-and-error before it spends a human approval.
///
/// implements: agent-self-sufficiency
#[derive(Debug, Clone)]
pub struct Preflight {
    /// The disposition the rule-set gives this exact argv.
    pub disposition: Disposition,
    /// Absolute path `argv[0]` resolves to on the daemon's PATH, or `None` if
    /// it does not resolve (paired with `effective_path` so the fix is obvious).
    pub program: Option<PathBuf>,
    /// The PATH the daemon would spawn with.
    pub effective_path: String,
    /// The absolute, symlink-resolved cwd the job would be confined to.
    pub resolved_cwd: Option<PathBuf>,
    /// Why the cwd was rejected, when `resolved_cwd` is `None`.
    pub cwd_error: Option<String>,
}
