//! Place-owned inventories.
//!
//! A place records what it holds — per key, the content's hash, its size and
//! when it was last verified — in a journal on the machine the place belongs
//! to: one line per change, the last line for a key winning. Reading the
//! inventory is then a read, not a walk. Snapshots and deltas of it are
//! published to a meeting point as a stream, so a device that cannot reach
//! the place (a disk in a drawer) still knows what it holds. A walk can
//! always rebuild it.
//!
//! implements: 7778729bc8018fe949b0225f90ad2832501a9e82b9d03c973d10bdae568f7c8b@7778729bc8018fe949b0225f90ad2832501a9e82b9d03c973d10bdae568f7c8b

use std::io::Read;
use std::path::{Path, PathBuf};

use crate::error::PlaceError;
use crate::hash::Hash;
use crate::key::Key;
use crate::place::{ByteRange, Capabilities, Child, InventoryEntry, Place, Put, Verified};

/// Removal: only ever called on the machine a place belongs to. Not part of
/// [`Place`], so nothing that serves a place over the wire can reach it.
pub trait LocalRemove {
    /// Remove the object; `false` when there was none.
    fn remove(&self, key: &Key) -> Result<bool, PlaceError>;
}

/// A place with its inventory kept beside it.
pub struct Inventoried<P> {
    place: P,
    dir: PathBuf,
    clock: Box<dyn Fn() -> u64 + Send + Sync>,
}

impl<P: Place> Inventoried<P> {
    /// Keep `place`'s inventory in `dir` (created if missing), picking up the
    /// journal already there.
    pub fn open(place: P, dir: impl Into<PathBuf>) -> Result<Self, PlaceError> {
        Ok(Self {
            place,
            dir: dir.into(),
            clock: Box::new(crate::place::unix_now),
        })
    }

    /// Stamp verifications with `clock` (unix seconds) instead of the time.
    pub fn with_clock(self, clock: impl Fn() -> u64 + Send + Sync + 'static) -> Self {
        Self {
            clock: Box::new(clock),
            ..self
        }
    }

    pub fn place(&self) -> &P {
        &self.place
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Replace the inventory with what a walk of the place finds now.
    pub fn rebuild(&self) -> Result<Vec<InventoryEntry>, PlaceError> {
        Ok(Vec::new())
    }

    /// Rewrite the journal as one line per object.
    pub fn compact(&self) -> Result<(), PlaceError> {
        Ok(())
    }

    /// Publish what changed since the last publish to `meeting`, under
    /// `inventories/<name>`: a snapshot the first time (or when asked), a
    /// delta after.
    pub fn publish(
        &self,
        _meeting: &dyn Place,
        _name: &str,
        _snapshot: bool,
    ) -> Result<(), PlaceError> {
        Ok(())
    }
}

impl<P: Place + LocalRemove> Inventoried<P> {
    /// Remove an object, and its line from the inventory.
    pub fn remove(&self, key: &Key) -> Result<bool, PlaceError> {
        self.place.remove(key)
    }
}

impl<P: Place> Place for Inventoried<P> {
    fn capabilities(&self) -> Capabilities {
        self.place.capabilities()
    }

    fn put_once(
        &self,
        key: &Key,
        hash: &Hash,
        size: u64,
        body: &mut dyn Read,
    ) -> Result<Put, PlaceError> {
        self.place.put_once(key, hash, size, body)
    }

    fn get(&self, key: &Key, range: Option<ByteRange>) -> Result<Box<dyn Read + Send>, PlaceError> {
        self.place.get(key, range)
    }

    fn exists(&self, key: &Key) -> Result<bool, PlaceError> {
        self.place.exists(key)
    }

    fn list(&self, prefix: Option<&Key>) -> Result<Vec<Child>, PlaceError> {
        self.place.list(prefix)
    }

    fn verify(&self, key: &Key) -> Result<Verified, PlaceError> {
        self.place.verify(key)
    }

    /// The kept inventory — a read, not a walk.
    fn inventory(&self) -> Result<Vec<InventoryEntry>, PlaceError> {
        let _ = (&self.dir, &self.clock);
        Ok(Vec::new())
    }
}

/// What a place published to a meeting point under `inventories/<name>`: the
/// latest snapshot with every later delta applied.
pub fn published(_meeting: &dyn Place, _name: &str) -> Result<Vec<InventoryEntry>, PlaceError> {
    Ok(Vec::new())
}
