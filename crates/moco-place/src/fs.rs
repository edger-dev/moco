//! A place in a directory of the local file system.

use std::fs::{self, File, OpenOptions};
use std::io::{self, ErrorKind, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime};

use crate::error::PlaceError;
use crate::hash::Hash;
use crate::key::Key;
use crate::place::{ByteRange, Capabilities, Child, Place, Put, copy_checked, key_names, span};

/// The file that makes a directory a place. A mount point with its disk
/// unplugged lacks it, so an unmounted disk reads as offline, never as an
/// empty place to write into.
const MARKER: &str = ".moco-place";
/// Temporaries start so; a sweep clears old ones a crash left behind.
const TEMP_PREFIX: &str = ".tmp-";

/// Temporaries are named apart per put within a process.
static TEMP_SEQ: AtomicU64 = AtomicU64::new(0);

/// Objects as plain files under a root directory, at their key's path.
/// Names starting with '.' are the place's own (its marker, temporaries,
/// records) and never listed. Symlinked directories and files are followed.
pub struct FsPlace {
    root: PathBuf,
}

impl FsPlace {
    /// Make `root` a place (creating it if needed) and open it.
    pub fn create(root: impl Into<PathBuf>) -> Result<Self, PlaceError> {
        let root = root.into();
        fs::create_dir_all(&root)?;
        let marker = root.join(MARKER);
        if !marker.is_file() {
            fs::write(&marker, b"moco place\n")?;
        }
        Ok(Self { root })
    }

