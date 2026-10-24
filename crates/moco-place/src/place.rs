//! The trait every place implements.

use std::io::{self, Read, Write};

use crate::error::PlaceError;
use crate::hash::{Hash, Hasher};
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

impl Child {
    /// The key of the object or prefix.
    pub fn key(&self) -> &Key {
        match self {
            Self::Object { key, .. } | Self::Prefix(key) => key,
        }
    }
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
    /// What its bytes hashed to when last read — for a corrupt object, not
    /// the hash its key names.
    pub hash: Hash,
    pub size: u64,
    /// Unix seconds when its bytes were last hashed.
    pub verified_at: u64,
}

impl InventoryEntry {
    /// Its bytes hashed to what its key names: the place holds the object,
    /// not a corrupt copy of it.
    pub fn is_intact(&self) -> bool {
        self.key.named_hash().as_ref() == Some(&self.hash)
    }
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
    fn verify(&self, key: &Key) -> Result<Verified, PlaceError> {
        if self.capabilities().cold {
            return Err(PlaceError::Cold);
        }
        let Some(named) = key.named_hash() else {
            return Ok(Verified::Unnamed);
        };
        let mut body = match self.get(key, None) {
            Ok(body) => body,
            Err(PlaceError::NotFound(_)) => return Ok(Verified::Missing),
            Err(e) => return Err(e),
        };
        let (got, _) = Hash::of_reader(&mut body)?;
        Ok(if got == named {
            Verified::Intact
        } else {
            Verified::Corrupt { got }
        })
    }

    /// Every object the place holds that its key names a hash for, with the
    /// hash its bytes have now (a corrupt object shows a hash its key does
    /// not name). The default walks the place and hashes everything.
    fn inventory(&self) -> Result<Vec<InventoryEntry>, PlaceError> {
        if self.capabilities().cold {
            return Err(PlaceError::Cold);
        }
        let mut entries = Vec::new();
        let mut prefixes = vec![None];
        while let Some(prefix) = prefixes.pop() {
            for child in self.list(prefix.as_ref())? {
                match child {
                    Child::Prefix(key) => prefixes.push(Some(key)),
                    Child::Object { key, .. } if key.named_hash().is_some() => {
                        // Gone between the listing and the read: not held.
                        let mut body = match self.get(&key, None) {
                            Ok(body) => body,
                            Err(PlaceError::NotFound(_)) => continue,
                            Err(e) => return Err(e),
                        };
                        let (hash, size) = Hash::of_reader(&mut body)?;
                        entries.push(InventoryEntry {
                            key,
                            hash,
                            size,
                            verified_at: unix_now(),
                        });
                    }
                    Child::Object { .. } => {}
                }
            }
        }
        entries.sort_by(|a, b| a.key.cmp(&b.key));
        Ok(entries)
    }
}

macro_rules! forward_place {
    ($($ty:ty),*) => {$(
        impl<T: Place + ?Sized> Place for $ty {
            fn capabilities(&self) -> Capabilities {
                (**self).capabilities()
            }
            fn put_once(
                &self,
                key: &Key,
                hash: &Hash,
                size: u64,
                body: &mut dyn Read,
            ) -> Result<Put, PlaceError> {
                (**self).put_once(key, hash, size, body)
            }
            fn get(
                &self,
                key: &Key,
                range: Option<ByteRange>,
            ) -> Result<Box<dyn Read + Send>, PlaceError> {
                (**self).get(key, range)
            }
            fn exists(&self, key: &Key) -> Result<bool, PlaceError> {
                (**self).exists(key)
            }
            fn list(&self, prefix: Option<&Key>) -> Result<Vec<Child>, PlaceError> {
                (**self).list(prefix)
            }
            fn verify(&self, key: &Key) -> Result<Verified, PlaceError> {
                (**self).verify(key)
            }
            fn inventory(&self) -> Result<Vec<InventoryEntry>, PlaceError> {
                (**self).inventory()
            }
        }
    )*};
}

forward_place!(&T, Box<T>, std::sync::Arc<T>);

/// Put bytes already in memory: hash and size computed here.
pub fn put_bytes(place: &dyn Place, key: &Key, bytes: &[u8]) -> Result<Put, PlaceError> {
    place.put_once(
        key,
        &Hash::of_content(bytes),
        bytes.len() as u64,
        &mut &bytes[..],
    )
}

/// An object's whole content.
pub fn read_all(place: &dyn Place, key: &Key) -> Result<Vec<u8>, PlaceError> {
    let mut out = Vec::new();
    place.get(key, None)?.read_to_end(&mut out)?;
    Ok(out)
}

/// Seconds since the Unix epoch, now.
pub fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Refuse a put whose key does not name the hash it declares.
pub(crate) fn key_names(key: &Key, hash: &Hash) -> Result<(), PlaceError> {
    if key.named_hash().as_ref() == Some(hash) {
        Ok(())
    } else {
        Err(PlaceError::KeyNamesOtherHash {
            key: key.clone(),
            declared: hash.clone(),
        })
    }
}

/// Copy a put's body to `out`, hashing as it goes, and check it is the
/// declared size and hash. The caller discards `out` on an error.
pub(crate) fn copy_checked(
    key: &Key,
    hash: &Hash,
    size: u64,
    body: &mut dyn Read,
    out: &mut dyn Write,
) -> Result<(), PlaceError> {
    let mut hasher = Hasher::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = match body.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e.into()),
        };
        hasher.update(&buf[..n]);
        // Past the declared size already: no need to read the rest.
        if hasher.len() > size {
            return Err(PlaceError::SizeMismatch {
                key: key.clone(),
                declared: size,
                got: hasher.len(),
            });
        }
        out.write_all(&buf[..n])?;
    }
    if hasher.len() != size {
        return Err(PlaceError::SizeMismatch {
            key: key.clone(),
            declared: size,
            got: hasher.len(),
        });
    }
    let got = hasher.finalize();
    if &got != hash {
        return Err(PlaceError::ContentMismatch {
            key: key.clone(),
            declared: hash.clone(),
            got,
        });
    }
    Ok(())
}

/// The part of `len` bytes a range covers, clamped to them.
pub(crate) fn span(range: Option<ByteRange>, len: u64) -> (u64, u64) {
    match range {
        None => (0, len),
        Some(r) => {
            let start = r.start.min(len);
            let end = r.end.map_or(len, |e| e.min(len)).max(start);
            (start, end)
        }
    }
}
