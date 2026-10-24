//! Place-owned inventories.
//!
//! A place records what it holds — per key, the content's hash, its size and
//! when it was last verified — in a journal on the machine the place belongs
//! to: one line per change, the last line for a key winning. Reading the
//! inventory is then a read, not a walk. Snapshots and deltas of it are
//! published to a meeting point as a stream, so a device that cannot reach
//! the place (a disk in a drawer) still knows what it holds. A walk can
//! always rebuild it.
//!
//! One keeper per inventory: the directory is locked while open.
//!
//! implements: 7778729bc8018fe949b0225f90ad2832501a9e82b9d03c973d10bdae568f7c8b@7778729bc8018fe949b0225f90ad2832501a9e82b9d03c973d10bdae568f7c8b

use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

use crate::error::PlaceError;
use crate::hash::Hash;
use crate::key::Key;
use crate::place::{
    Body, ByteRange, Capabilities, Child, InventoryEntry, Place, Put, Verified, read_all,
};
use crate::stream::Stream;

const JOURNAL: &str = "journal";
const PUBLISHED: &str = "published";
const LOCK: &str = "lock";
const SNAPSHOT: &str = "snapshot";
const DELTA: &str = "delta";

/// Removal: only ever called on the machine a place belongs to. Not part of
/// [`Place`], so nothing that serves a place over the wire can reach it.
pub trait LocalRemove {
    /// Remove the object; `false` when there was none.
    fn remove(&self, key: &Key) -> Result<bool, PlaceError>;
}

/// What the inventory knows of one object.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Record {
    hash: Hash,
    size: u64,
    verified_at: u64,
}

/// What the last publish covered.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Published {
    /// The next publish is a snapshot: never published, or the journal was
    /// rewritten since. `ts` is the last entry's stamp, so the snapshot
    /// sorts after it.
    Snapshot { ts: u64 },
    /// The journal's first `offset` bytes went to stream `name`, its last
    /// entry stamped `ts`.
    Upto { name: String, offset: u64, ts: u64 },
}

impl Published {
    fn ts(&self) -> u64 {
        match self {
            Self::Snapshot { ts } | Self::Upto { ts, .. } => *ts,
        }
    }
}

/// The kept state.
struct Kept {
    records: BTreeMap<Key, Record>,
    /// Bytes in the journal: always ends at a line's end.
    len: u64,
    published: Published,
}

/// A place with its inventory kept beside it.
pub struct Inventoried<P> {
    place: P,
    dir: PathBuf,
    clock: Box<dyn Fn() -> u64 + Send + Sync>,
    kept: Mutex<Kept>,
    /// Held for as long as this keeper lives.
    _lock: File,
}

impl<P: Place> Inventoried<P> {
    /// Keep `place`'s inventory in `dir` (created if missing), picking up the
    /// journal already there. Refused while another keeper holds `dir`.
    pub fn open(place: P, dir: impl Into<PathBuf>) -> Result<Self, PlaceError> {
        let dir = dir.into();
        let lock = lock_dir(&dir)?;
        let (records, len) = replay(&dir.join(JOURNAL))?;
        let published = match fs::read_to_string(dir.join(PUBLISHED)) {
            Ok(text) => parse_published(&text),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Published::Snapshot { ts: 0 },
            Err(e) => return Err(e.into()),
        };
        Ok(Self {
            place,
            dir,
            clock: Box::new(crate::place::unix_now),
            kept: Mutex::new(Kept {
                records,
                len,
                published,
            }),
            _lock: lock,
        })
    }

    /// Stamp verifications with `clock` (unix seconds) instead of the time.
    pub fn with_clock(self, clock: impl Fn() -> u64 + Send + Sync + 'static) -> Self {
        Self {
            clock: Box::new(clock),
            ..self
        }
    }

    pub fn place(&self) -> &P {
        &self.place
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn kept(&self) -> MutexGuard<'_, Kept> {
        // Every change reaches the journal before the map, so a panic
        // mid-change leaves the map no newer than the journal.
        self.kept
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Append lines to the journal, then apply them. A failed write is cut
    /// back off, so the journal always ends at a line's end.
    fn record(&self, kept: &mut Kept, lines: &[String]) -> Result<(), PlaceError> {
        let mut journal = OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.dir.join(JOURNAL))?;
        let text: String = lines.iter().map(|l| format!("{l}\n")).collect();
        let written = journal
            .write_all(text.as_bytes())
            .and_then(|()| journal.sync_data());
        if let Err(e) = written {
            let _ = journal.set_len(kept.len);
            return Err(e.into());
        }
        for line in lines {
            apply(&mut kept.records, line);
        }
        kept.len += text.len() as u64;
        Ok(())
    }

