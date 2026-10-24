//! Append-only line journals, kept on the machine that owns them.
//!
//! Lines are appended and synced; a crash mid-line leaves a torn tail, cut
//! off when the journal is next opened, and a failed append is cut back at
//! once — so a journal always ends at a line's end and no line ever glues
//! onto a fragment. A rewrite replaces the whole file durably.

use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use crate::error::PlaceError;

/// One journal file and its length up to the last whole line.
pub(crate) struct Journal {
    path: PathBuf,
    len: u64,
}

impl Journal {
    /// Open the journal at `path` (empty if missing): its whole lines, with
    /// a torn tail cut off.
    pub(crate) fn open(path: PathBuf) -> Result<(Self, Vec<String>), PlaceError> {
        let file = match File::open(&path) {
            Ok(file) => file,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok((Self { path, len: 0 }, Vec::new()));
            }
            Err(e) => return Err(e.into()),
        };
        let mut lines = Vec::new();
        let mut len = 0u64;
        let mut reader = BufReader::new(file);
        let mut line = String::new();
        loop {
            line.clear();
            let n = reader.read_line(&mut line)?;
            if n == 0 || !line.ends_with('\n') {
                break;
            }
            lines.push(line.trim_end_matches('\n').to_owned());
            len += n as u64;
        }
        if fs::metadata(&path)?.len() > len {
            OpenOptions::new().write(true).open(&path)?.set_len(len)?;
        }
        Ok((Self { path, len }, lines))
    }

    /// Bytes up to the last whole line.
    pub(crate) fn len(&self) -> u64 {
        self.len
    }

    /// Append `lines` and sync them; on failure, cut back what was written.
    pub(crate) fn append(&mut self, lines: &[String]) -> Result<(), PlaceError> {
        let created = !self.path.exists();
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        let text: String = lines.iter().map(|l| format!("{l}\n")).collect();
        let written = file
            .write_all(text.as_bytes())
            .and_then(|()| file.sync_data());
        if let Err(e) = written {
            let _ = file.set_len(self.len);
            return Err(e.into());
        }
        if created && let Some(dir) = self.path.parent() {
            sync_dir(dir);
        }
        self.len += text.len() as u64;
        Ok(())
    }

    /// Replace the whole journal with `lines`, durably.
    pub(crate) fn rewrite(&mut self, lines: &[String]) -> Result<(), PlaceError> {
        let text: String = lines.iter().map(|l| format!("{l}\n")).collect();
        let dir = self.path.parent().unwrap_or(Path::new("."));
        let name = self
            .path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        write_durably(dir, &name, text.as_bytes())?;
        self.len = text.len() as u64;
        Ok(())
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }
}

/// Take `dir`'s lock, or refuse: one keeper per directory.
pub(crate) fn lock_dir(dir: &Path) -> Result<File, PlaceError> {
    fs::create_dir_all(dir)?;
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(dir.join("lock"))?;
    match lock.try_lock() {
        Ok(()) => Ok(lock),
        Err(std::fs::TryLockError::WouldBlock) => Err(PlaceError::Busy(dir.to_path_buf())),
        Err(std::fs::TryLockError::Error(e)) => Err(e.into()),
    }
}

/// Write `name` in `dir` so it survives a power loss: a synced temporary,
/// renamed over, the directory synced.
pub(crate) fn write_durably(dir: &Path, name: &str, bytes: &[u8]) -> Result<(), PlaceError> {
    let temp = dir.join(format!("{name}.tmp"));
    let mut file = File::create(&temp)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    drop(file);
    fs::rename(&temp, dir.join(name))?;
    sync_dir(dir);
    Ok(())
}

/// Make a directory's entries durable; best effort where a platform cannot
/// sync a directory.
pub(crate) fn sync_dir(dir: &Path) {
    if let Ok(d) = File::open(dir) {
        let _ = d.sync_all();
    }
}
