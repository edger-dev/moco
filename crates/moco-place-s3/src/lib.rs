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

use std::future::Future;
use std::io::{self, Read};
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use futures::channel::{mpsc, oneshot};
use futures::{SinkExt, StreamExt};
use moco_place::place::{copy_checked, key_names, span};
use moco_place::{Body, ByteRange, Capabilities, Child, Hash, Hasher, Key, Place, PlaceError, Put};
use object_store::aws::{AmazonS3Builder, AmazonS3ConfigKey};
use object_store::local::LocalFileSystem;
use object_store::memory::InMemory;
use object_store::path::Path as StorePath;
use object_store::{
    ClientOptions, GetOptions, GetRange, ObjectStore, ObjectStoreExt, PutMode, PutOptions,
    PutPayload, RetryConfig,
};
use tokio::runtime::Runtime;

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
    /// Its own runtime; shut down in the background on drop, which is
    /// allowed even inside another runtime.
    runtime: Option<Runtime>,
}

impl ObjectPlace {
    pub fn new(store: Arc<dyn ObjectStore>, prefix: &str) -> Result<Self, PlaceError> {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("moco-place-s3")
            .enable_all()
            .build()?;
        Ok(Self {
            store,
            prefix: StorePath::from(prefix.trim_matches('/')),
            cold: false,
            multipart_threshold: DEFAULT_MULTIPART_THRESHOLD,
            part_size: DEFAULT_PART_SIZE,
            runtime: Some(runtime),
        })
    }

    /// A place named by URL, configured from the environment (`AWS_*` for
    /// `s3://`).
    pub fn from_url(url: &str) -> Result<Self, PlaceError> {
        Self::build(
            url,
            AmazonS3Builder::from_env(),
            Vec::<(String, String)>::new(),
        )
    }

    /// A place named by URL, with explicit options (for `s3://`, the
    /// `object_store` S3 config keys such as `aws_access_key_id`).
    pub fn from_url_with<K, V>(
        url: &str,
        options: impl IntoIterator<Item = (K, V)>,
    ) -> Result<Self, PlaceError>
    where
        K: AsRef<str>,
        V: Into<String>,
    {
        Self::build(url, AmazonS3Builder::new(), options)
    }

    fn build<K, V>(
        url: &str,
        s3: AmazonS3Builder,
        options: impl IntoIterator<Item = (K, V)>,
    ) -> Result<Self, PlaceError>
    where
        K: AsRef<str>,
        V: Into<String>,
    {
        let bad = |why: String| PlaceError::Backend(format!("place url {url:?}: {why}"));
        let parsed = url::Url::parse(url).map_err(|e| bad(e.to_string()))?;
        let path = parsed.path().trim_matches('/').to_owned();
        match parsed.scheme() {
            "s3" => {
                let bucket = parsed
                    .host_str()
                    .filter(|b| !b.is_empty())
                    .ok_or_else(|| bad("no bucket".into()))?;
                // Short budgets: a device that cannot reach the place falls
                // back to its working copy, and must find out in seconds,
                // not after minutes of retries.
                let mut s3 = s3
                    .with_bucket_name(bucket)
                    .with_client_options(
                        ClientOptions::new()
                            .with_connect_timeout(Duration::from_secs(3))
                            .with_timeout(Duration::from_secs(30)),
                    )
                    .with_retry(RetryConfig {
                        max_retries: 1,
                        retry_timeout: Duration::from_secs(8),
                        ..RetryConfig::default()
                    });
                for (key, value) in options {
                    let config: AmazonS3ConfigKey = key
                        .as_ref()
                        .parse()
                        .map_err(|e| bad(format!("option {}: {e}", key.as_ref())))?;
                    s3 = s3.with_config(config, value);
                }
                // A local S3 (MinIO for development) speaks plain HTTP: an
                // http:// endpoint is consent to it.
                if s3
                    .get_config_value(&AmazonS3ConfigKey::Endpoint)
                    .is_some_and(|e| e.starts_with("http://"))
                {
                    s3 = s3.with_allow_http(true);
                }
                let store = s3.build().map_err(|e| bad(e.to_string()))?;
                Self::new(Arc::new(store), &path)
            }
            "file" => {
                let dir = parsed
                    .to_file_path()
                    .map_err(|()| bad("not a local path".into()))?;
                let store =
                    LocalFileSystem::new_with_prefix(&dir).map_err(|e| bad(e.to_string()))?;
                Self::new(Arc::new(store), "")
            }
            "memory" => Self::new(Arc::new(InMemory::new()), &path),
            other => Err(bad(format!("unknown scheme {other:?}"))),
        }
    }

    /// Mark the place cold: a read costs money or hours.
    pub fn with_cold(mut self, cold: bool) -> Self {
        self.cold = cold;
        self
    }

    /// Puts larger than `threshold` go up in parts of `part_size`.
    pub fn with_multipart(mut self, threshold: u64, part_size: usize) -> Self {
        self.multipart_threshold = threshold;
        self.part_size = part_size.max(1);
        self
    }