    /// Replace the journal with one line per record of `records`, and adopt
    /// them. The publish state is reset first: a crash after the rename
    /// must not leave it pointing into the old journal.
    fn rewrite(&self, kept: &mut Kept, records: BTreeMap<Key, Record>) -> Result<(), PlaceError> {
        let ts = kept.published.ts();
        self.save_published(kept, Published::Snapshot { ts })?;
        let text: String = records
            .iter()
            .map(|(key, r)| format!("{}\n", held_line(key, r)))
            .collect();
        write_durably(&self.dir, JOURNAL, text.as_bytes())?;
        kept.len = text.len() as u64;
        kept.records = records;
        Ok(())
    }

    fn save_published(&self, kept: &mut Kept, published: Published) -> Result<(), PlaceError> {
        let text = match &published {
            Published::Snapshot { ts } => format!("snapshot {ts}\n"),
            Published::Upto { name, offset, ts } => format!("upto {name} {offset} {ts}\n"),
        };
        write_durably(&self.dir, PUBLISHED, text.as_bytes())?;
        kept.published = published;
        Ok(())
    }

    /// Replace the inventory with what a walk of the place finds now. Changes
    /// recorded while the walk ran are laid over what it found.
    pub fn rebuild(&self) -> Result<Vec<InventoryEntry>, PlaceError> {
        let from = self.kept().len;
        let walked = self.place.inventory()?;
        let mut kept = self.kept();
        let mut records: BTreeMap<Key, Record> = walked
            .into_iter()
            .map(|e| {
                (
                    e.key,
                    Record {
                        hash: e.hash,
                        size: e.size,
                        verified_at: e.verified_at,
                    },
                )
            })
            .collect();
        for line in journal_since(&self.dir.join(JOURNAL), from)?.lines() {
            apply(&mut records, line);
        }
        self.rewrite(&mut kept, records)?;
        Ok(entries(&kept.records))
    }

    /// Rewrite the journal as one line per object.
    pub fn compact(&self) -> Result<(), PlaceError> {
        let mut kept = self.kept();
        let records = kept.records.clone();
        self.rewrite(&mut kept, records)
    }

    /// Publish what changed since the last publish to `meeting`, under
    /// `inventories/<name>`: a delta when the stream there still ends with
    /// this keeper's last publish to that name; a snapshot otherwise — the
    /// first time, when asked, after a rewrite, for a new name or meeting
    /// point. Nothing when nothing changed.
    pub fn publish(
        &self,
        meeting: &dyn Place,
        name: &str,
        snapshot: bool,
    ) -> Result<(), PlaceError> {
        let stream = inventory_stream(name)?;
        let mut kept = self.kept();
        let latest = stream.latest(meeting)?;
        let delta_from = match &kept.published {
            Published::Upto {
                name: published_to,
                offset,
                ts,
            } if !snapshot
                && published_to == name
                && latest.as_ref().map(|e| e.ts) == Some(*ts) =>
            {
                Some(*offset)
            }
            _ => None,
        };
        let content = match delta_from {
            Some(offset) if offset >= kept.len => return Ok(()),
            Some(offset) => format!(
                "{DELTA}\n{}",
                journal_since(&self.dir.join(JOURNAL), offset)?
            ),
            None => {
                let mut text = format!("{SNAPSHOT}\n");
                for (key, r) in &kept.records {
                    text.push_str(&held_line(key, r));
                    text.push('\n');
                }
                text
            }
        };
        // Strictly after both this keeper's last entry and the stream's, so
        // a delta never sorts before what it follows.
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        let after = kept
            .published
            .ts()
            .max(latest.map_or(0, |e| e.ts))
            .saturating_add(1);
        let ts = now.max(after);
        stream.append_at(meeting, ts, content.as_bytes())?;
        let offset = kept.len;
        self.save_published(
            &mut kept,
            Published::Upto {
                name: name.to_owned(),
                offset,
                ts,
            },
        )
    }
}

