//! The standing overlay: the rules the *always* verdicts write.
//!
//! The seed is the committed, reviewed half of a node's rule-set; this is the
//! daemon-owned half, grown only by a human's *allow always* or *deny always*.
//! It is node state on disk — a file in the registry's directory, in the
//! seed's own shape — so a restarted daemon applies what was granted before.
//!
//! implements: rule-set-target-owned-human-mutated
//! implements: four-verdicts-exact-argv
//! implements: registry-is-node-state-on-disk

use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::audit::{escape_field, unescape_field};
use crate::error::JobError;
use crate::rules::{RuleSet, SeedConfig};

/// The overlay's file name inside the registry directory.
pub const STANDING_FILE: &str = "standing-rules.styx";

/// The overlay and where it lives.
pub(crate) struct Standing {
    rules: RuleSet,
    path: PathBuf,
}

impl Standing {
    /// Read the overlay in `dir`; none written yet is an empty overlay. A file
    /// that cannot be read or decoded is an error: rules that silently vanish
    /// would turn every standing deny into a question again.
    pub(crate) fn load(dir: &Path) -> Result<Self, JobError> {
        let path = dir.join(STANDING_FILE);
        let rules = match std::fs::read_to_string(&path) {
            Ok(text) => {
                let seed: SeedConfig = facet_styx::from_str(&text).map_err(|e| {
                    JobError::Seed(format!(
                        "standing rules '{}' could not be decoded: {e}",
                        path.display()
                    ))
                })?;
                RuleSet::from_seed(unescaped(seed))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => RuleSet::default(),
            Err(e) => return Err(JobError::Io(e)),
        };
        Ok(Self { rules, path })
    }

    pub(crate) fn rules(&self) -> &RuleSet {
        &self.rules
    }

    /// Append an exact-argv rule (allow or deny) and write the overlay. The
    /// write comes first: on failure nothing changes, in memory or on disk.
    pub(crate) fn add(&mut self, allow: bool, argv: Vec<String>) -> Result<(), JobError> {
        let next = self.rules.with_rule(allow, argv);
        write_atomically(&self.path, &next)?;
        self.rules = next;
        Ok(())
    }
}

fn escaped(rules: &RuleSet) -> SeedConfig {
    let esc = |list: &[Vec<String>]| -> Vec<Vec<String>> {
        list.iter()
            .map(|argv| argv.iter().map(|a| escape_field(a)).collect())
            .collect()
    };
    SeedConfig {
        allow: esc(rules.allow_rules()),
        deny: esc(rules.deny_rules()),
    }
}

fn unescaped(seed: SeedConfig) -> SeedConfig {
    let unesc = |list: Vec<Vec<String>>| -> Vec<Vec<String>> {
        list.into_iter()
            .map(|argv| argv.iter().map(|a| unescape_field(a)).collect())
            .collect()
    };
    SeedConfig {
        allow: unesc(seed.allow),
        deny: unesc(seed.deny),
    }
}

/// A full owner-only temp file, then a rename over the overlay: a crash
/// mid-write leaves the previous rules intact, and no other user can read or
/// plant one.
fn write_atomically(path: &Path, rules: &RuleSet) -> Result<(), JobError> {
    use std::os::unix::fs::OpenOptionsExt;

    let text = facet_styx::to_string(&escaped(rules))
        .map_err(|e| JobError::Seed(format!("failed to encode standing rules: {e}")))?;
    let tmp = path.with_extension(format!("{}.tmp", std::process::id()));
    let _ = std::fs::remove_file(&tmp);
    let mut file = File::options()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(&tmp)
        .map_err(JobError::Io)?;
    file.write_all(text.as_bytes()).map_err(JobError::Io)?;
    file.sync_all().map_err(JobError::Io)?;
    std::fs::rename(&tmp, path).map_err(JobError::Io)
}
