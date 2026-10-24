//! The working copy: every device reads and writes one local place.
//!
//! A miss reads through to the remotes, the bytes checked as they land
//! locally. A write is done once it is local and queued; `push` carries the
//! queue to the remotes policy names, and the queue survives a restart.
//! Whether a write is *safe* is a separate question, answered by the
//! remotes' inventories. Eviction is local, never of what policy keeps,
//! never of what is still queued or being written.
//!
//! The queue directory is locked while open (a file named `lock`), so give
//! the queue and an inventory directories of their own.
//!
//! implements: 7d189e690068d0f7200fcdbb236afe6ab64b6ad6535c8a8a883c3f825b72c79a@7d189e690068d0f7200fcdbb236afe6ab64b6ad6535c8a8a883c3f825b72c79a

use std::collections::{BTreeSet, HashMap, HashSet};
use std::io::{self, Read};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use crate::error::PlaceError;
use crate::hash::Hash;
use crate::inventory::LocalRemove;
use crate::journal::{Journal, lock_dir};
use crate::key::Key;
use crate::place::{Body, ByteRange, Capabilities, Child, InventoryEntry, Place, Put, Verified};

const QUEUE: &str = "queue";
/// Settled lines beyond this many (and beyond the live ones) compact the
/// queue journal.
const COMPACT_AFTER: usize = 1024;

/// Which remotes a written key goes to, by name. Called with no lock held.
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
    /// Queued copies whose local object is gone: dropped from the queue,
    /// reported so whoever removed it hears of it.
    pub lost: Vec<(String, Key)>,
}

/// What one pull did.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct PullReport {
    /// Objects taken in.
    pub fetched: usize,
    /// Objects that could not be: key, error. The pull carried on.
    pub failed: Vec<(Key, String)>,
    /// Objects whose key names no hash, so they cannot travel this way.
    pub unnamed: Vec<Key>,
}

/// The queue: copies to push, and keys being written now.
struct Queue {
    pending: BTreeSet<(String, Key)>,
    writing: HashMap<Key, usize>,
    /// The journal; `None` for a working copy kept in memory.
    journal: Option<Journal>,
    settled: usize,
}

/// A local place with remotes behind it.
pub struct WorkingCopy<L> {
    local: L,
    remotes: Vec<(String, Arc<dyn Place>)>,
    route: Route,
    keep: Keep,
    queue: Mutex<Queue>,
    /// Held for as long as this working copy lives: one keeper per queue.
    _lock: Option<std::fs::File>,
}

impl<L: Place + LocalRemove> WorkingCopy<L> {
    /// A working copy over `local`, its queue kept in `dir` (created if
    /// missing, a queue already there picked up, a torn tail cut off).
    /// Refused while another keeper holds `dir`. Writes go nowhere and
    /// everything is evictable until `route` and `keep` say otherwise.
    pub fn open(local: L, dir: impl Into<PathBuf>) -> Result<Self, PlaceError> {
        let dir = dir.into();
        let lock = lock_dir(&dir)?;
        let (journal, lines) = Journal::open(dir.join(QUEUE))?;
        let mut pending = BTreeSet::new();
        let mut settled = 0;
        for line in &lines {
            let mut fields = line.splitn(3, ' ');
            let (Some(op), Some(remote), Some(key)) = (fields.next(), fields.next(), fields.next())
            else {
                continue;
            };
            let Ok(key) = Key::new(key) else { continue };
            match op {
                "+" => {
                    pending.insert((remote.to_owned(), key));
                }
                "-" => {
                    pending.remove(&(remote.to_owned(), key));
                    settled += 1;
                }
                _ => {}
            }
        }
        let copy = Self::with_queue(
            local,
            Queue {
                pending,
                writing: HashMap::new(),
                journal: Some(journal),
                settled,
            },
            Some(lock),
        );
        if settled > 0 {
            let mut queue = copy.queue();
            compact(&mut queue)?;
        }
        Ok(copy)
    }

