//! moco-place-s3 — a place in an object store.
//!
//! S3 and anything speaking it (Cloudflare R2, MinIO), or `object_store`'s
//! local-directory and in-memory stores, named by URL: `s3://bucket/prefix`
//! (endpoint and keys from the `AWS_*` environment, so any S3 tool's
//! configuration carries over), `file:///dir`, `memory://`.
//!
//! The `Place` trait is sync; this place runs its requests on a runtime of
//! its own, so it is safe to call from plain code and from inside another
//! runtime alike. A put declares its hash, so the bytes are checked as they
//! are sent: nothing that does not match is ever uploaded whole (a large
//! upload is aborted). Small puts are a conditional create (`If-None-Match:
//! *`); a large one goes up in parts after a check that it is absent — two
//! writers racing on one key upload the same bytes, so the race is harmless.
//!
//! implements: a7089331ac0dff49deb5acc9afce92b2a9d8a9d5022f66af915f3a2332d6f447@a7089331ac0dff49deb5acc9afce92b2a9d8a9d5022f66af915f3a2332d6f447
//! implements: 95cf2a2fda557b922e0e3ff0ddaf48c25d7e598de9358cb53985b8f1aa7b3aa0@95cf2a2fda557b922e0e3ff0ddaf48c25d7e598de9358cb53985b8f1aa7b3aa0

use std::io::Read;
use std::sync::Arc;

use moco_place::{Body, ByteRange, Capabilities, Child, Hash, Key, Place, PlaceError, Put};
use object_store::ObjectStore;
use object_store::path::Path as StorePath;

/// Puts larger than this go up in parts.
pub const DEFAULT_MULTIPART_THRESHOLD: u64 = 16 * 1024 * 1024;
/// The size of each part (S3's minimum is 5 MiB, bar the last).
pub const DEFAULT_PART_SIZE: usize = 8 * 1024 * 1024;

/// A place in an object store, under a prefix.
pub struct ObjectPlace {
    store: Arc<dyn ObjectStore>,
    prefix: StorePath,
    cold: bool,
    multipart_threshold: u64,
    part_size: usize,
}

impl ObjectPlace {
    pub fn new(store: Arc<dyn ObjectStore>, prefix: &str) -> Result<Self, PlaceError> {
        Ok(Self {
            store,
            prefix: StorePath::from(prefix),
            cold: false,
            multipart_threshold: DEFAULT_MULTIPART_THRESHOLD,
            part_size: DEFAULT_PART_SIZE,
        })
    }

    /// A place named by URL, configured from the environment (`AWS_*` for
    /// `s3://`).
    pub fn from_url(url: &str) -> Result<Self, PlaceError> {
        Err(PlaceError::Backend(format!("unbuilt: {url}")))
    }

    /// A place named by URL, with explicit options (for `s3://`, the
    /// `object_store` S3 config keys such as `aws_access_key_id`).
    pub fn from_url_with<K, V>(
        url: &str,
        _options: impl IntoIterator<Item = (K, V)>,
    ) -> Result<Self, PlaceError>
    where
        K: AsRef<str>,
        V: Into<String>,
    {
        Err(PlaceError::Backend(format!("unbuilt: {url}")))
    }

    /// Mark the place cold: a read costs money or hours.
    pub fn with_cold(self, cold: bool) -> Self {
        Self { cold, ..self }
    }

    /// Puts larger than `threshold` go up in parts of `part_size`.
    pub fn with_multipart(self, threshold: u64, part_size: usize) -> Self {
        Self {
            multipart_threshold: threshold,
            part_size,
            ..self
        }
    }
}

impl Place for ObjectPlace {
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            online: true,
            writable: true,
            cold: self.cold,
        }
    }

    fn put_once(
        &self,
        _key: &Key,
        _hash: &Hash,
        _size: u64,
        _body: &mut dyn Read,
    ) -> Result<Put, PlaceError> {
        let _ = (
            &self.store,
            &self.prefix,
            self.multipart_threshold,
            self.part_size,
        );
        Err(PlaceError::Backend("unbuilt".into()))
    }

    fn get(&self, key: &Key, _range: Option<ByteRange>) -> Result<Body, PlaceError> {
        Err(PlaceError::NotFound(key.clone()))
    }

    fn exists(&self, _key: &Key) -> Result<bool, PlaceError> {
        Err(PlaceError::Backend("unbuilt".into()))
    }

    fn list(&self, _prefix: Option<&Key>) -> Result<Vec<Child>, PlaceError> {
        Err(PlaceError::Backend("unbuilt".into()))
    }
}