    fn path(&self, key: &Key) -> StorePath {
        if self.prefix.as_ref().is_empty() {
            StorePath::from(key.as_str())
        } else {
            StorePath::from(format!("{}/{key}", self.prefix))
        }
    }

    /// The key a stored object sits under, if it is one of this place's.
    fn key_of(&self, location: &StorePath) -> Option<Key> {
        let parts: Vec<String> = if self.prefix.as_ref().is_empty() {
            location.parts().map(|p| p.as_ref().to_owned()).collect()
        } else {
            location
                .prefix_match(&self.prefix)?
                .map(|p| p.as_ref().to_owned())
                .collect()
        };
        Key::new(parts.join("/")).ok()
    }

    /// Run `future` on this place's runtime and wait for it — from plain
    /// code or from inside another runtime alike.
    fn run<T: Send + 'static>(
        &self,
        future: impl Future<Output = T> + Send + 'static,
    ) -> Result<T, PlaceError> {
        let runtime = self
            .runtime
            .as_ref()
            .ok_or_else(|| PlaceError::Backend("the place's runtime is gone".into()))?;
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        runtime.spawn(async move {
            let _ = tx.send(future.await);
        });
        rx.recv()
            .map_err(|_| PlaceError::Backend("a request ended without an answer".into()))
    }

    fn put_in_parts(
        &self,
        key: &Key,
        hash: &Hash,
        size: u64,
        body: &mut dyn Read,
    ) -> Result<Put, PlaceError> {
        // No conditional create for uploads in parts: check first. Two
        // writers racing on one key upload the same bytes.
        if self.exists(key)? {
            return Ok(Put::Present);
        }
        let store = self.store.clone();
        let path = self.path(key);
        let mut upload = self
            .run(async move { store.put_multipart(&path).await })?
            .map_err(|e| backend(e, key))?;
        let mut hasher = Hasher::new();
        let mut part = vec![0u8; self.part_size];
        let outcome: Result<(), PlaceError> = loop {
            let filled = match fill(body, &mut part) {
                Ok(n) => n,
                Err(e) => break Err(e.into()),
            };
            if filled == 0 {
                break Ok(());
            }
            hasher.update(&part[..filled]);
            if hasher.len() > size {
                break Err(PlaceError::SizeMismatch {
                    key: key.clone(),
                    declared: size,
                    got: hasher.len(),
                });
            }
            let sent = upload.put_part(PutPayload::from(Bytes::copy_from_slice(&part[..filled])));
            match self.run(sent) {
                Ok(Ok(())) => {}
                Ok(Err(e)) => break Err(backend(e, key)),
                Err(e) => break Err(e),
            }
        };
        let outcome = outcome.and_then(|()| {
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
        });
        match outcome {
            Ok(()) => {
                let done = self.run(async move { upload.complete().await })?;
                done.map(|_| Put::Stored).map_err(|e| backend(e, key))
            }
            Err(e) => {
                let _ = self.run(async move { upload.abort().await });
                Err(e)
            }
        }
    }
}

impl Drop for ObjectPlace {
    fn drop(&mut self) {
        if let Some(runtime) = self.runtime.take() {
            runtime.shutdown_background();
        }
    }
}

/// Read until `buf` is full or the body ends; how much was read.
fn fill(body: &mut dyn Read, buf: &mut [u8]) -> io::Result<usize> {
    let mut filled = 0;
    while filled < buf.len() {
        match body.read(&mut buf[filled..]) {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        }
    }
    Ok(filled)
}

/// What an object-store failure on `key` means for a place.
fn backend(e: object_store::Error, key: &Key) -> PlaceError {
    match e {
        object_store::Error::NotFound { .. } => PlaceError::NotFound(key.clone()),
        // A local directory store says so when a key runs below a file:
        // nothing is there.
        object_store::Error::Generic { .. } if e.to_string().contains("Not a directory") => {
            PlaceError::NotFound(key.clone())
        }
        other => classify(other),
    }
}

/// What an object-store failure means for a place, absent a key.
fn classify(e: object_store::Error) -> PlaceError {
    match e {
        object_store::Error::PermissionDenied { .. }
        | object_store::Error::Unauthenticated { .. } => PlaceError::Denied(e.to_string()),
        other => {
            // A refusal or an unreachable host come back generic, with the
            // status or the transport's words in the text.
            let text = format!("{other:?} {other}");
            if ["403 Forbidden", "401 Unauthorized"]
                .iter()
                .any(|s| text.contains(s))
            {
                PlaceError::Denied(other.to_string())
            } else if [
                "error sending request",
                "Connection refused",
                "connection refused",
                "timed out",
                "dns error",
                "Connect",
            ]
            .iter()
            .any(|s| text.contains(s))
            {
                PlaceError::Offline
            } else {
                PlaceError::Backend(other.to_string())
            }
        }
    }
}

/// One message from a get's task: its length first, then its bytes.
type Chunk = Result<Bytes, io::Error>;

/// A get's bytes as they arrive from the place's runtime.
struct Streamed {
    chunks: mpsc::Receiver<Chunk>,
    current: Bytes,
}

