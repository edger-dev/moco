//! BLAKE3 content hashes, as 64 lowercase hex digits.
//!
//! The public API matches kinora's `Hash`, so kinora can re-export this one.

use std::fmt;
use std::io::{self, Read};
use std::str::FromStr;

const HASH_HEX_LEN: usize = 64;
/// How many leading digits a short hash shows.
pub const SHORTHASH_LEN: usize = 8;

/// A content hash.
#[derive(Debug, Clone, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub struct Hash(String);

/// Why a string is not a hash.
#[derive(Debug, Clone, Eq, PartialEq)]
pub enum HashParseError {
    WrongLength { got: usize },
    NotLowerHex,
}

impl fmt::Display for HashParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::WrongLength { got } => {
                write!(f, "hash must be {HASH_HEX_LEN} hex chars, got {got}")
            }
            Self::NotLowerHex => f.write_str("hash must be lowercase hex [0-9a-f]"),
        }
    }
}

impl std::error::Error for HashParseError {}

impl Hash {
    pub fn of_content(_content: &[u8]) -> Self {
        Self(String::new())
    }

    /// The hash of everything `reader` yields, and how many bytes that was.
    pub fn of_reader(_reader: &mut dyn Read) -> io::Result<(Self, u64)> {
        Ok((Self(String::new()), 0))
    }

    pub fn as_hex(&self) -> &str {
        &self.0
    }

    pub fn shorthash(&self) -> &str {
        &self.0
    }

    /// The first two digits: the directory a sharded layout files it under.
    pub fn shard(&self) -> &str {
        &self.0
    }
}

impl FromStr for Hash {
    type Err = HashParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(Self(s.to_owned()))
    }
}

impl fmt::Display for Hash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A hash computed incrementally, for bytes that arrive as a stream.
#[derive(Default)]
pub struct Hasher {
    inner: blake3::Hasher,
    len: u64,
}

impl Hasher {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn update(&mut self, _bytes: &[u8]) {}

    /// Bytes hashed so far.
    pub fn len(&self) -> u64 {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn finalize(&self) -> Hash {
        let _ = &self.inner;
        Hash(String::new())
    }
}
