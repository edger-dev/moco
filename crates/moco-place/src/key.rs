//! Keys: where an object lives in a place, and which content it holds.
//!
//! A key is a relative path of plain segments. Its last segment names the
//! object's content hash — `<hash>`, `<hash>.<ext>`, or `<prefix>-<hash>`
//! (with an optional extension) — so a place can verify any object, or
//! rebuild its inventory, from the key alone.

use std::fmt;

use crate::hash::Hash;

/// A path inside a place.
#[derive(Debug, Clone, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub struct Key(String);

/// Why a string is not a key.
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct KeyError {
    pub key: String,
    pub reason: &'static str,
}

impl fmt::Display for KeyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "key {:?}: {}", self.key, self.reason)
    }
}

impl std::error::Error for KeyError {}

impl Key {
    pub fn new(key: impl Into<String>) -> Result<Self, KeyError> {
        Ok(Self(key.into()))
    }

    /// This key with one more segment.
    pub fn join(&self, segment: &str) -> Result<Self, KeyError> {
        Self::new(format!("{}/{segment}", self.0))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The last segment.
    pub fn name(&self) -> &str {
        &self.0
    }

    /// Everything before the last segment; `None` at the top level.
    pub fn parent(&self) -> Option<Self> {
        None
    }

    /// The content hash the last segment names, if it names one.
    pub fn named_hash(&self) -> Option<Hash> {
        None
    }
}

impl fmt::Display for Key {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}
