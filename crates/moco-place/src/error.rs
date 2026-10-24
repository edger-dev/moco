//! What can go wrong at a place.

use std::fmt;
use std::io;

use crate::hash::Hash;
use crate::key::{Key, KeyError};

#[derive(Debug)]
pub enum PlaceError {
    /// No object under this key.
    NotFound(Key),
    /// A put declared a hash its key does not name.
    KeyNamesOtherHash {
        key: Key,
        declared: Hash,
    },
    /// A put's bytes do not hash to what it declared; nothing was stored.
    ContentMismatch {
        key: Key,
        declared: Hash,
        got: Hash,
    },
    /// A put's bytes are not as long as it declared; nothing was stored.
    SizeMismatch {
        key: Key,
        declared: u64,
        got: u64,
    },
    /// The place takes no writes.
    NotWritable,
    /// The place cannot be reached now.
    Offline,
    /// A key could not be formed (a stream's extension, say).
    BadKey(KeyError),
    Io(io::Error),
}

impl fmt::Display for PlaceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotFound(key) => write!(f, "no object at {key}"),
            Self::KeyNamesOtherHash { key, declared } => write!(
                f,
                "key {key} does not name the declared hash {declared}: a key names its content"
            ),
            Self::ContentMismatch { key, declared, got } => write!(
                f,
                "the bytes put at {key} hash to {got}, not the declared {declared}; nothing was stored"
            ),
            Self::SizeMismatch { key, declared, got } => write!(
                f,
                "the bytes put at {key} are {got} long, not the declared {declared}; nothing was stored"
            ),
            Self::NotWritable => f.write_str("this place takes no writes"),
            Self::Offline => f.write_str("this place cannot be reached now"),
            Self::BadKey(e) => write!(f, "{e}"),
            Self::Io(e) => write!(f, "place i/o: {e}"),
        }
    }
}

impl std::error::Error for PlaceError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(e) => Some(e),
            Self::BadKey(e) => Some(e),
            _ => None,
        }
    }
}

impl From<io::Error> for PlaceError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}

impl From<KeyError> for PlaceError {
    fn from(e: KeyError) -> Self {
        Self::BadKey(e)
    }
}
