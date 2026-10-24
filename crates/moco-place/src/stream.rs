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
use crate::place::{Child, Place, put_bytes, read_all};

/// Digits of an entry's timestamp: milliseconds until the year 33658.
const TS_DIGITS: usize = 15;

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
    pub fn with_ext(self, ext: &str) -> Self {
        Self {
            ext: Some(ext.to_owned()),
            ..self
        }
    }

    pub fn prefix(&self) -> &Key {
        &self.prefix
    }

    /// Append `bytes` as an entry stamped now.
    pub fn append(&self, place: &dyn Place, bytes: &[u8]) -> Result<Entry, PlaceError> {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        self.append_at(place, now, bytes)
    }

    /// Append `bytes` as an entry stamped `ts` (unix milliseconds).
    pub fn append_at(&self, place: &dyn Place, ts: u64, bytes: &[u8]) -> Result<Entry, PlaceError> {
        let hash = Hash::of_content(bytes);
        let name = match &self.ext {
            Some(ext) => format!("{ts:0TS_DIGITS$}-{hash}.{ext}"),
            None => format!("{ts:0TS_DIGITS$}-{hash}"),
        };
        let key = self.prefix.join(&name)?;
        put_bytes(place, &key, bytes)?;
        Ok(Entry { key, ts, hash })
    }

    /// Every entry, oldest first (ties by hash). Anything under the prefix
    /// not named like an entry is not one.
    pub fn entries(&self, place: &dyn Place) -> Result<Vec<Entry>, PlaceError> {
        let mut entries: Vec<Entry> = place
            .list(Some(&self.prefix))?
            .into_iter()
            .filter_map(|child| match child {
                Child::Object { key, .. } => self.parse(key),
                Child::Prefix(_) => None,
            })
            .collect();
        entries.sort_by(|a, b| (a.ts, &a.hash).cmp(&(b.ts, &b.hash)));
        Ok(entries)
    }

    /// The newest entry.
    pub fn latest(&self, place: &dyn Place) -> Result<Option<Entry>, PlaceError> {
        Ok(self.entries(place)?.pop())
    }

    /// The entry a key names, if it is one of this stream's.
    fn parse(&self, key: Key) -> Option<Entry> {
        let name = key.name();
        let stem = match &self.ext {
            Some(ext) => name.strip_suffix(ext.as_str())?.strip_suffix('.')?,
            None => name,
        };
        let (ts, hash) = stem.split_once('-')?;
        if ts.len() != TS_DIGITS || !ts.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        let ts = ts.parse().ok()?;
        let hash = hash.parse().ok()?;
        Some(Entry { key, ts, hash })
    }

    /// The entries no other entry names as a parent, oldest first.
    /// `parents` reads the parent hashes out of an entry's content — the
    /// stream's user owns that format.
    pub fn heads(
        &self,
        place: &dyn Place,
        parents: &dyn Fn(&[u8]) -> Vec<Hash>,
    ) -> Result<Vec<Entry>, PlaceError> {
        let entries = self.entries(place)?;
        let mut superseded = std::collections::HashSet::new();
        for entry in &entries {
            superseded.extend(parents(&read_all(place, &entry.key)?));
        }
        Ok(entries
            .into_iter()
            .filter(|e| !superseded.contains(&e.hash))
            .collect())
    }
}
