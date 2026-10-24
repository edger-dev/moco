//! A place in memory, for tests and for a working copy with no disk.

use std::collections::BTreeMap;
use std::io::Read;
use std::sync::{Arc, Mutex};

use crate::error::PlaceError;
use crate::hash::Hash;
use crate::key::Key;
use crate::place::{ByteRange, Capabilities, Child, Place, Put};

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
}

impl Place for MemPlace {
    fn capabilities(&self) -> Capabilities {
        self.capabilities
    }

    fn put_once(
        &self,
        _key: &Key,
        _hash: &Hash,
        _size: u64,
        _body: &mut dyn Read,
    ) -> Result<Put, PlaceError> {
        let _ = &self.objects;
        Err(PlaceError::NotWritable)
    }

    fn get(
        &self,
        key: &Key,
        _range: Option<ByteRange>,
    ) -> Result<Box<dyn Read + Send>, PlaceError> {
        Err(PlaceError::NotFound(key.clone()))
    }

    fn exists(&self, _key: &Key) -> Result<bool, PlaceError> {
        Ok(false)
    }

    fn list(&self, _prefix: Option<&Key>) -> Result<Vec<Child>, PlaceError> {
        Ok(Vec::new())
    }
}
