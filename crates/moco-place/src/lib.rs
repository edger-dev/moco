//! moco-place — content-addressed bytes behind one streaming trait.
//!
//! A [`Place`] holds write-once objects under [`Key`]s that name their own
//! content hash. The same trait carries a ledger's small files and a media
//! archive's large ones; a file-system directory, an in-memory map, an object
//! store and a place on another machine are all places.
//!
//! Framework-free on purpose: no cell framework, no facet. Bytes and hashes
//! are all that cross this boundary.
//!
//! implements: a7089331ac0dff49deb5acc9afce92b2a9d8a9d5022f66af915f3a2332d6f447@a7089331ac0dff49deb5acc9afce92b2a9d8a9d5022f66af915f3a2332d6f447
//! task: 8eb001aab192eaab652562506e51b47cbb032b1e69a491c9406ea333a40d5cb3@7840a6fb3279e6c7054b626944f8e202e79231afde7f4d3344d7410cc7e87f18

pub mod error;
pub mod fs;
pub mod hash;
pub mod key;
pub mod mem;
pub mod place;
pub mod stream;

pub use error::PlaceError;
pub use fs::FsPlace;
pub use hash::{Hash, HashParseError, Hasher, SHORTHASH_LEN};
pub use key::{Key, KeyError};
pub use mem::MemPlace;
pub use place::{
    ByteRange, Capabilities, Child, InventoryEntry, Place, Put, Verified, put_bytes, read_all,
};
pub use stream::{Entry, Stream};
