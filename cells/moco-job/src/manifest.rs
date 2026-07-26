//! What a workspace declares.
//!
//! The manifest is where a repo writes down the jobs it expects to run, so they
//! are reproducible and an agent does not have to re-describe them every time.
//!
//! **It declares; it does not authorize.** The file lives in a checkout the
//! agent can edit, so "committed to VCS" describes where it sits, not that
//! anyone reviewed it. Whether a declared argv may actually run is the node's
//! question, answered by the node's rule-set on exactly the same path an ad-hoc
//! request takes.
//!
//! implements: manifest-declares-node-authorizes
//! implements: config-failure-never-degrades-to-empty

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use facet::Facet;

use crate::error::JobError;

/// The manifest's filename, at the workspace root.
///
/// Visible rather than hidden: a declaration meant to be reviewed in version
/// control should be somewhere a reviewer trips over it.
pub const MANIFEST_FILE: &str = "moco-processes.styx";

/// One declared job.
#[derive(Facet, Debug, Clone, PartialEq, Eq)]
pub struct ProcEntry {
    /// Unique within this workspace. The fully-qualified id is
    /// `workspace:name`, so a second checkout may declare its own `check`.
    pub name: String,
    /// What to run, as an **argument vector**. Never a shell string: shell
    /// metacharacters stay inert data, and an exact-argv rule-set has something
    /// it can soundly match.
    pub argv: Vec<String>,
    /// Where to run it, relative to the workspace root. Empty means the root
    /// itself, which is what almost every declared job wants.
    pub cwd: String,
    /// Execution deadline in milliseconds; 0 means unbounded.
    pub deadline_ms: u64,
}

/// Everything one workspace declares.
///
/// The file is Styx, the same format the node's rule seed uses:
///
/// ```text
/// proc (
///   {name check,      argv (cargo check),        cwd "",    deadline_ms 0}
///   {name unit-tests,  argv (cargo test --lib),  cwd "",    deadline_ms 600000}
/// )
/// ```
///
/// `cwd` is relative to the workspace root, and empty means the root itself.
#[derive(Facet, Debug, Clone, Default, PartialEq, Eq)]
pub struct Manifest {
    pub proc: Vec<ProcEntry>,
}

impl Manifest {
    /// The manifest file for a workspace root.
    pub fn path_in(root: impl AsRef<Path>) -> PathBuf {
        root.as_ref().join(MANIFEST_FILE)
    }

    /// Read a workspace's manifest.
    ///
    /// **Absent and broken are different answers.** No file means nothing is
    /// declared, which is correct and silent. A file that cannot be parsed is an
    /// error naming the file — never an empty manifest, because that reports a
    /// missing *entry* for a *file-level* problem and sends the reader hunting
    /// for a typo in a name when the real fault is one bad field elsewhere.
    pub fn load(root: impl AsRef<Path>) -> Result<Self, JobError> {
        let path = Self::path_in(&root);
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(e) => {
                return Err(JobError::Manifest {
                    path: path.display().to_string(),
                    detail: e.to_string(),
                });
            }
        };

        let manifest: Manifest = facet_styx::from_str(&text).map_err(|e| JobError::Manifest {
            path: path.display().to_string(),
            detail: e.to_string(),
        })?;
        manifest.check_unique_names(&path)?;
        Ok(manifest)
    }

    /// Two entries with one name is a config error, not last-one-wins: silently
    /// picking one would make the file say something it does not do.
    fn check_unique_names(&self, path: &Path) -> Result<(), JobError> {
        let mut seen = BTreeSet::new();
        for entry in &self.proc {
            if !seen.insert(entry.name.as_str()) {
                return Err(JobError::Manifest {
                    path: path.display().to_string(),
                    detail: format!(
                        "'{}' is declared more than once; names are unique within a workspace",
                        entry.name
                    ),
                });
            }
        }
        Ok(())
    }

    pub fn is_empty(&self) -> bool {
        self.proc.is_empty()
    }

    /// The entry declared under `name`.
    pub fn get(&self, name: &str) -> Option<&ProcEntry> {
        self.proc.iter().find(|e| e.name == name)
    }

    /// Every declared name, for an error that wants to be helpful.
    pub fn names(&self) -> Vec<&str> {
        self.proc.iter().map(|e| e.name.as_str()).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_entry_round_trips_through_styx() {
        let manifest = Manifest {
            proc: vec![ProcEntry {
                name: "check".into(),
                argv: vec!["cargo".into(), "check".into()],
                cwd: String::new(),
                deadline_ms: 0,
            }],
        };
        let text = facet_styx::to_string(&manifest).expect("encode");
        let back: Manifest = facet_styx::from_str(&text).expect("decode");
        assert_eq!(manifest, back);
    }
}