impl Read for Streamed {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        while self.current.is_empty() {
            match futures::executor::block_on(self.chunks.next()) {
                Some(Ok(bytes)) => self.current = bytes,
                Some(Err(e)) => return Err(e),
                None => return Ok(0),
            }
        }
        let n = self.current.len().min(buf.len());
        buf[..n].copy_from_slice(&self.current[..n]);
        self.current = self.current.slice(n..);
        Ok(n)
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
        key: &Key,
        hash: &Hash,
        size: u64,
        body: &mut dyn Read,
    ) -> Result<Put, PlaceError> {
        key_names(key, hash)?;
        if size > self.multipart_threshold {
            return self.put_in_parts(key, hash, size, body);
        }
        // Checked as it is read, so a mismatch is never sent.
        let mut bytes = Vec::with_capacity(size as usize);
        copy_checked(key, hash, size, body, &mut bytes)?;
        let store = self.store.clone();
        let path = self.path(key);
        let put = self.run(async move {
            let opts = PutOptions {
                mode: PutMode::Create,
                ..PutOptions::default()
            };
            store.put_opts(&path, PutPayload::from(bytes), opts).await
        })?;
        match put {
            Ok(_) => Ok(Put::Stored),
            Err(
                object_store::Error::AlreadyExists { .. }
                | object_store::Error::Precondition { .. },
            ) => Ok(Put::Present),
            Err(e) => Err(backend(e, key)),
        }
    }

    fn get(&self, key: &Key, range: Option<ByteRange>) -> Result<Body, PlaceError> {
        let path = self.path(key);
        // A range is clamped to the object, as every place reads ranges: past
        // its end or backwards is empty (S3 would answer 416).
        let range = match range {
            None => None,
            Some(r) => {
                let store = self.store.clone();
                let head_path = path.clone();
                let meta = self
                    .run(async move { store.head(&head_path).await })?
                    .map_err(|e| backend(e, key))?;
                let (start, end) = span(Some(r), meta.size);
                if start == end {
                    return Ok(Body::new(io::empty(), 0));
                }
                Some(GetRange::Bounded(start..end))
            }
        };
        let (len_tx, len_rx) = oneshot::channel::<Result<u64, object_store::Error>>();
        let (mut chunk_tx, chunk_rx) = mpsc::channel::<Chunk>(4);
        let store = self.store.clone();
        let runtime = self
            .runtime
            .as_ref()
            .ok_or_else(|| PlaceError::Backend("the place's runtime is gone".into()))?;
        runtime.spawn(async move {
            let opts = GetOptions {
                range,
                ..GetOptions::default()
            };
            let result = match store.get_opts(&path, opts).await {
                Ok(result) => result,
                Err(e) => {
                    let _ = len_tx.send(Err(e));
                    return;
                }
            };
            let len = result.range.end - result.range.start;
            if len_tx.send(Ok(len)).is_err() {
                return;
            }
            let mut stream = result.into_stream();
            while let Some(chunk) = stream.next().await {
                let chunk = chunk.map_err(io::Error::other);
                let failed = chunk.is_err();
                if chunk_tx.send(chunk).await.is_err() || failed {
                    return;
                }
            }
        });
        let len = futures::executor::block_on(len_rx)
            .map_err(|_| PlaceError::Backend("a get ended without an answer".into()))?
            .map_err(|e| backend(e, key))?;
        Ok(Body::new(
            Streamed {
                chunks: chunk_rx,
                current: Bytes::new(),
            },
            len,
        ))
    }

    fn exists(&self, key: &Key) -> Result<bool, PlaceError> {
        let store = self.store.clone();
        let path = self.path(key);
        match self.run(async move { store.head(&path).await })? {
            Ok(_) => Ok(true),
            Err(e) => match backend(e, key) {
                PlaceError::NotFound(_) => Ok(false),
                other => Err(other),
            },
        }
    }

    fn list(&self, prefix: Option<&Key>) -> Result<Vec<Child>, PlaceError> {
        let store = self.store.clone();
        let at = match prefix {
            Some(p) => Some(self.path(p)),
            None if self.prefix.as_ref().is_empty() => None,
            None => Some(self.prefix.clone()),
        };
        // Nothing under a prefix lists as nothing, whichever way a store
        // says so.
        let listed = match self.run(async move { store.list_with_delimiter(at.as_ref()).await })? {
            Ok(listed) => listed,
            Err(object_store::Error::NotFound { .. }) => return Ok(Vec::new()),
            Err(e) => return Err(classify(e)),
        };
        let mut children: Vec<Child> = listed
            .common_prefixes
            .iter()
            .filter_map(|p| self.key_of(p))
            .map(Child::Prefix)
            .chain(listed.objects.iter().filter_map(|meta| {
                self.key_of(&meta.location).map(|key| Child::Object {
                    key,
                    size: meta.size,
                })
            }))
            .collect();
        children.sort_by(|a, b| a.key().cmp(b.key()));
        Ok(children)
    }
}