    /// A working copy whose queue lives in memory only — for a device with
    /// no disk of its own. The same rules; nothing survives a restart.
    pub fn in_memory(local: L) -> Self {
        Self::with_queue(
            local,
            Queue {
                pending: BTreeSet::new(),
                writing: HashMap::new(),
                journal: None,
                settled: 0,
            },
            None,
        )
    }

    fn with_queue(local: L, queue: Queue, lock: Option<std::fs::File>) -> Self {
        Self {
            local,
            remotes: Vec::new(),
            route: Arc::new(|_| Vec::new()),
            keep: Arc::new(|_| false),
            queue: Mutex::new(queue),
            _lock: lock,
        }
    }

    /// Add a remote, read through in the order added. Its name is one word
    /// (it is written in the queue's lines).
    pub fn with_remote(mut self, name: &str, place: Arc<dyn Place>) -> Result<Self, PlaceError> {
        if name.is_empty() || name.chars().any(char::is_whitespace) {
            return Err(PlaceError::UnknownRemote(format!(
                "{name:?}: a remote's name is one word"
            )));
        }
        self.remotes.push((name.to_owned(), place));
        Ok(self)
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

    /// The local place. Removing through it skips eviction's checks; a
    /// queued copy removed so is reported lost by the next push.
    pub fn local(&self) -> &L {
        &self.local
    }

    fn queue(&self) -> MutexGuard<'_, Queue> {
        // Every change reaches the journal before the set.
        self.queue
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn remote(&self, name: &str) -> Option<&Arc<dyn Place>> {
        self.remotes.iter().find(|(n, _)| n == name).map(|(_, p)| p)
    }

    /// Copies still queued, as (remote, key).
    pub fn pending(&self) -> Vec<(String, Key)> {
        self.queue().pending.iter().cloned().collect()
    }

    /// The first remote that holds `key` and can be reached now — a network
    /// call per remote asked. `exists` answers for this device only.
    pub fn locate(&self, key: &Key) -> Option<String> {
        self.remotes
            .iter()
            .find(|(_, remote)| {
                let caps = remote.capabilities();
                caps.online && !caps.cold && remote.exists(key).unwrap_or(false)
            })
            .map(|(name, _)| name.clone())
    }

    /// Drop every queued copy for `remote` — a remote retired from the
    /// configuration. Those writes are then nobody's to carry there.
    pub fn forget_remote(&self, remote: &str) -> Result<usize, PlaceError> {
        let mut queue = self.queue();
        let gone: Vec<(String, Key)> = queue
            .pending
            .iter()
            .filter(|(r, _)| r == remote)
            .cloned()
            .collect();
        settle(&mut queue, &gone)?;
        Ok(gone.len())
    }

    /// Carry the queue to the remotes. A remote found offline is not asked
    /// again in this push.
    pub fn push(&self) -> PushReport {
        let mut report = PushReport::default();
        let mut offline: HashSet<String> = HashSet::new();
        for (name, key) in self.pending() {
            if offline.contains(&name) {
                report.pending += 1;
                continue;
            }
            let Some(remote) = self.remote(&name) else {
                report.pending += 1;
                report
                    .failed
                    .push((name, key, "no remote by this name".to_owned()));
                continue;
            };
            if !remote.capabilities().online {
                offline.insert(name);
                report.pending += 1;
                continue;
            }
            let Some(hash) = key.named_hash() else {
                report.pending += 1;
                report
                    .failed
                    .push((name, key, "its key names no hash".to_owned()));
                continue;
            };
            let mut body = match self.local.get(&key, None) {
                Ok(body) => body,
                Err(PlaceError::NotFound(_)) => {
                    let lost = (name, key);
                    if settle(&mut self.queue(), std::slice::from_ref(&lost)).is_ok() {
                        report.lost.push(lost);
                    } else {
                        report.pending += 1;
                    }
                    continue;
                }
                Err(e) => {
                    report.pending += 1;
                    report.failed.push((name, key, e.to_string()));
                    continue;
                }
            };
            let len = body.len();
            match remote.put_once(&key, &hash, len, &mut body) {
                Ok(put) => {
                    let done = (name, key);
                    if let Err(e) = settle(&mut self.queue(), std::slice::from_ref(&done)) {
                        report.pending += 1;
                        report.failed.push((done.0, done.1, e.to_string()));
                        continue;
                    }
                    match put {
                        Put::Stored => report.stored += 1,
                        Put::Present => report.present += 1,
                    }
                }
                Err(PlaceError::Offline) => {
                    offline.insert(name);
                    report.pending += 1;
                }
                Err(e) => {
                    report.pending += 1;
                    report.failed.push((name, key, e.to_string()));
                }
            }
        }
        report
    }

    /// Fetch every object below `prefix` that `remote` holds and the local
    /// place lacks (how a device takes in the ledger trees). A bad object is
    /// reported and the pull carries on.
    pub fn pull(&self, remote: &str, prefix: &Key) -> Result<PullReport, PlaceError> {
        let place = self
            .remote(remote)
            .ok_or_else(|| PlaceError::UnknownRemote(remote.to_owned()))?;
        let caps = place.capabilities();
        if !caps.online {
            return Err(PlaceError::Offline);
        }
        if caps.cold {
            return Err(PlaceError::Cold);
        }
        let mut report = PullReport::default();
        let mut prefixes = vec![prefix.clone()];
        while let Some(prefix) = prefixes.pop() {
            for child in place.list(Some(&prefix))? {
                match child {
                    Child::Prefix(key) => prefixes.push(key),
                    Child::Object { key, .. } => {
                        let Some(hash) = key.named_hash() else {
                            report.unnamed.push(key);
                            continue;
                        };
                        match self.fetch(place.as_ref(), &key, &hash) {
                            Ok(true) => report.fetched += 1,
                            Ok(false) => {}
                            Err(e) => report.failed.push((key, e.to_string())),
                        }
                    }
                }
            }
        }
        Ok(report)
    }

    /// Take one object in from `place` unless the local place has it;
    /// whether it came.
    fn fetch(&self, place: &dyn Place, key: &Key, hash: &Hash) -> Result<bool, PlaceError> {
        if self.local.exists(key)? {
            return Ok(false);
        }
        let mut body = place.get(key, None)?;
        let len = body.len();
        Ok(self.local.put_once(key, hash, len, &mut body)? == Put::Stored)
    }

    /// Drop the local copy of `key`. Refused for what `keep` keeps, for what
    /// is still queued to push, and for what a put is writing now. `false`
    /// when there was no local copy.
    pub fn evict(&self, key: &Key) -> Result<bool, PlaceError> {
        if (self.keep)(key) {
            return Err(PlaceError::Refused {
                key: key.clone(),
                reason: "policy keeps it on this device",
            });
        }
        let queue = self.queue();
        if queue.writing.contains_key(key) {
            return Err(PlaceError::Refused {
                key: key.clone(),
                reason: "a put is writing it now",
            });
        }
        if queue.pending.iter().any(|(_, k)| k == key) {
            return Err(PlaceError::Refused {
                key: key.clone(),
                reason: "it is still queued to push; this may be its only copy",
            });
        }
        // Held while removing, so no put can start on it in between.
        let removed = self.local.remove(key);
        drop(queue);
        removed
    }

    /// Read `key` through from the first remote that holds an intact copy,
    /// landing it locally; whether one did. A local failure is returned as
    /// itself; a remote's is remembered and returned when no remote served.
    fn read_through(&self, key: &Key) -> Result<bool, PlaceError> {
        let Some(hash) = key.named_hash() else {
            return Ok(false);
        };
        let mut last: Option<PlaceError> = None;
        for (_, remote) in &self.remotes {
            let caps = remote.capabilities();
            if !caps.online || caps.cold {
                continue;
            }
            let body = match remote.get(key, None) {
                Ok(body) => body,
                Err(PlaceError::NotFound(_) | PlaceError::Offline) => continue,
                Err(e) => {
                    last = Some(e);
                    continue;
                }
            };
            let len = body.len();
            let failed = Arc::new(AtomicBool::new(false));
            let mut tagged = Tagged {
                body,
                failed: failed.clone(),
            };
            match self.local.put_once(key, &hash, len, &mut tagged) {
                Ok(_) => return Ok(true),
                // A rotten or short copy, or a stream that broke: the
                // remote's fault; try the next.
                Err(e @ (PlaceError::ContentMismatch { .. } | PlaceError::SizeMismatch { .. })) => {
                    last = Some(e);
                }
                Err(e @ PlaceError::Io(_)) if failed.load(Ordering::Relaxed) => last = Some(e),
                Err(e) => return Err(e),
            }
        }
        match last {
            Some(e) => Err(e),
            None => Ok(false),
        }
    }
}

/// A remote's body, noting whether a read from it failed — so a put's I/O
/// error can be told apart: the remote's stream, or the local disk.
struct Tagged {
    body: Body,
    failed: Arc<AtomicBool>,
}

impl Read for Tagged {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.body.read(buf).inspect_err(|_| {
            self.failed.store(true, Ordering::Relaxed);
        })
    }
}

