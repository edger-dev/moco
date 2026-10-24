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
//! implements: 7778729bc8018fe949b0225f90ad2832501a9e82b9d03c973d10bdae568f7c8b@7778729bc8018fe949b0225f90ad2832501a9e82b9d03c973d10bdae568f7c8b

use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

use crate::error::PlaceError;
use crate::hash::Hash;
use crate::key::Key;
use crate::place::{
    ByteRange, Capabilities, Child, InventoryEntry, Place, Put, Verified, read_all,
};
use crate::stream::Stream;

const JOURNAL: &str = "journal";
const PUBLISHED: &str = "published";
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

/// The kept state: the records, and how far the journal had got at the
/// last publish.
struct Kept {
    records: BTreeMap<Key, Record>,
    lines: u64,
    published: Published,
}

/// What the last publish covered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Published {
    /// Never published, or the journal was rewritten since: the next
    /// publish is a snapshot.
    Snapshot,
    /// The journal's first `lines` lines, the last entry stamped `ts`.
    Upto { lines: u64, ts: u64 },
}

/// A place with its inventory kept beside it.
pub struct Inventoried<P> {
    place: P,
    dir: PathBuf,
    clock: Box<dyn Fn() -> u64 + Send + Sync>,
    kept: Mutex<Kept>,
}

