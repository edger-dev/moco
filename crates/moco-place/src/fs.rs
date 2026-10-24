//! A place in a directory of the local file system.

use std::fs::{self, File};
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::error::PlaceError;
use crate::hash::Hash;
use crate::key::Key;
use crate::place::{ByteRange, Capabilities, Child, Place, Put, copy_checked, key_names, span};

/// Temporaries are named apart per process and per put.
static TEMP_SEQ: AtomicU64 = AtomicU64::new(0);

/// Objects as plain files under a root directory, at their key's path.
/// Names starting with '.' are the place's own (temporaries, its records)
/// and never listed.
pub struct FsPlace {
    root: PathBuf,
}

impl FsPlace {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    fn path(&self, key: &Key) -> PathBuf {
        self.root.join(key.as_str())
    }

    fn not_found(key: &Key) -> impl FnOnce(io::Error) -> PlaceError + '_ {
        move |e| {
            if e.kind() == io::ErrorKind::NotFound {
                PlaceError::NotFound(key.clone())
            } else {
                PlaceError::Io(e)
            }
        }
    }
}

impl Place for FsPlace {
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            online: true,
            writable: true,
            cold: false,
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
        let path = self.path(key);
        if path.is_file() {
            return Ok(Put::Present);
        }
        let parent = path.parent().unwrap_or(&self.root);
        fs::create_dir_all(parent)?;
        let temp = parent.join(format!(
            ".tmp-{}-{}",
            std::process::id(),
            TEMP_SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        let written = File::create(&temp)
            .map_err(PlaceError::from)
            .and_then(|mut file| {
                copy_checked(key, hash, size, body, &mut file)?;
                file.sync_all()?;
                Ok(())
            });
        if let Err(e) = written {
            let _ = fs::remove_file(&temp);
            return Err(e);
        }
        // A hard link never replaces an existing file, so two writers racing
        // on one key cannot clobber each other (they hold the same bytes
        // anyway). A file system without hard links (exFAT on a USB disk)
        // falls back to a rename.
        let placed = match fs::hard_link(&temp, &path) {
            Ok(()) => Ok(Put::Stored),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => Ok(Put::Present),
            Err(_) => fs::rename(&temp, &path).map(|()| Put::Stored),
        };
        let _ = fs::remove_file(&temp);
        Ok(placed?)
    }

    fn get(&self, key: &Key, range: Option<ByteRange>) -> Result<Box<dyn Read + Send>, PlaceError> {
        let mut file = File::open(self.path(key)).map_err(Self::not_found(key))?;
        if file.metadata()?.is_dir() {
            return Err(PlaceError::NotFound(key.clone()));
        }
        let len = file.metadata()?.len();
        let (start, end) = span(range, len);
        file.seek(SeekFrom::Start(start))?;
        Ok(Box::new(file.take(end - start)))
    }

    fn exists(&self, key: &Key) -> Result<bool, PlaceError> {
        Ok(self.path(key).is_file())
    }

    fn list(&self, prefix: Option<&Key>) -> Result<Vec<Child>, PlaceError> {
        let dir = prefix.map_or_else(|| self.root.clone(), |p| self.path(p));
        let entries = match fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e.into()),
        };
        let mut children = Vec::new();
        for entry in entries {
            let entry = entry?;
            let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            // Dot-names and names no key could have are not objects here.
            let key = match prefix {
                Some(p) => p.join(&name),
                None => Key::new(name),
            };
            let Ok(key) = key else { continue };
            let meta = entry.metadata()?;
            if meta.is_dir() {
                children.push(Child::Prefix(key));
            } else if meta.is_file() {
                children.push(Child::Object {
                    key,
                    size: meta.len(),
                });
            }
        }
        children.sort_by(|a, b| a.key().cmp(b.key()));
        Ok(children)
    }
}