impl<P: Place + LocalRemove> Inventoried<P> {
    /// Remove an object, and its line from the inventory.
    pub fn remove(&self, key: &Key) -> Result<bool, PlaceError> {
        let mut kept = self.kept();
        let removed = self.place.remove(key)?;
        if removed || kept.records.contains_key(key) {
            self.record(&mut kept, &[gone_line(key)])?;
        }
        Ok(removed)
    }
}

impl<P: Place> Place for Inventoried<P> {
    fn capabilities(&self) -> Capabilities {
        self.place.capabilities()
    }

    /// A stored put is recorded as verified now: a place that keeps an
    /// inventory is local, and checks bytes as they land. One already
    /// present and not yet recorded is recorded as never verified
    /// (`verified_at` 0) under the hash its key names.
    fn put_once(
        &self,
        key: &Key,
        hash: &Hash,
        size: u64,
        body: &mut dyn Read,
    ) -> Result<Put, PlaceError> {
        let put = self.place.put_once(key, hash, size, body)?;
        let mut kept = self.kept();
        let verified_at = match put {
            Put::Present if kept.records.contains_key(key) => return Ok(put),
            Put::Present => 0,
            Put::Stored => (self.clock)(),
        };
        // Removed again before this got the lock: nothing to record.
        if !self.place.exists(key)? {
            return Ok(put);
        }
        let record = Record {
            hash: hash.clone(),
            size,
            verified_at,
        };
        self.record(&mut kept, &[held_line(key, &record)])?;
        Ok(put)
    }

    fn get(&self, key: &Key, range: Option<ByteRange>) -> Result<Body, PlaceError> {
        self.place.get(key, range)
    }

    fn exists(&self, key: &Key) -> Result<bool, PlaceError> {
        self.place.exists(key)
    }

    fn list(&self, prefix: Option<&Key>) -> Result<Vec<Child>, PlaceError> {
        self.place.list(prefix)
    }

    /// Hash the object where it is and record what was found: restamped when
    /// intact, its real hash when corrupt, dropped when missing. (The kept
    /// place is local, so reading it back is the check.)
    fn verify(&self, key: &Key) -> Result<Verified, PlaceError> {
        if self.place.capabilities().cold {
            return Err(PlaceError::Cold);
        }
        let Some(named) = key.named_hash() else {
            return Ok(Verified::Unnamed);
        };
        let found = match self.place.get(key, None) {
            Ok(mut body) => Some(Hash::of_reader(&mut body)?),
            Err(PlaceError::NotFound(_)) => None,
            Err(e) => return Err(e),
        };
        let mut kept = self.kept();
        // Removed while it was being hashed: it is missing now.
        let found = found.filter(|_| self.place.exists(key).unwrap_or(false));
        Ok(match found {
            None => {
                if kept.records.contains_key(key) {
                    self.record(&mut kept, &[gone_line(key)])?;
                }
                Verified::Missing
            }
            Some((hash, size)) => {
                let record = Record {
                    hash: hash.clone(),
                    size,
                    verified_at: (self.clock)(),
                };
                self.record(&mut kept, &[held_line(key, &record)])?;
                if hash == named {
                    Verified::Intact
                } else {
                    Verified::Corrupt { got: hash }
                }
            }
        })
    }

    /// The kept inventory — a read, not a walk.
    fn inventory(&self) -> Result<Vec<InventoryEntry>, PlaceError> {
        Ok(entries(&self.kept().records))
    }
}

/// What a place published to a meeting point under `inventories/<name>`: the
/// latest snapshot with every later delta applied. Deltas with no snapshot
/// under them are refused, not read as a partial inventory.
pub fn published(meeting: &dyn Place, name: &str) -> Result<Vec<InventoryEntry>, PlaceError> {
    let stream = inventory_stream(name)?;
    let all = stream.entries(meeting)?;
    if all.is_empty() {
        return Ok(Vec::new());
    }
    let mut contents = Vec::new();
    let mut based = false;
    for entry in all.iter().rev() {
        let text = String::from_utf8_lossy(&read_all(meeting, &entry.key)?).into_owned();
        let is_snapshot = text.lines().next() == Some(SNAPSHOT);
        contents.push(text);
        if is_snapshot {
            based = true;
            break;
        }
    }
    if !based {
        return Err(PlaceError::Refused {
            key: stream.prefix().clone(),
            reason: "its deltas have no snapshot under them",
        });
    }
    let mut records = BTreeMap::new();
    for text in contents.iter().rev() {
        for line in text.lines().skip(1) {
            apply(&mut records, line);
        }
    }
    Ok(entries(&records))
}