    /// Open the place at `root`, which must already be one.
    pub fn open(root: impl Into<PathBuf>) -> Result<Self, PlaceError> {
        let root = root.into();
        if root.join(MARKER).is_file() {
            Ok(Self { root })
        } else {
            Err(PlaceError::NotAPlace { root })
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    fn path(&self, key: &Key) -> PathBuf {
        self.root.join(key.as_str())
    }

    fn online(&self) -> bool {
        self.root.join(MARKER).is_file()
    }

    fn reachable(&self) -> Result<(), PlaceError> {
        if self.online() {
            Ok(())
        } else {
            Err(PlaceError::Offline)
        }
    }

    /// Remove temporaries older than `older_than` that crashed puts left
    /// behind; how many were removed.
    pub fn sweep_temporaries(&self, older_than: Duration) -> Result<usize, PlaceError> {
        self.reachable()?;
        let mut removed = 0;
        let mut dirs = vec![self.root.clone()];
        while let Some(dir) = dirs.pop() {
            let entries = match fs::read_dir(&dir) {
                Ok(entries) => entries,
                Err(e) if missing(&e) => continue,
                Err(e) => return Err(e.into()),
            };
            for entry in entries {
                let entry = entry?;
                let name = entry.file_name();
                let name = name.to_string_lossy();
                let Ok(meta) = entry.metadata() else { continue };
                if meta.is_dir() && !name.starts_with('.') {
                    dirs.push(entry.path());
                } else if meta.is_file() && name.starts_with(TEMP_PREFIX) {
                    let age = meta
                        .modified()
                        .ok()
                        .and_then(|m| SystemTime::now().duration_since(m).ok())
                        .unwrap_or_default();
                    if age >= older_than && fs::remove_file(entry.path()).is_ok() {
                        removed += 1;
                    }
                }
            }
        }
        Ok(removed)
    }
}

/// Nothing at this path: not there, or below a file.
fn missing(e: &io::Error) -> bool {
    matches!(e.kind(), ErrorKind::NotFound | ErrorKind::NotADirectory)
}

/// Make a directory's new entries durable; best effort where a platform
/// cannot sync a directory.
fn sync_dir(dir: &Path) {
    if let Ok(d) = File::open(dir) {
        let _ = d.sync_all();
    }
}

impl Place for FsPlace {
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            online: self.online(),
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
        self.reachable()?;
        key_names(key, hash)?;
        let path = self.path(key);
        let occupied = |path: &Path| -> Result<Option<Put>, PlaceError> {
            match fs::metadata(path) {
                Ok(meta) if meta.is_file() => Ok(Some(Put::Present)),
                Ok(_) => Err(PlaceError::Io(io::Error::new(
                    ErrorKind::AlreadyExists,
                    format!("something other than an object sits at {key}"),
                ))),
                Err(e) if missing(&e) => Ok(None),
                Err(e) => Err(e.into()),
            }
        };
        if let Some(present) = occupied(&path)? {
            return Ok(present);
        }
        let parent = path.parent().unwrap_or(&self.root);
        fs::create_dir_all(parent)?;

        // A name no other writer can hold: created exclusively, so a
        // collision (another host sharing this disk) is retried, never
        // shared.
        let (temp, mut file) = loop {
            let nanos = SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .map(|d| d.subsec_nanos())
                .unwrap_or(0);
            let temp = parent.join(format!(
                "{TEMP_PREFIX}{}-{}-{nanos}",
                std::process::id(),
                TEMP_SEQ.fetch_add(1, Ordering::Relaxed)
            ));
            match OpenOptions::new().write(true).create_new(true).open(&temp) {
                Ok(file) => break (temp, file),
                Err(e) if e.kind() == ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(e.into()),
            }
        };
        let written = copy_checked(key, hash, size, body, &mut file)
            .and_then(|()| file.sync_all().map_err(PlaceError::from));
        drop(file);
        if let Err(e) = written {
            let _ = fs::remove_file(&temp);
            return Err(e);
        }
        // A hard link never replaces an existing file, so racing writers
        // cannot clobber each other. A file system without hard links (exFAT
        // on a USB disk refuses them) falls back to a rename.
        let placed = match fs::hard_link(&temp, &path) {
            Ok(()) => Ok(Put::Stored),
            Err(e) if e.kind() == ErrorKind::AlreadyExists => {
                occupied(&path).map(|p| p.unwrap_or(Put::Present))
            }
            Err(e)
                if matches!(
                    e.kind(),
                    ErrorKind::Unsupported | ErrorKind::PermissionDenied
                ) =>
            {
                match occupied(&path) {
                    Ok(Some(present)) => Ok(present),
                    Ok(None) => fs::rename(&temp, &path)
                        .map(|()| Put::Stored)
                        .map_err(PlaceError::from),
                    Err(e) => Err(e),
                }
            }
            Err(e) => Err(e.into()),
        };
        let _ = fs::remove_file(&temp);
        sync_dir(parent);
        placed
    }

    fn get(&self, key: &Key, range: Option<ByteRange>) -> Result<Box<dyn Read + Send>, PlaceError> {
        self.reachable()?;
        let mut file = File::open(self.path(key)).map_err(|e| {
            if missing(&e) {
                PlaceError::NotFound(key.clone())
            } else {
                PlaceError::Io(e)
            }
        })?;
        let meta = file.metadata()?;
        if !meta.is_file() {
            return Err(PlaceError::NotFound(key.clone()));
        }
        let (start, end) = span(range, meta.len());
        file.seek(SeekFrom::Start(start))?;
        Ok(Box::new(file.take(end - start)))
    }

    fn exists(&self, key: &Key) -> Result<bool, PlaceError> {
        self.reachable()?;
        match fs::metadata(self.path(key)) {
            Ok(meta) => Ok(meta.is_file()),
            Err(e) if missing(&e) => Ok(false),
            Err(e) => Err(e.into()),
        }
    }

    fn list(&self, prefix: Option<&Key>) -> Result<Vec<Child>, PlaceError> {
        self.reachable()?;
        let dir = prefix.map_or_else(|| self.root.clone(), |p| self.path(p));
        let entries = match fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(e) if missing(&e) => return Ok(Vec::new()),
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
            // Follows symlinks; a dangling one, or an entry gone since the
            // listing, is not there.
            let meta = match fs::metadata(entry.path()) {
                Ok(meta) => meta,
                Err(e) if missing(&e) => continue,
                Err(e) => return Err(e.into()),
            };
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

impl crate::inventory::LocalRemove for FsPlace {
    fn remove(&self, key: &Key) -> Result<bool, PlaceError> {
        self.reachable()?;
        let path = self.path(key);
        match fs::metadata(&path) {
            Ok(meta) if meta.is_file() => {}
            Ok(_) => return Ok(false),
            Err(e) if missing(&e) => return Ok(false),
            Err(e) => return Err(e.into()),
        }
        match fs::remove_file(&path) {
            Ok(()) => {
                sync_dir(path.parent().unwrap_or(&self.root));
                Ok(true)
            }
            Err(e) if missing(&e) => Ok(false),
            Err(e) => Err(e.into()),
        }
    }
}
