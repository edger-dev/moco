//! A place in memory, for tests and for a working copy with no disk.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{Cursor, Read};
use std::sync::{Arc, Mutex, MutexGuard};

use crate::error::PlaceError;
use crate::hash::Hash;
use crate::key::Key;
use crate::place::{ByteRange, Capabilities, Child, Place, Put, copy_checked, key_names, span};

/// Objects in a map, shared by clones.
#[derive(Clone)]
pub struct MemPlace {
    objects: Arc<Mutex<BTreeMap<Key, Arc<Vec<u8>>>>>,
    capabilities: Capabilities,
}

impl Default for MemPlace {
    fn default() -> Self {
        Self::new()
    }
}

impl MemPlace {
    pub fn new() -> Self {
        Self::with_capabilities(Capabilities {
            online: true,
            writable: true,
            cold: false,
        })
    }

    /// A place that says it can do only what `capabilities` says — an
    /// offline or read-only place for tests.
    pub fn with_capabilities(capabilities: Capabilities) -> Self {
        Self {
            objects: Arc::default(),
            capabilities,
        }
    }

    fn objects(&self) -> MutexGuard<'_, BTreeMap<Key, Arc<Vec<u8>>>> {
        // A panic while holding the lock leaves the map as it was: every
        // change is one insert.
        self.objects
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn reachable(&self) -> Result<(), PlaceError> {
        if self.capabilities.online {
            Ok(())
        } else {
            Err(PlaceError::Offline)
        }
    }
}

impl Place for MemPlace {
    fn capabilities(&self) -> Capabilities {
        self.capabilities
    }

    fn put_once(
        &self,
        key: &Key,
        hash: &Hash,
        size: u64,
        body: &mut dyn Read,
    ) -> Result<Put, PlaceError> {
        self.reachable()?;
        if !self.capabilities.writable {
            return Err(PlaceError::NotWritable);
        }
        key_names(key, hash)?;
        if self.objects().contains_key(key) {
            return Ok(Put::Present);
        }
        let mut bytes = Vec::new();
        copy_checked(key, hash, size, body, &mut bytes)?;
        let mut objects = self.objects();
        if objects.contains_key(key) {
            return Ok(Put::Present);
        }
        objects.insert(key.clone(), Arc::new(bytes));
        Ok(Put::Stored)
    }

    fn get(&self, key: &Key, range: Option<ByteRange>) -> Result<Box<dyn Read + Send>, PlaceError> {
        self.reachable()?;
        let bytes = self
            .objects()
            .get(key)
            .cloned()
            .ok_or_else(|| PlaceError::NotFound(key.clone()))?;
        let (start, end) = span(range, bytes.len() as u64);
        let mut cursor = Cursor::new(bytes);
        cursor.set_position(start);
        Ok(Box::new(ArcReader(cursor).take(end - start)))
    }

    fn exists(&self, key: &Key) -> Result<bool, PlaceError> {
        self.reachable()?;
        Ok(self.objects().contains_key(key))
    }

    fn list(&self, prefix: Option<&Key>) -> Result<Vec<Child>, PlaceError> {
        self.reachable()?;
        let lead = prefix.map(|p| format!("{p}/")).unwrap_or_default();
        let mut objects = BTreeMap::new();
        let mut prefixes = BTreeSet::new();
        for (key, bytes) in self.objects().range(..) {
            let Some(rest) = key.as_str().strip_prefix(&lead) else {
                continue;
            };
            match rest.split_once('/') {
                Some((first, _)) => {
                    prefixes.insert(format!("{lead}{first}"));
                }
                None => {
                    objects.insert(key.clone(), bytes.len() as u64);
                }
            }
        }
        let mut children: Vec<Child> = objects
            .into_iter()
            .map(|(key, size)| Child::Object { key, size })
            .chain(
                prefixes
                    .into_iter()
                    .filter_map(|p| Key::new(p).ok())
                    .map(Child::Prefix),
            )
            .collect();
        children.sort_by(|a, b| a.key().cmp(b.key()));
        Ok(children)
    }
}

/// A cursor over shared bytes, readable as a stream.
struct ArcReader(Cursor<Arc<Vec<u8>>>);

impl Read for ArcReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let pos = self.0.position() as usize;
        let bytes = self.0.get_ref();
        let available = bytes.len().saturating_sub(pos);
        let n = available.min(buf.len());
        buf[..n].copy_from_slice(&bytes[pos..pos + n]);
        self.0.set_position((pos + n) as u64);
        Ok(n)
    }
}

impl crate::inventory::LocalRemove for MemPlace {
    fn remove(&self, key: &Key) -> Result<bool, PlaceError> {
        self.reachable()?;
        Ok(self.objects().remove(key).is_some())
    }
}
