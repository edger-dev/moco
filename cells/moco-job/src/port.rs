//! The node hands out dev ports.
//!
//! Because the node already owns the whole job set, it is the natural place to
//! allocate ports — replacing the lockfile-plus-state-file-plus-cooldown shell
//! script every multi-checkout setup grows otherwise.
//!
//! **The reserved set is derived from the shared on-disk registry, never held in
//! memory.** An in-memory set is invisible to a peer daemon sharing the same
//! directory, so two daemons would hand out the same port. Under multiple
//! daemons the registry is the only correct source of truth.
//!
//! The payoff turned out not to be collision avoidance but **visibility**: the
//! port is a first-class field, so "which of my five servers is this?" stops
//! being a question.
//!
//! implements: node-is-the-port-authority
//! implements: node-supplied-argv-tokens

use std::net::TcpListener;
use std::time::{Duration, SystemTime};

use facet::Facet;

use crate::error::JobError;
use crate::record::RecordStore;
use crate::scope::Scope;

/// The environment variable a job's port is delivered in, unless overridden.
///
/// One prefix namespaces everything the supervisor injects. A bare `PORT` is too
/// likely to collide with something the process or its own tooling already sets.
pub const DEFAULT_PORT_ENV: &str = "MOCO_PORT";

/// The argv token replaced with the allocated port.
///
/// Spelled with `@`, not `$`: the sigil names **who expands it**. `$` means a
/// shell did, and there is no shell. It also shrinks the footgun surface to the
/// `@MOCO_` prefix alone, so no other `@` needs escaping.
pub const PORT_TOKEN: &str = "@MOCO_PORT";

/// The prefix reserved for node-supplied tokens.
pub const TOKEN_PREFIX: &str = "@MOCO_";

/// Every token this version understands, for an error that can list them.
pub const KNOWN_TOKENS: &[&str] = &[PORT_TOKEN];

/// Environment variable naming the node's port range, as `low-high`.
pub const RANGE_ENV: &str = "MOCO_PORT_RANGE";

/// What a declaration asks for.
#[derive(Facet, Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(u8)]
pub enum PortRequest {
    /// No port. Most jobs.
    #[default]
    None,
    /// The node picks one, stickily.
    Auto,
    /// This exact port, reserved as declared and excluded from the auto pool.
    Fixed { port: u16 },
}

/// A node's port range — a *node* range, not a per-repo one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PortRange {
    pub low: u16,
    pub high: u16,
}

impl Default for PortRange {
    fn default() -> Self {
        Self::new(10000, 19999)
    }
}

impl PortRange {
    pub fn new(low: u16, high: u16) -> Self {
        Self { low, high }
    }

    /// The range this node uses, from the environment if it says.
    ///
    /// A malformed value falls back to the default rather than failing startup:
    /// a bad range should not cost a daemon.
    pub fn from_env() -> Self {
        let Ok(raw) = std::env::var(RANGE_ENV) else {
            return Self::default();
        };
        let Some((low, high)) = raw.split_once('-') else {
            return Self::default();
        };
        match (low.trim().parse::<u16>(), high.trim().parse::<u16>()) {
            (Ok(low), Ok(high)) if low <= high => Self::new(low, high),
            _ => Self::default(),
        }
    }

    fn contains(&self, port: u16) -> bool {
        (self.low..=self.high).contains(&port)
    }
}

/// Ports currently spoken for, according to the registry.
///
/// Only **live** entries reserve: a terminal job stops holding its port the
/// moment it ends. There is deliberately no cooldown — the old script needed one
/// only because nothing outlived the allocation, and **stickiness, not
/// reservation, is what keeps a port stable across the gap**. Holding a stopped
/// job's port back would starve the range and reintroduce exactly that.
fn reserved(store: &RecordStore) -> Result<Vec<u16>, JobError> {
    Ok(store
        .all()?
        .into_iter()
        .filter(|r| !r.status.is_terminal() && r.port != 0)
        .map(|r| r.port)
        .collect())
}

