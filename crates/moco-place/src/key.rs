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
        let key = key.into();
        let refuse = |reason| {
            Err(KeyError {
                key: key.clone(),
                reason,
            })
        };
        if key.is_empty() {
            return refuse("empty");
        }
        for segment in key.split('/') {
            if segment.is_empty() {
                return refuse("a segment is empty (leading, trailing or doubled '/')");
            }
            if segment.starts_with('.') {
                return refuse("a segment starts with '.': dot-names are the place's own");
            }
            if !segment
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
            {
                return refuse("a segment holds a character other than [A-Za-z0-9._-]");
            }
        }
        Ok(Self(key))
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
        self.0.rsplit('/').next().unwrap_or(&self.0)
    }

    /// Everything before the last segment; `None` at the top level.
    pub fn parent(&self) -> Option<Self> {
        self.0
            .rsplit_once('/')
            .map(|(parent, _)| Self(parent.to_owned()))
    }

    /// The content hash the last segment names, if it names one. The name
    /// is `[<prefix>-]<hash>[.<ext>]`, the extension one run of letters and
    /// digits: so `v1.2-<hash>` names its hash, and a tool's leftover
    /// `<hash>.jsonl.tmp` does not.
    pub fn named_hash(&self) -> Option<Hash> {
        let name = self.name();
        let stem = match name.rsplit_once('.') {
            Some((stem, ext))
                if !ext.is_empty() && ext.bytes().all(|b| b.is_ascii_alphanumeric()) =>
            {
                stem
            }
            _ => name,
        };
        let split = stem.len().checked_sub(64)?;
        let (before, digits) = (stem.get(..split)?, stem.get(split..)?);
        if !(before.is_empty() || before.ends_with('-')) {
            return None;
        }
        digits.parse().ok()
    }
}

impl fmt::Display for Key {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}
