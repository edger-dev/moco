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

use std::collections::BTreeSet;
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};

use crate::error::PlaceError;
use crate::hash::Hash;
use crate::inventory::LocalRemove;
use crate::key::Key;
use crate::place::{Body, ByteRange, Capabilities, Child, InventoryEntry, Place, Put, Verified};

const QUEUE: &str = "queue";

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
    /// Copies still to push: (remote, key).
    queue: Mutex<BTreeSet<(String, Key)>>,
}

impl<L: Place + LocalRemove> WorkingCopy<L> {
    /// A working copy over `local`, its queue kept in `dir` (created if
    /// missing, a queue already there picked up). Writes go nowhere and
    /// everything is evictable until `route` and `keep` say otherwise.
    pub fn open(local: L, dir: impl Into<PathBuf>) -> Result<Self, PlaceError> {
        let dir = dir.into();
        fs::create_dir_all(&dir)?;
        let mut queue = BTreeSet::new();
        let mut settled = false;
        match fs::read_to_string(dir.join(QUEUE)) {
            Ok(text) => {
                for line in text.lines() {
                    let mut fields = line.splitn(3, ' ');
                    let (Some(op), Some(remote), Some(key)) =
                        (fields.next(), fields.next(), fields.next())
                    else {
                        continue;
                    };
                    let Ok(key) = Key::new(key) else { continue };
                    match op {
                        "+" => {
                            queue.insert((remote.to_owned(), key));
                        }
                        "-" => {
                            queue.remove(&(remote.to_owned(), key));
                            settled = true;
                        }
                        _ => {}
                    }
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
        let copy = Self {
            local,
            remotes: Vec::new(),
            route: Arc::new(|_| Vec::new()),
            keep: Arc::new(|_| false),
            dir,
            queue: Mutex::new(queue),
        };
        // Settled copies need not be replayed forever.
        if settled {
            let queue = copy.queue();
            copy.rewrite(&queue)?;
        }
        Ok(copy)
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

    fn queue(&self) -> MutexGuard<'_, BTreeSet<(String, Key)>> {
        // Every change reaches the journal before the set.
        self.queue
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn journal(&self, lines: &[String]) -> Result<(), PlaceError> {
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.dir.join(QUEUE))?;
        let text: String = lines.iter().map(|l| format!("{l}\n")).collect();
        file.write_all(text.as_bytes())?;
        file.sync_data()?;
        Ok(())
    }

    fn rewrite(&self, queue: &BTreeSet<(String, Key)>) -> Result<(), PlaceError> {
        let text: String = queue
            .iter()
            .map(|(remote, key)| format!("+ {remote} {key}\n"))
            .collect();
        let temp = self.dir.join(format!("{QUEUE}.tmp"));
        fs::write(&temp, text)?;
        fs::rename(&temp, self.dir.join(QUEUE))?;
        Ok(())
    }

    fn remote(&self, name: &str) -> Option<&Arc<dyn Place>> {
        self.remotes.iter().find(|(n, _)| n == name).map(|(_, p)| p)
    }

    /// Copies still queued, as (remote, key).
    pub fn pending(&self) -> Vec<(String, Key)> {
        self.queue().iter().cloned().collect()
    }

    /// Carry the queue to the remotes.
    pub fn push(&self) -> PushReport {
        let mut report = PushReport::default();
        for (name, key) in self.pending() {
            let Some(remote) = self.remote(&name) else {
                report.pending += 1;
                report
                    .failed
                    .push((name, key, "no remote by this name".to_owned()));
                continue;
            };
            if !remote.capabilities().online {
                report.pending += 1;
                continue;
            }
            let sent = key
                .named_hash()
                .ok_or(PlaceError::Refused {
                    key: key.clone(),
                    reason: "its key names no hash",
                })
                .and_then(|hash| {
                    let mut body = self.local.get(&key, None)?;
                    let len = body.len();
                    remote.put_once(&key, &hash, len, &mut body)
                });
            match sent {
                Ok(put) => {
                    let done = format!("- {name} {key}");
                    if let Err(e) = self.journal(&[done]) {
                        report.pending += 1;
                        report.failed.push((name, key, e.to_string()));
                        continue;
                    }
                    self.queue().remove(&(name, key));
                    match put {
                        Put::Stored => report.stored += 1,
                        Put::Present => report.present += 1,
                    }
                }
                Err(PlaceError::Offline) => report.pending += 1,
                Err(e) => {
                    report.pending += 1;
                    report.failed.push((name, key, e.to_string()));
                }
            }
        }
        report
    }

    /// Fetch every object below `prefix` that `remote` holds and the local
    /// place lacks (how a device takes in the ledger trees); how many came.
    pub fn pull(&self, remote: &str, prefix: &Key) -> Result<usize, PlaceError> {
        let place = self
            .remote(remote)
            .ok_or_else(|| PlaceError::UnknownRemote(remote.to_owned()))?;
        let mut fetched = 0;
        let mut prefixes = vec![prefix.clone()];
        while let Some(prefix) = prefixes.pop() {
            for child in place.list(Some(&prefix))? {
                match child {
                    Child::Prefix(key) => prefixes.push(key),
                    Child::Object { key, .. } => {
                        let Some(hash) = key.named_hash() else {
                            continue;
                        };
                        if self.local.exists(&key)? {
                            continue;
                        }
                        let mut body = place.get(&key, None)?;
                        let len = body.len();
                        if self.local.put_once(&key, &hash, len, &mut body)? == Put::Stored {
                            fetched += 1;
                        }
                    }
                }
            }
        }
        Ok(fetched)
    }

    /// Drop the local copy of `key`. Refused for what `keep` keeps and for
    /// what is still queued to push. `false` when there was no local copy.
    pub fn evict(&self, key: &Key) -> Result<bool, PlaceError> {
        if (self.keep)(key) {
            return Err(PlaceError::Refused {
                key: key.clone(),
                reason: "policy keeps it on this device",
            });
        }
        let queue = self.queue();
        if queue.iter().any(|(_, k)| k == key) {
            return Err(PlaceError::Refused {
                key: key.clone(),
                reason: "it is still queued to push; this may be its only copy",
            });
        }
        // Held while removing, so no put can queue it in between.
        let removed = self.local.remove(key);
        drop(queue);
        removed
    }

    /// Read `key` through from the first remote that holds an intact copy,
    /// landing it locally; whether one did.
    fn read_through(&self, key: &Key) -> Result<bool, PlaceError> {
        let Some(hash) = key.named_hash() else {
            return Ok(false);
        };
        for (_, remote) in &self.remotes {
            let caps = remote.capabilities();
            if !caps.online || caps.cold {
                continue;
            }
            let mut body = match remote.get(key, None) {
                Ok(body) => body,
                Err(_) => continue,
            };
            let len = body.len();
            match self.local.put_once(key, &hash, len, &mut body) {
                Ok(_) => return Ok(true),
                // A rotten or short copy: try the next remote.
                Err(PlaceError::ContentMismatch { .. } | PlaceError::SizeMismatch { .. }) => {
                    continue;
                }
                // A read error mid-stream is the remote's; a local failure
                // is not worth hiding.
                Err(PlaceError::Io(e)) if e.kind() != std::io::ErrorKind::StorageFull => {
                    continue;
                }
                Err(e) => return Err(e),
            }
        }
        Ok(false)
    }
}

impl<L: Place + LocalRemove> Place for WorkingCopy<L> {
    fn capabilities(&self) -> Capabilities {
        self.local.capabilities()
    }

    /// Done once local; queued for every remote policy routes it to.
    fn put_once(
        &self,
        key: &Key,
        hash: &Hash,
        size: u64,
        body: &mut dyn Read,
    ) -> Result<Put, PlaceError> {
        let put = self.local.put_once(key, hash, size, body)?;
        let mut queue = self.queue();
        let new: Vec<(String, Key)> = (self.route)(key)
            .into_iter()
            .filter(|name| self.remote(name).is_some())
            .map(|name| (name, key.clone()))
            .filter(|entry| !queue.contains(entry))
            .collect();
        if !new.is_empty() {
            let lines: Vec<String> = new.iter().map(|(r, k)| format!("+ {r} {k}")).collect();
            self.journal(&lines)?;
            queue.extend(new);
        }
        Ok(put)
    }

    /// Local, or read through from the first remote with an intact copy.
    fn get(&self, key: &Key, range: Option<ByteRange>) -> Result<Body, PlaceError> {
        match self.local.get(key, range) {
            Err(PlaceError::NotFound(_)) if self.read_through(key)? => self.local.get(key, range),
            other => other,
        }
    }

    /// Held locally, or by a remote that can be reached now.
    fn exists(&self, key: &Key) -> Result<bool, PlaceError> {
        if self.local.exists(key)? {
            return Ok(true);
        }
        Ok(self.remotes.iter().any(|(_, remote)| {
            let caps = remote.capabilities();
            caps.online && !caps.cold && remote.exists(key).unwrap_or(false)
        }))
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
