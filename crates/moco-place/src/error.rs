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
    /// A read here costs money or hours; such a place verifies itself or
    /// not at all.
    Cold,
    /// No place at this directory: it holds no place marker (an unmounted
    /// disk's mount point, or a directory never made a place).
    NotAPlace {
        root: std::path::PathBuf,
    },
    /// Another keeper holds this directory (another process, or another
    /// handle in this one).
    Busy(std::path::PathBuf),
    /// A working copy has no remote by this name.
    UnknownRemote(String),
    /// A working copy will not do this to this key: why.
    Refused {
        key: Key,
        reason: &'static str,
    },
    /// The place turned this machine away: credentials or permissions — a
    /// misconfiguration, not an outage worth waiting out.
    Denied(String),
    /// The place's backend failed in a way none of the above names.
    Backend(String),
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
            Self::Cold => f.write_str(
                "a read here costs money or hours: this place verifies itself or not at all",
            ),
            Self::NotAPlace { root } => write!(
                f,
                "no place at {}: it holds no place marker (unmounted, or never made a place)",
                root.display()
            ),
            Self::Busy(dir) => write!(
                f,
                "{} is kept by another process or handle already",
                dir.display()
            ),
            Self::UnknownRemote(name) => write!(f, "no remote named {name:?}"),
            Self::Refused { key, reason } => write!(f, "refused for {key}: {reason}"),
            Self::Denied(why) => write!(f, "the place refused this machine: {why}"),
            Self::Backend(why) => write!(f, "the place failed: {why}"),
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
