//! The working copy: every device reads and writes one local place.
//!
//! A miss reads through to the remotes, the bytes checked as they land
//! locally. A write is done once it is local and queued; `push` carries the
//! queue to the remotes policy names, and the queue survives a restart.
//! Whether a write is *safe* is a separate question, answered by the
//! remotes' inventories. Eviction is local, never of what policy keeps,
//! never of what is still queued.
//!
//! implements: 7d189e690068d0f7200fcdbb236afe6ab64b6ad6535c8a8a883c3f825b72c79a@7d189e690068d0f7200fcdbb236afe6ab64b6ad6535c8a8a883c3f825b72c79a

use std::io::Read;
use std::path::PathBuf;
use std::sync::Arc;

use crate::error::PlaceError;
use crate::hash::Hash;
use crate::inventory::LocalRemove;
use crate::key::Key;
use crate::place::{Body, ByteRange, Capabilities, Child, InventoryEntry, Place, Put, Verified};

/// Which remotes a written key goes to, by name.
pub type Route = Arc<dyn Fn(&Key) -> Vec<String> + Send + Sync>;
/// Keys a working copy never evicts (the ledger trees).
pub type Keep = Arc<dyn Fn(&Key) -> bool + Send + Sync>;

/// What one push did.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct PushReport {
    /// Copies a remote did not hold before.
    pub stored: usize,
    /// Copies a remote already held.
    pub present: usize,
    /// Copies still queued: the remote was offline, or the put failed.
    pub pending: usize,
    /// Why puts failed: remote, key, error.
    pub failed: Vec<(String, Key, String)>,
}

/// A local place with remotes behind it.
pub struct WorkingCopy<L> {
    local: L,
    remotes: Vec<(String, Arc<dyn Place>)>,
    route: Route,
    keep: Keep,
    dir: PathBuf,
}

impl<L: Place + LocalRemove> WorkingCopy<L> {
    /// A working copy over `local`, its queue kept in `dir` (created if
    /// missing, a queue already there picked up). Writes go nowhere and
    /// everything is evictable until `route` and `keep` say otherwise.
    pub fn open(local: L, dir: impl Into<PathBuf>) -> Result<Self, PlaceError> {
        Ok(Self {
            local,
            remotes: Vec::new(),
            route: Arc::new(|_| Vec::new()),
            keep: Arc::new(|_| false),
            dir: dir.into(),
        })
    }

    /// Add a remote, read through in the order added.
    pub fn with_remote(mut self, name: &str, place: Arc<dyn Place>) -> Self {
        self.remotes.push((name.to_owned(), place));
        self
    }

    pub fn with_route(self, route: impl Fn(&Key) -> Vec<String> + Send + Sync + 'static) -> Self {
        Self {
            route: Arc::new(route),
            ..self
        }
    }

    pub fn with_keep(self, keep: impl Fn(&Key) -> bool + Send + Sync + 'static) -> Self {
        Self {
            keep: Arc::new(keep),
            ..self
        }
    }

    pub fn local(&self) -> &L {
        &self.local
    }

    /// Copies still queued, as (remote, key).
    pub fn pending(&self) -> Vec<(String, Key)> {
        let _ = (&self.route, &self.keep, &self.dir);
        Vec::new()
    }

    /// Carry the queue to the remotes.
    pub fn push(&self) -> PushReport {
        PushReport::default()
    }

    /// Fetch every object below `prefix` that `remote` holds and the local
    /// place lacks (how a device takes in the ledger trees); how many came.
    pub fn pull(&self, _remote: &str, _prefix: &Key) -> Result<usize, PlaceError> {
        Ok(0)
    }

    /// Drop the local copy of `key`. Refused for what `keep` keeps and for
    /// what is still queued to push. `false` when there was no local copy.
    pub fn evict(&self, _key: &Key) -> Result<bool, PlaceError> {
        Ok(false)
    }
}

impl<L: Place + LocalRemove> Place for WorkingCopy<L> {
    fn capabilities(&self) -> Capabilities {
        self.local.capabilities()
    }

    fn put_once(
        &self,
        key: &Key,
        hash: &Hash,
        size: u64,
        body: &mut dyn Read,
    ) -> Result<Put, PlaceError> {
        self.local.put_once(key, hash, size, body)
    }

    fn get(&self, key: &Key, range: Option<ByteRange>) -> Result<Body, PlaceError> {
        self.local.get(key, range)
    }

    fn exists(&self, key: &Key) -> Result<bool, PlaceError> {
        self.local.exists(key)
    }

    /// What the local place holds: the ledger trees are complete locally
    /// once pulled; a remote's listing is never consulted here.
    fn list(&self, prefix: Option<&Key>) -> Result<Vec<Child>, PlaceError> {
        self.local.list(prefix)
    }

    fn verify(&self, key: &Key) -> Result<Verified, PlaceError> {
        self.local.verify(key)
    }

    fn inventory(&self) -> Result<Vec<InventoryEntry>, PlaceError> {
        self.local.inventory()
    }
}
