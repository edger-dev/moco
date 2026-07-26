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

use crate::admission::WorktreePolicy;
use crate::error::JobError;
use crate::lens::HumanView;
use crate::lifecycle::{Autostart, Lifetime, RestartPolicy};
use crate::port::{self, PortRequest};

/// The manifest's filename, at the workspace root.
///
/// Visible rather than hidden: a declaration meant to be reviewed in version
/// control should be somewhere a reviewer trips over it.
pub const MANIFEST_FILE: &str = "moco-processes.styx";

/// One declared job.
///
/// Only `name` and `argv` are required. Everything else defaults to the
/// unsurprising thing — a one-shot, started manually, never restarted, run at
/// the workspace root with no deadline — so a job that wants no automatic
/// behaviour of any kind says nothing about it.
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
    #[facet(default)]
    pub cwd: String,
    /// Execution deadline in milliseconds; 0 means unbounded.
    #[facet(default)]
    pub deadline_ms: u64,
    /// Whether this is expected to end or to keep running.
    #[facet(default = Lifetime::OneShot)]
    pub lifetime: Lifetime,
    /// What happens when it exits. Meaningful only for a service.
    #[facet(default = RestartPolicy::Never)]
    pub restart: RestartPolicy,
    /// When it is *first* started, and by whom.
    #[facet(default = Autostart::Manual)]
    pub autostart: Autostart,
    /// Whether this job wants a port, and which.
    #[facet(default = PortRequest::None)]
    pub port: PortRequest,
    /// The environment variable the port arrives in. The argv token stays
    /// `@MOCO_PORT` regardless — only `@MOCO_*` names are node-supplied, so the
    /// token namespace is fixed even when the variable is renamed for the
    /// program's benefit.
    #[facet(default)]
    pub port_env: String,
    /// Where in the repo this may run. Unstated resolves from the port.
    #[facet(default = WorktreePolicy::Unset)]
    pub worktree: WorktreePolicy,
    /// Node names allowed to run it. Empty means anywhere, which is how one
    /// checked-in manifest stays usable on every machine.
    #[facet(default)]
    pub hosts: Vec<String>,
    /// Whether a human watches this through a pty or as a log stream.
    #[facet(default = HumanView::Logs)]
    pub human_view: HumanView,
    /// A sidecar this job writes, relative to its working directory, holding a
    /// machine-readable view of what it found. Empty means none, and a reader
    /// falls back to scrollback.
    #[facet(default)]
    pub machine_file: String,
    /// What is in that file, so a reader knows how to read it. A label, not a
    /// parser: the engine never interprets it, which is what keeps "declared,
    /// never inferred" from turning into a format zoo in here.
    #[facet(default)]
    pub machine_format: String,
}

/// Everything one workspace declares.
///
/// The file is Styx, the same format the node's rule seed uses:
///
/// ```text
/// proc (
///   {name build, argv (cargo build)}
///   {name check, argv (cargo check),
///    lifetime @Service, restart @OnFailure, autostart @Session}
/// )
/// ```
///
/// `cwd` is relative to the workspace root, and empty means the root itself.
///
/// **Quote any argv element containing `@`.** In Styx `@` introduces an enum
/// variant, so a bare `@MOCO_PORT` or `@notes.txt` is a parse error — this is a
/// fact about the file format, separate from what the token means. Write
/// `argv (serve --port "@MOCO_PORT")`.
///
/// Quote an argv element that would otherwise read as something else: bare
/// `true` and `false` parse as booleans, so the `/bin/true` command is
/// `argv ("true")`. The parser refuses the mismatch rather than coercing, so
/// this surfaces as a load error naming the file — not as a job that runs
/// something unexpected.
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
        manifest.check_tokens(&path)?;
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

    /// Refuse an argv containing something that will not expand.
    ///
    /// At **load**, so a typo is a config error naming the file rather than a
    /// literal `@MOCO_PROT` arriving at the program as an argument.
    fn check_tokens(&self, path: &Path) -> Result<(), JobError> {
        for entry in &self.proc {
            port::validate_tokens(&entry.argv).map_err(|detail| JobError::Manifest {
                path: path.display().to_string(),
                detail: format!("in '{}': {detail}", entry.name),
            })?;
        }
        Ok(())
    }

    /// The environment variable this entry's port should arrive in.
    pub fn port_env_of(entry: &ProcEntry) -> &str {
        if entry.port_env.is_empty() {
            port::DEFAULT_PORT_ENV
        } else {
            &entry.port_env
        }
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
                lifetime: Lifetime::OneShot,
                restart: RestartPolicy::Never,
                autostart: Autostart::Manual,
                port: PortRequest::None,
                port_env: String::new(),
                worktree: WorktreePolicy::Unset,
                hosts: Vec::new(),
                human_view: HumanView::Logs,
                machine_file: String::new(),
                machine_format: String::new(),
            }],
        };
        let text = facet_styx::to_string(&manifest).expect("encode");
        let back: Manifest = facet_styx::from_str(&text).expect("decode");
        assert_eq!(manifest, back);
    }
}