/// Could this port be bound right now?
///
/// The live probe is what respects ports held by processes the supervisor knows
/// nothing about — the registry can only speak for jobs it started.
fn bindable(port: u16) -> bool {
    TcpListener::bind(("127.0.0.1", port)).is_ok()
}

/// Is `port` free according to both the registry and the machine?
pub fn is_available(store: &RecordStore, port: u16) -> Result<bool, JobError> {
    Ok(!reserved(store)?.contains(&port) && bindable(port))
}

/// The port this declaration held last time, if any.
///
/// Read from every record, terminal ones included: that is the whole point of
/// stickiness — the previous run is over, and its port should still come back.
fn sticky(store: &RecordStore, scope: &Scope, name: &str) -> Result<Option<u16>, JobError> {
    Ok(store
        .all()?
        .into_iter()
        .filter(|r| r.scope == *scope && r.name == name && r.port != 0)
        // Ids lead with a millisecond stamp, so the greatest is the most recent.
        .max_by(|a, b| a.id.cmp(&b.id))
        .map(|r| r.port))
}

/// A node-level lock serializing scan → pick → persist.
///
/// Distinct from the per-job start/stop lock: two concurrent starts, in two
/// different daemons, must not both see the same port as free. A lock file in
/// the shared registry directory is what makes it node-level rather than
/// process-level.
struct AllocationLock {
    path: std::path::PathBuf,
}

impl AllocationLock {
    /// How long before a lock is presumed abandoned. A daemon that dies holding
    /// it must not wedge port allocation for the life of the node.
    const STALE_AFTER: Duration = Duration::from_secs(30);

    fn acquire(store: &RecordStore) -> Result<Self, JobError> {
        let path = store.dir().join(".ports.lock");
        for _ in 0..200 {
            match std::fs::File::options()
                .create_new(true)
                .write(true)
                .open(&path)
            {
                Ok(_) => return Ok(Self { path }),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    if Self::is_stale(&path) {
                        let _ = std::fs::remove_file(&path);
                        continue;
                    }
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(e) => return Err(JobError::Io(e)),
            }
        }
        Err(JobError::Audit(format!(
            "could not acquire the port allocation lock at {}",
            path.display()
        )))
    }

    fn is_stale(path: &std::path::Path) -> bool {
        std::fs::metadata(path)
            .and_then(|m| m.modified())
            .map(|t| {
                SystemTime::now()
                    .duration_since(t)
                    .map(|age| age > Self::STALE_AFTER)
                    .unwrap_or(false)
            })
            .unwrap_or(false)
    }
}

impl Drop for AllocationLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Allocate a port for `(scope, name)` under `request`.
///
/// **Sticky, then lowest-free.** If this exact declaration previously held an
/// auto port, and that port is in range and free — absent from the reserved set
/// *and* passing a live bind probe — it comes back. Otherwise the lowest port
/// passing both checks.
///
/// Called **after** admission, so a job the gate refuses never consumes a port.
pub fn allocate(
    store: &RecordStore,
    range: PortRange,
    scope: &Scope,
    name: Option<&str>,
    request: PortRequest,
) -> Result<Option<u16>, JobError> {
    match request {
        PortRequest::None => Ok(None),
        // Declared, so not negotiable and not drawn from the auto pool.
        PortRequest::Fixed { port } => Ok(Some(port)),
        PortRequest::Auto => {
            let _lock = AllocationLock::acquire(store)?;
            let taken = reserved(store)?;

            if let Some(name) = name
                && let Some(previous) = sticky(store, scope, name)?
                && range.contains(previous)
                && !taken.contains(&previous)
                && bindable(previous)
            {
                return Ok(Some(previous));
            }

            for port in range.low..=range.high {
                if !taken.contains(&port) && bindable(port) {
                    return Ok(Some(port));
                }
            }
            Err(JobError::Audit(format!(
                "no free port in {}-{}",
                range.low, range.high
            )))
        }
    }
}