/// Mark copies done: journal them, drop them from the queue, compact when
/// settled lines pile up.
fn settle(queue: &mut Queue, done: &[(String, Key)]) -> Result<(), PlaceError> {
    let done: Vec<&(String, Key)> = done.iter().filter(|d| queue.pending.contains(*d)).collect();
    if done.is_empty() {
        return Ok(());
    }
    if let Some(journal) = &mut queue.journal {
        let lines: Vec<String> = done.iter().map(|(r, k)| format!("- {r} {k}")).collect();
        journal.append(&lines)?;
    }
    for d in done {
        queue.pending.remove(d);
        queue.settled += 1;
    }
    if queue.settled > COMPACT_AFTER.max(queue.pending.len()) {
        compact(queue)?;
    }
    Ok(())
}

/// Rewrite the queue journal as its live lines only.
fn compact(queue: &mut Queue) -> Result<(), PlaceError> {
    let lines: Vec<String> = queue
        .pending
        .iter()
        .map(|(r, k)| format!("+ {r} {k}"))
        .collect();
    if let Some(journal) = &mut queue.journal {
        journal.rewrite(&lines)?;
    }
    queue.settled = 0;
    Ok(())
}

impl<L: Place + LocalRemove> Place for WorkingCopy<L> {
    fn capabilities(&self) -> Capabilities {
        self.local.capabilities()
    }

