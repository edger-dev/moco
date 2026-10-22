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
///
/// Two daemons may share a registry directory, so the file — not this copy —
/// is the truth: a write locks the directory's lock file, re-reads, merges
/// and writes; a read reloads when the file changed underneath.
pub(crate) struct Standing {
    rules: RuleSet,
    path: PathBuf,
    /// What the file looked like when `rules` was read from it.
    seen: Option<Stamp>,
}

/// Enough of a file's metadata to tell that it was replaced.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Stamp {
    ino: u64,
    len: u64,
    modified: Option<std::time::SystemTime>,
}

fn stamp(path: &Path) -> Option<Stamp> {
    use std::os::unix::fs::MetadataExt;
    let meta = std::fs::metadata(path).ok()?;
    Some(Stamp {
        ino: meta.ino(),
        len: meta.len(),
        modified: meta.modified().ok(),
    })
}

/// Read the rules at `path`; none written yet is no rules. A file that cannot
/// be read or decoded is an error: rules that silently vanish would turn
/// every standing deny into a question again.
fn read_rules(path: &Path) -> Result<RuleSet, JobError> {
    match std::fs::read_to_string(path) {
        Ok(text) => {
            let seed: SeedConfig = facet_styx::from_str(&text).map_err(|e| {
                JobError::Seed(format!(
                    "standing rules '{}' could not be decoded: {e}",
                    path.display()
                ))
            })?;
            Ok(RuleSet::from_seed(unescaped(seed)))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(RuleSet::default()),
        Err(e) => Err(JobError::Io(e)),
    }
}

impl Standing {
    /// Read the overlay in `dir`.
    pub(crate) fn load(dir: &Path) -> Result<Self, JobError> {
        let path = dir.join(STANDING_FILE);
        let seen = stamp(&path);
        let rules = read_rules(&path)?;
        Ok(Self { rules, path, seen })
    }

    /// The current rules, re-read if another writer replaced the file. A file
    /// replaced by something unreadable keeps the last rules read: every
    /// write is a whole-file rename, so that is a hand edit gone wrong, and
    /// the next write — which reads strictly — reports it.
    pub(crate) fn rules(&mut self) -> &RuleSet {
        let now = stamp(&self.path);
        if now != self.seen
            && let Ok(rules) = read_rules(&self.path)
        {
            self.rules = rules;
            self.seen = now;
        }
        &self.rules
    }

    /// Append an exact-argv rule (allow or deny) and write the overlay, under
    /// the directory's lock and merged with whatever is on disk, so a rule
    /// another daemon wrote is never erased. On failure nothing changes.
    pub(crate) fn add(&mut self, allow: bool, argv: Vec<String>) -> Result<(), JobError> {
        use std::os::unix::fs::OpenOptionsExt;

        let lock = File::options()
            .create(true)
            .truncate(false)
            .write(true)
            .mode(0o600)
            .open(self.path.with_extension("lock"))
            .map_err(JobError::Io)?;
        lock.lock().map_err(JobError::Io)?;
        let next = read_rules(&self.path)?.with_rule(allow, argv);
        write_atomically(&self.path, &next)?;
        self.seen = stamp(&self.path);
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

static TMP_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// A full owner-only temp file, then a rename over the overlay, then the
/// directory synced: a crash mid-write leaves the previous rules intact, a
/// crash after it keeps the new ones, and no other user can read or plant one.
fn write_atomically(path: &Path, rules: &RuleSet) -> Result<(), JobError> {
    use std::os::unix::fs::OpenOptionsExt;

    let text = facet_styx::to_string(&escaped(rules))
        .map_err(|e| JobError::Seed(format!("failed to encode standing rules: {e}")))?;
    let tmp = path.with_extension(format!(
        "{}.{}.tmp",
        std::process::id(),
        TMP_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    let written = (|| -> std::io::Result<()> {
        let mut file = File::options()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(&tmp)?;
        file.write_all(text.as_bytes())?;
        file.sync_all()?;
        std::fs::rename(&tmp, path)
    })();
    if let Err(e) = written {
        let _ = std::fs::remove_file(&tmp);
        return Err(JobError::Io(e));
    }
    if let Some(dir) = path.parent() {
        File::open(dir)
            .and_then(|d| d.sync_all())
            .map_err(JobError::Io)?;
    }
    Ok(())
}
