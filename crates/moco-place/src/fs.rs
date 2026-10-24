//! A place in a directory of the local file system.

use std::io::Read;
use std::path::{Path, PathBuf};

use crate::error::PlaceError;
use crate::hash::Hash;
use crate::key::Key;
use crate::place::{ByteRange, Capabilities, Child, Place, Put};

/// Objects as plain files under a root directory, at their key's path.
pub struct FsPlace {
    root: PathBuf,
}

impl FsPlace {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }
}

impl Place for FsPlace {
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            online: true,
            writable: true,
            cold: false,
        }
    }

    fn put_once(
        &self,
        _key: &Key,
        _hash: &Hash,
        _size: u64,
        _body: &mut dyn Read,
    ) -> Result<Put, PlaceError> {
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
