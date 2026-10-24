//! The trait every place implements.

use std::io::Read;

use crate::error::PlaceError;
use crate::hash::Hash;
use crate::key::Key;

/// What a place can do — the only ways places differ.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Capabilities {
    /// It can be reached now.
    pub online: bool,
    /// It takes puts.
    pub writable: bool,
    /// A read costs money or hours (an archive tier).
    pub cold: bool,
}

/// Bytes `start..end` of an object; `end: None` reads to its end.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ByteRange {
    pub start: u64,
    pub end: Option<u64>,
}

/// What a put-once did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Put {
    /// The object is new here.
    Stored,
    /// The place already held it; nothing was written.
    Present,
}

/// One entry one level below a prefix.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Child {
    Object { key: Key, size: u64 },
    Prefix(Key),
}

/// The outcome of checking one object against the hash its key names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verified {
    Intact,
    /// Its bytes now hash to something else.
    Corrupt {
        got: Hash,
    },
    Missing,
    /// The key names no hash, so there is nothing to check against.
    Unnamed,
}

/// One object a place holds, as of its last check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InventoryEntry {
    pub key: Key,
    pub hash: Hash,
    pub size: u64,
    /// Unix seconds when its bytes were last hashed and found intact.
    pub verified_at: u64,
}

/// A place holding write-once, content-addressed objects.
pub trait Place: Send + Sync {
    fn capabilities(&self) -> Capabilities;

    /// Store `body` under `key` unless the place already holds it. The caller
    /// declares the content's hash and size; the key must name that hash. A
    /// place that can check the bytes as they arrive refuses a mismatch and
    /// stores nothing.
    fn put_once(
        &self,
        key: &Key,
        hash: &Hash,
        size: u64,
        body: &mut dyn Read,
    ) -> Result<Put, PlaceError>;

    /// The object's bytes, or the range of them asked for.
    fn get(&self, key: &Key, range: Option<ByteRange>) -> Result<Box<dyn Read + Send>, PlaceError>;

    fn exists(&self, key: &Key) -> Result<bool, PlaceError>;

    /// What lies one level below `prefix` (`None`: the top), sorted by key.
    /// The place's own dot-names never appear.
    fn list(&self, prefix: Option<&Key>) -> Result<Vec<Child>, PlaceError>;

    /// Check an object against the hash its key names. The default reads it
    /// back and hashes it; a place that can do this where the bytes are
    /// overrides it.
    fn verify(&self, _key: &Key) -> Result<Verified, PlaceError> {
        Ok(Verified::Unnamed)
    }

    /// Every object the place holds that its key names a hash for. The
    /// default walks the place and hashes everything.
    fn inventory(&self) -> Result<Vec<InventoryEntry>, PlaceError> {
        Ok(Vec::new())
    }
}

/// Put bytes already in memory: hash and size computed here.
pub fn put_bytes(_place: &dyn Place, _key: &Key, _bytes: &[u8]) -> Result<Put, PlaceError> {
    Err(PlaceError::NotWritable)
}

/// An object's whole content.
pub fn read_all(_place: &dyn Place, key: &Key) -> Result<Vec<u8>, PlaceError> {
    Err(PlaceError::NotFound(key.clone()))
}