impl<P: Place> Inventoried<P> {
    /// Keep `place`'s inventory in `dir` (created if missing), picking up the
    /// journal already there.
    pub fn open(place: P, dir: impl Into<PathBuf>) -> Result<Self, PlaceError> {
        let dir = dir.into();
        fs::create_dir_all(&dir)?;
        let mut records = BTreeMap::new();
        let mut lines = 0;
        match fs::read_to_string(dir.join(JOURNAL)) {
            Ok(text) => {
                for line in text.lines() {
                    lines += 1;
                    apply(&mut records, line);
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
        let published = match fs::read_to_string(dir.join(PUBLISHED)) {
            Ok(text) => parse_published(&text),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Published::Snapshot,
            Err(e) => return Err(e.into()),
        };
        Ok(Self {
            place,
            dir,
            clock: Box::new(crate::place::unix_now),
            kept: Mutex::new(Kept {
                records,
                lines,
                published,
            }),
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
        // Every change is applied to the journal before the map, so a panic
        // mid-change leaves the map no newer than the journal.
        self.kept
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Append lines to the journal and apply them.
    fn record(&self, kept: &mut Kept, lines: &[String]) -> Result<(), PlaceError> {
        let mut journal = OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.dir.join(JOURNAL))?;
        let mut text = String::new();
        for line in lines {
            text.push_str(line);
            text.push('\n');
        }
        journal.write_all(text.as_bytes())?;
        journal.sync_data()?;
        for line in lines {
            apply(&mut kept.records, line);
            kept.lines += 1;
        }
        Ok(())
    }

    /// Replace the journal with one line per record.
    fn rewrite(&self, kept: &mut Kept) -> Result<(), PlaceError> {
        let text: String = kept
            .records
            .iter()
            .map(|(key, r)| format!("{}\n", held_line(key, r)))
            .collect();
        let temp = self.dir.join(format!("{JOURNAL}.tmp"));
        fs::write(&temp, text)?;
        fs::rename(&temp, self.dir.join(JOURNAL))?;
        kept.lines = kept.records.len() as u64;
        // Offsets into the old journal mean nothing now.
        self.save_published(kept, Published::Snapshot)
    }

    fn save_published(&self, kept: &mut Kept, published: Published) -> Result<(), PlaceError> {
        let text = match published {
            Published::Snapshot => "snapshot\n".to_owned(),
            Published::Upto { lines, ts } => format!("upto {lines} {ts}\n"),
        };
        let temp = self.dir.join(format!("{PUBLISHED}.tmp"));
        fs::write(&temp, text)?;
        fs::rename(&temp, self.dir.join(PUBLISHED))?;
        kept.published = published;
        Ok(())
    }

    /// Replace the inventory with what a walk of the place finds now.
    pub fn rebuild(&self) -> Result<Vec<InventoryEntry>, PlaceError> {
        let walked = self.place.inventory()?;
        let mut kept = self.kept();
        kept.records = walked
            .iter()
            .map(|e| {
                (
                    e.key.clone(),
                    Record {
                        hash: e.hash.clone(),
                        size: e.size,
                        verified_at: e.verified_at,
                    },
                )
            })
            .collect();
        self.rewrite(&mut kept)?;
        Ok(walked)
    }

    /// Rewrite the journal as one line per object.
    pub fn compact(&self) -> Result<(), PlaceError> {
        let mut kept = self.kept();
        self.rewrite(&mut kept)
    }

    /// Publish what changed since the last publish to `meeting`, under
    /// `inventories/<name>`: a snapshot the first time (or when asked, or
    /// after the journal was rewritten), a delta after; nothing when nothing
    /// changed.
    pub fn publish(
        &self,
        meeting: &dyn Place,
        name: &str,
        snapshot: bool,
    ) -> Result<(), PlaceError> {
        let stream = inventory_stream(name)?;
        let mut kept = self.kept();
        let (content, last_ts) = match (snapshot, kept.published) {
            (false, Published::Upto { lines, ts }) => {
                if lines >= kept.lines {
                    return Ok(());
                }
                let journal = fs::read_to_string(self.dir.join(JOURNAL))?;
                let changed: Vec<&str> = journal.lines().skip(lines as usize).collect();
                (format!("{DELTA}\n{}\n", changed.join("\n")), ts)
            }
            (_, published) => {
                let last_ts = match published {
                    Published::Upto { ts, .. } => ts,
                    Published::Snapshot => 0,
                };
                let lines: Vec<String> = kept
                    .records
                    .iter()
                    .map(|(key, r)| held_line(key, r))
                    .collect();
                (format!("{SNAPSHOT}\n{}\n", lines.join("\n")), last_ts)
            }
        };
        // Strictly later than the last entry, so a delta never sorts before
        // what it follows.
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        let ts = now.max(last_ts + 1);
        stream.append_at(meeting, ts, content.as_bytes())?;
        let lines = kept.lines;
        self.save_published(&mut kept, Published::Upto { lines, ts })
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
    /// inventory checks bytes as they land. One already present and not yet
    /// recorded is recorded as never verified.
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
            Put::Stored => (self.clock)(),
            Put::Present if kept.records.contains_key(key) => return Ok(put),
            Put::Present => 0,
        };
        let record = Record {
            hash: hash.clone(),
            size,
            verified_at,
        };
        self.record(&mut kept, &[held_line(key, &record)])?;
        Ok(put)
    }

    fn get(&self, key: &Key, range: Option<ByteRange>) -> Result<Box<dyn Read + Send>, PlaceError> {
        self.place.get(key, range)
    }

    fn exists(&self, key: &Key) -> Result<bool, PlaceError> {
        self.place.exists(key)
    }

    fn list(&self, prefix: Option<&Key>) -> Result<Vec<Child>, PlaceError> {
        self.place.list(prefix)
    }

    /// Hash the object where it is and record what was found: restamped when
    /// intact, its real hash when corrupt, dropped when missing.
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
/// latest snapshot with every later delta applied.
pub fn published(meeting: &dyn Place, name: &str) -> Result<Vec<InventoryEntry>, PlaceError> {
    let stream = inventory_stream(name)?;
    let mut contents = Vec::new();
    for entry in stream.entries(meeting)?.iter().rev() {
        let text = String::from_utf8_lossy(&read_all(meeting, &entry.key)?).into_owned();
        let is_snapshot = text.lines().next() == Some(SNAPSHOT);
        contents.push(text);
        if is_snapshot {
            break;
        }
    }
    let mut records = BTreeMap::new();
    for text in contents.iter().rev() {
        for line in text.lines().skip(1) {
            apply(&mut records, line);
        }
    }
    Ok(entries(&records))
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

/// Apply one journal line; a line that does not parse is skipped (a torn
/// last line after a crash).
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
        ["upto", lines, ts] => match (lines.parse(), ts.parse()) {
            (Ok(lines), Ok(ts)) => Published::Upto { lines, ts },
            _ => Published::Snapshot,
        },
        _ => Published::Snapshot,
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
