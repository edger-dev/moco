//! Streams: mutable state as append-only, write-once entries.
//!
//! Anything that would be a mutable pointer — a root's current version, a
//! place's inventory snapshot — is a stream: entries under one prefix, each
//! named `<ts>-<hash>[.<ext>]` (milliseconds, zero-padded so names sort as
//! times do), the latest found by listing. Two writers appending at once
//! leave two entries; whoever writes next folds them by naming both as
//! parents. No compare-and-swap anywhere.
//!
//! implements: c08aa5551e04cf03a5f4df25c0f647b02dd78d7eacfb3872cb197affc6b52d2a@c08aa5551e04cf03a5f4df25c0f647b02dd78d7eacfb3872cb197affc6b52d2a

use crate::error::PlaceError;
use crate::hash::Hash;
use crate::key::Key;
use crate::place::Place;

/// One entry of a stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub key: Key,
    /// Unix milliseconds it was appended at.
    pub ts: u64,
    /// Its content's hash.
    pub hash: Hash,
}

/// A stream: a prefix in a place, and the extension its entries carry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stream {
    prefix: Key,
    ext: Option<String>,
}

impl Stream {
    pub fn new(prefix: Key) -> Self {
        Self { prefix, ext: None }
    }

    /// Entries carry `.<ext>` after their hash.
    pub fn with_ext(self, _ext: &str) -> Self {
        self
    }

    pub fn prefix(&self) -> &Key {
        &self.prefix
    }

    /// Append `bytes` as an entry stamped now.
    pub fn append(&self, place: &dyn Place, bytes: &[u8]) -> Result<Entry, PlaceError> {
        self.append_at(place, 0, bytes)
    }

    /// Append `bytes` as an entry stamped `ts` (unix milliseconds).
    pub fn append_at(
        &self,
        _place: &dyn Place,
        _ts: u64,
        _bytes: &[u8],
    ) -> Result<Entry, PlaceError> {
        Err(PlaceError::NotWritable)
    }

    /// Every entry, oldest first (ties by hash). Anything under the prefix
    /// not named like an entry is not one.
    pub fn entries(&self, _place: &dyn Place) -> Result<Vec<Entry>, PlaceError> {
        Ok(Vec::new())
    }

    /// The newest entry.
    pub fn latest(&self, _place: &dyn Place) -> Result<Option<Entry>, PlaceError> {
        Ok(None)
    }

    /// The entries no other entry names as a parent, oldest first.
    /// `parents` reads the parent hashes out of an entry's content — the
    /// stream's user owns that format.
    pub fn heads(
        &self,
        _place: &dyn Place,
        _parents: &dyn Fn(&[u8]) -> Vec<Hash>,
    ) -> Result<Vec<Entry>, PlaceError> {
        Ok(Vec::new())
    }
}
