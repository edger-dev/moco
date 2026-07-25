use std::path::PathBuf;
use std::time::Duration;

use facet::Facet;

use crate::error::JobError;

/// What a rule-set says about one exact argv.
///
/// implements: four-verdicts-exact-argv (the matching half)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Disposition {
    /// A rule permits this exact argv — it may spawn.
    Allow,
    /// No rule matches. Fail-closed: it becomes a pending job awaiting a human
    /// decision, and is denied if none arrives in time.
    NeedsApproval,
    /// A rule forbids this exact argv — it never spawns.
    Deny,
}

/// The committed seed: the reviewed, git-tracked half of a node's rule-set.
///
/// Two lists of **exact** argvs. This is the shape parsed from the node's
/// committed Styx config — the only way a standing rule enters v1.
///
/// implements: rule-set-target-owned-human-mutated
#[derive(Facet, Debug, Default, Clone)]
pub struct SeedConfig {
    pub allow: Vec<Vec<String>>,
    pub deny: Vec<Vec<String>>,
}

/// A node's allow/deny rule-set, matched on **exact argv**.
///
/// Owned by the target node. There is deliberately **no method that adds,
/// removes, or widens a rule** — a requesting agent may evaluate and read, never
/// write. Standing rules come from the committed seed; the human-gated overlay
/// writer (the "always" verdicts) is a later slice.
///
/// implements: rule-set-target-owned-human-mutated
/// implements: four-verdicts-exact-argv
#[derive(Debug, Default, Clone)]
pub struct RuleSet {
    allow: Vec<Vec<String>>,
    deny: Vec<Vec<String>>,
}

impl RuleSet {
    /// Build a rule-set from an already-parsed seed.
    pub fn from_seed(seed: SeedConfig) -> Self {
        Self {
            allow: seed.allow,
            deny: seed.deny,
        }
    }

    /// Parse a node's committed Styx seed config.
    pub fn from_styx(input: &str) -> Result<Self, JobError> {
        let seed: SeedConfig = facet_styx::from_str(input)
            .map_err(|e| JobError::Seed(format!("failed to parse rule seed: {e}")))?;
        Ok(Self::from_seed(seed))
    }

    /// Read the standing allow rules. Read-only introspection: the agent may see
    /// what a human already granted, which is not the power to grant.
    ///
    /// implements: agent-self-sufficiency
    pub fn allow_rules(&self) -> &[Vec<String>] {
        &self.allow
    }

    /// Read the standing deny rules.
    pub fn deny_rules(&self) -> &[Vec<String>] {
        &self.deny
    }

    /// Resolve an argv to its disposition. **Deny beats allow.**
    pub fn evaluate(&self, argv: &[String]) -> Disposition {
        if self.deny.iter().any(|rule| rule == argv) {
            return Disposition::Deny;
        }
        if self.allow.iter().any(|rule| rule == argv) {
            return Disposition::Allow;
        }
        Disposition::NeedsApproval
    }
}

/// The governance a node applies to jobs started through it: its rule-set, the
/// root every job's cwd must resolve inside, and how long a pending job waits
/// for a human before it is denied.
///
/// implements: rule-set-target-owned-human-mutated
pub struct NodePolicy {
    pub rules: RuleSet,
    /// Every job's cwd must resolve within this root; `..`-escape is an error.
    pub allowed_root: PathBuf,
    /// How long a pending job waits for a decision before failing closed.
    pub approval_timeout: Duration,
}

impl NodePolicy {
    pub fn new(rules: RuleSet, allowed_root: impl Into<PathBuf>) -> Self {
        Self {
            rules,
            allowed_root: allowed_root.into(),
            approval_timeout: Duration::from_secs(300),
        }
    }

    pub fn with_approval_timeout(mut self, timeout: Duration) -> Self {
        self.approval_timeout = timeout;
        self
    }
}

/// A human's decision on a pending job.
///
/// v1 carries the *once* verdicts: they transition this job and write no rule.
/// The "always" verdicts, which append to a node-owned overlay, arrive with the
/// human-gated overlay writer.
///
/// implements: approval-is-a-job-state
/// implements: four-verdicts-exact-argv
#[derive(Debug, Clone)]
pub enum Decision {
    /// Run this instance. `edited_argv` carries a corrected argv — the
    /// structured correction back-channel — and is what actually runs.
    AllowOnce { edited_argv: Option<Vec<String>> },
    /// Reject this instance.
    DenyOnce,
}

impl Decision {
    /// Approve without editing the argv.
    pub fn allow() -> Self {
        Decision::AllowOnce { edited_argv: None }
    }

    /// Approve, replacing the proposed argv with a corrected one.
    pub fn allow_edited<I, S>(argv: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Decision::AllowOnce {
            edited_argv: Some(argv.into_iter().map(Into::into).collect()),
        }
    }
}