/// Record that a declaration holds `port`, so a later allocation is sticky.
///
/// The registry normally does this by persisting the job's record; this is the
/// same fact written directly, for callers allocating outside a job.
pub fn remember(store: &RecordStore, scope: &Scope, name: &str, port: u16) -> Result<(), JobError> {
    store.remember_port(scope, name, port)
}

/// Replace node-supplied tokens in one argv element.
///
/// Substitution **cannot change the argument count** — it is textual within a
/// single element, the value is node-generated, and there is no shell to
/// re-split — which is why `--port=@MOCO_PORT` is as safe as a whole-element
/// token.
pub fn substitute(argv: &[String], port: Option<u16>) -> Vec<String> {
    let Some(port) = port else {
        return argv.to_vec();
    };
    let value = port.to_string();
    argv.iter()
        .map(|arg| arg.replace(PORT_TOKEN, &value))
        .collect()
}

/// Check an argv for node-supplied tokens that will not expand.
///
/// Loud, and at load time: a typo like `@MOCO_PROT` would otherwise reach the
/// program as a literal argument and fail far from its cause.
///
/// **`$` is deliberately not policed.** The spec this implements originally
/// called any `$`-sequence a config error, on the reasoning that no shell is
/// involved. Implementing it showed that wrong twice over. A job may ship a
/// shell as `argv[0]` — the escape hatch `argv-not-shell` explicitly grants for
/// a pipeline — and there `$` is the shell's business. Worse, `sh -c "… \$MOCO_PORT"`
/// is the *correct* way for such a job to read the port, because the node
/// injects it into the environment under exactly that name. A rule that rejects
/// the intended usage is worse than no rule, so only the `@MOCO_` namespace —
/// which is unambiguously ours — is checked.
pub fn validate_tokens(argv: &[String]) -> Result<(), String> {
    for arg in argv {
        if let Some(at) = arg.find(TOKEN_PREFIX) {
            let rest = &arg[at..];
            let end = rest
                .char_indices()
                .find(|(_, c)| !(c.is_ascii_alphanumeric() || *c == '_' || *c == '@'))
                .map(|(i, _)| i)
                .unwrap_or(rest.len());
            let token = &rest[..end];
            if !KNOWN_TOKENS.contains(&token) {
                return Err(format!(
                    "'{token}' is not a node-supplied value. Known tokens: {}",
                    KNOWN_TOKENS.join(", ")
                ));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn substitution_preserves_the_argument_count() {
        let argv = vec![
            "serve".to_string(),
            "--port=@MOCO_PORT".to_string(),
            "@MOCO_PORT".to_string(),
        ];
        let out = substitute(&argv, Some(1234));
        assert_eq!(out.len(), argv.len());
        assert_eq!(out[1], "--port=1234");
        assert_eq!(out[2], "1234");
    }

    #[test]
    fn an_ordinary_at_sign_is_not_a_token() {
        assert!(validate_tokens(&["user@host".to_string()]).is_ok());
        assert!(validate_tokens(&["@notes.txt".to_string()]).is_ok());
    }

    #[test]
    fn a_shell_wrapper_may_use_dollar_variables() {
        // `sh -c` is the sanctioned escape hatch; there the shell expands, and
        // refusing `$` would break it.
        assert!(
            validate_tokens(&["sh".to_string(), "-c".to_string(), "echo $HOME".to_string()])
                .is_ok()
        );
    }

    #[test]
    fn a_node_value_read_through_a_shell_is_allowed() {
        // The env var really is named MOCO_PORT, so this is correct usage, not
        // a mistake to catch.
        assert!(validate_tokens(&["sh".into(), "-c".into(), "echo $MOCO_PORT".into()]).is_ok());
    }

    #[test]
    fn an_unknown_moco_token_is_refused() {
        let err = validate_tokens(&["@MOCO_PROT".to_string()]).expect_err("typo");
        assert!(err.contains("@MOCO_PROT") && err.contains(PORT_TOKEN));
    }

    #[test]
    fn a_range_from_a_malformed_env_falls_back() {
        assert_eq!(PortRange::new(1, 2).low, 1);
        assert_eq!(PortRange::default(), PortRange::new(10000, 19999));
    }
}