    /// Done once local; queued for every remote policy routes it to — a
    /// remote this copy does not have included, so a typo'd route keeps the
    /// write from ever looking safe (push reports it).
    fn put_once(
        &self,
        key: &Key,
        hash: &Hash,
        size: u64,
        body: &mut dyn Read,
    ) -> Result<Put, PlaceError> {
        let routes = (self.route)(key);
        *self.queue().writing.entry(key.clone()).or_insert(0) += 1;
        let put = self.local.put_once(key, hash, size, body);
        let mut queue = self.queue();
        if let Some(n) = queue.writing.get_mut(key) {
            *n -= 1;
            if *n == 0 {
                queue.writing.remove(key);
            }
        }
        let put = put?;
        let new: Vec<(String, Key)> = routes
            .into_iter()
            .map(|name| (name, key.clone()))
            .filter(|entry| !queue.pending.contains(entry))
            .collect();
        if !new.is_empty() {
            if let Some(journal) = &mut queue.journal {
                let lines: Vec<String> = new.iter().map(|(r, k)| format!("+ {r} {k}")).collect();
                journal.append(&lines)?;
            }
            queue.pending.extend(new);
        }
        Ok(put)
    }

    /// Local, or read through from the first remote with an intact copy. A
    /// ranged read of a miss fetches the whole object first.
    fn get(&self, key: &Key, range: Option<ByteRange>) -> Result<Body, PlaceError> {
        match self.local.get(key, range) {
            Err(PlaceError::NotFound(_)) if self.read_through(key)? => self.local.get(key, range),
            other => other,
        }
    }

    /// What this device holds; [`WorkingCopy::locate`] asks the remotes.
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