/// Take `dir`'s lock, or refuse: one keeper per directory.
pub(crate) fn lock_dir(dir: &Path) -> Result<File, PlaceError> {
    fs::create_dir_all(dir)?;
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(dir.join(LOCK))?;
    match lock.try_lock() {
        Ok(()) => Ok(lock),
        Err(std::fs::TryLockError::WouldBlock) => Err(PlaceError::Busy(dir.to_path_buf())),
        Err(std::fs::TryLockError::Error(e)) => Err(e.into()),
    }
}

/// Replay a journal: the records, and its length up to the last whole line.
/// A torn tail (a crash mid-write) is cut off, so the next line starts clean.
fn replay(path: &Path) -> Result<(BTreeMap<Key, Record>, u64), PlaceError> {
    let file = match File::open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok((BTreeMap::new(), 0)),
        Err(e) => return Err(e.into()),
    };
    let mut records = BTreeMap::new();
    let mut len = 0u64;
    let mut reader = BufReader::new(file);
    let mut line = String::new();
    loop {
        line.clear();
        let n = reader.read_line(&mut line)?;
        if n == 0 || !line.ends_with('\n') {
            break;
        }
        apply(&mut records, line.trim_end_matches('\n'));
        len += n as u64;
    }
    let total = fs::metadata(path)?.len();
    if total > len {
        OpenOptions::new().write(true).open(path)?.set_len(len)?;
    }
    Ok((records, len))
}

/// The journal's text from byte `offset` on.
fn journal_since(path: &Path, offset: u64) -> Result<String, PlaceError> {
    let mut file = match File::open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(String::new()),
        Err(e) => return Err(e.into()),
    };
    file.seek(SeekFrom::Start(offset))?;
    let mut text = String::new();
    file.read_to_string(&mut text)?;
    Ok(text)
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
    if let Ok(d) = File::open(dir) {
        let _ = d.sync_all();
    }
    Ok(())
}

fn inventory_stream(name: &str) -> Result<Stream, PlaceError> {
    Ok(Stream::new(Key::new("inventories")?.join(name)?))
}

fn held_line(key: &Key, r: &Record) -> String {
    format!("{key} {} {} {}", r.hash, r.size, r.verified_at)
}

fn gone_line(key: &Key) -> String {
    format!("{key} -")
}

/// Apply one journal line; one that does not parse is skipped.
fn apply(records: &mut BTreeMap<Key, Record>, line: &str) {
    let fields: Vec<&str> = line.split_whitespace().collect();
    match fields.as_slice() {
        [key, "-"] => {
            if let Ok(key) = Key::new(*key) {
                records.remove(&key);
            }
        }
        [key, hash, size, verified_at] => {
            if let (Ok(key), Ok(hash), Ok(size), Ok(verified_at)) = (
                Key::new(*key),
                hash.parse(),
                size.parse(),
                verified_at.parse(),
            ) {
                records.insert(
                    key,
                    Record {
                        hash,
                        size,
                        verified_at,
                    },
                );
            }
        }
        _ => {}
    }
}

fn parse_published(text: &str) -> Published {
    let fields: Vec<&str> = text.split_whitespace().collect();
    match fields.as_slice() {
        ["upto", name, offset, ts] => match (offset.parse(), ts.parse()) {
            (Ok(offset), Ok(ts)) => Published::Upto {
                name: (*name).to_owned(),
                offset,
                ts,
            },
            _ => Published::Snapshot { ts: 0 },
        },
        ["snapshot", ts] => Published::Snapshot {
            ts: ts.parse().unwrap_or(0),
        },
        _ => Published::Snapshot { ts: 0 },
    }
}

fn entries(records: &BTreeMap<Key, Record>) -> Vec<InventoryEntry> {
    records
        .iter()
        .map(|(key, r)| InventoryEntry {
            key: key.clone(),
            hash: r.hash.clone(),
            size: r.size,
            verified_at: r.verified_at,
        })
        .collect()
}
