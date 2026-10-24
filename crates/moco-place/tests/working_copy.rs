//! The working copy: local reads and writes, read-through on a miss, a
//! write-through queue that survives a restart, eviction local and careful.
//!
//! implements: 7d189e690068d0f7200fcdbb236afe6ab64b6ad6535c8a8a883c3f825b72c79a@7d189e690068d0f7200fcdbb236afe6ab64b6ad6535c8a8a883c3f825b72c79a

#![allow(clippy::unwrap_used, clippy::expect_used, reason = "tests")]

use std::io::Read;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use moco_place::{
    Body, ByteRange, Capabilities, Child, FsPlace, Hash, Key, MemPlace, Place, PlaceError, Put,
    WorkingCopy, put_bytes, read_all,
};

fn key_for(tree: &str, bytes: &[u8]) -> Key {
    let h = Hash::of_content(bytes);
    Key::new(format!("{tree}/{}/{h}", h.shard())).unwrap()
}

/// A remote that can be unplugged.
#[derive(Clone)]
struct Switch {
    inner: MemPlace,
    online: Arc<AtomicBool>,
}

impl Switch {
    fn new() -> Self {
        Self {
            inner: MemPlace::new(),
            online: Arc::new(AtomicBool::new(true)),
        }
    }
    fn set(&self, online: bool) {
        self.online.store(online, Ordering::Relaxed);
    }
    fn up(&self) -> Result<(), PlaceError> {
        if self.online.load(Ordering::Relaxed) {
            Ok(())
        } else {
            Err(PlaceError::Offline)
        }
    }
}

impl Place for Switch {
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            online: self.online.load(Ordering::Relaxed),
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
        self.up()?;
        self.inner.put_once(key, hash, size, body)
    }
    fn get(&self, key: &Key, range: Option<ByteRange>) -> Result<Body, PlaceError> {
        self.up()?;
        self.inner.get(key, range)
    }
    fn exists(&self, key: &Key) -> Result<bool, PlaceError> {
        self.up()?;
        self.inner.exists(key)
    }
    fn list(&self, prefix: Option<&Key>) -> Result<Vec<Child>, PlaceError> {
        self.up()?;
        self.inner.list(prefix)
    }
}

/// A remote whose copy of every object is rotten.
struct Rotten;

impl Place for Rotten {
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            online: true,
            writable: false,
            cold: false,
        }
    }
    fn put_once(&self, _: &Key, _: &Hash, _: u64, _: &mut dyn Read) -> Result<Put, PlaceError> {
        Err(PlaceError::NotWritable)
    }
    fn get(&self, _: &Key, _: Option<ByteRange>) -> Result<Body, PlaceError> {
        Ok(Body::new(&b"rotten!"[..], 7))
    }
    fn exists(&self, _: &Key) -> Result<bool, PlaceError> {
        Ok(true)
    }
    fn list(&self, _: Option<&Key>) -> Result<Vec<Child>, PlaceError> {
        Ok(vec![])
    }
}

struct Device {
    _dir: tempfile::TempDir,
    copy: WorkingCopy<FsPlace>,
}

/// A device whose media goes to `cloud` and `nas`, ledger trees to `cloud`
/// only, keeping the ledger.
fn device(cloud: &Switch, nas: &Switch) -> Device {
    let dir = tempfile::tempdir().unwrap();
    let copy = open(dir.path(), cloud, nas);
    Device { _dir: dir, copy }
}

fn open(dir: &std::path::Path, cloud: &Switch, nas: &Switch) -> WorkingCopy<FsPlace> {
    WorkingCopy::open(
        FsPlace::create(dir.join("local")).unwrap(),
        dir.join("queue"),
    )
    .unwrap()
    .with_remote("cloud", Arc::new(cloud.clone()))
    .unwrap()
    .with_remote("nas", Arc::new(nas.clone()))
    .unwrap()
    .with_route(|key| {
        if key.as_str().starts_with("media/") {
            vec!["cloud".into(), "nas".into()]
        } else {
            vec!["cloud".into()]
        }
    })
    .with_keep(|key| !key.as_str().starts_with("media/"))
}

/// A write is local at once and queued; a push carries it where policy
/// says, and only there.
#[test]
fn a_write_is_local_and_queued_then_pushed_by_policy() {
    let (cloud, nas) = (Switch::new(), Switch::new());
    let d = device(&cloud, &nas);
    let photo = key_for("media", b"photo");
    let note = key_for("store", b"note");
    put_bytes(&d.copy, &photo, b"photo").unwrap();
    put_bytes(&d.copy, &note, b"note").unwrap();
    assert!(d.copy.local().exists(&photo).unwrap());
    assert!(
        !cloud.inner.exists(&photo).unwrap(),
        "nothing leaves before a push"
    );
    assert_eq!(d.copy.pending().len(), 3);

    let report = d.copy.push();
    assert_eq!((report.stored, report.pending), (3, 0), "{report:?}");
    assert!(cloud.inner.exists(&photo).unwrap() && nas.inner.exists(&photo).unwrap());
    assert!(cloud.inner.exists(&note).unwrap());
    assert!(
        !nas.inner.exists(&note).unwrap(),
        "the ledger goes to the cloud only"
    );
    assert_eq!(d.copy.push(), Default::default(), "nothing left to push");
}

/// An offline remote keeps its copies queued, across a restart, until it is
/// back.
#[test]
fn an_offline_remote_keeps_its_queue_across_a_restart() {
    let (cloud, nas) = (Switch::new(), Switch::new());
    let dir = tempfile::tempdir().unwrap();
    let photo = key_for("media", b"trip");
    nas.set(false);
    {
        let copy = open(dir.path(), &cloud, &nas);
        put_bytes(&copy, &photo, b"trip").unwrap();
        let report = copy.push();
        assert_eq!((report.stored, report.pending), (1, 1), "{report:?}");
    }
    let copy = open(dir.path(), &cloud, &nas);
    assert_eq!(copy.pending(), vec![("nas".to_owned(), photo.clone())]);
    nas.set(true);
    assert_eq!(copy.push().stored, 1);
    assert!(nas.inner.exists(&photo).unwrap());
    assert!(copy.pending().is_empty());
}

/// A miss reads through: from the first remote that has it, checked as it
/// lands, and kept locally afterwards.
#[test]
fn a_miss_reads_through_and_stays() {
    let (cloud, nas) = (Switch::new(), Switch::new());
    let d = device(&cloud, &nas);
    let photo = key_for("media", b"remote only");
    put_bytes(&nas.inner, &photo, b"remote only").unwrap();
    assert!(
        !d.copy.exists(&photo).unwrap(),
        "exists says what this device holds"
    );
    assert_eq!(
        d.copy.locate(&photo),
        Some("nas".to_owned()),
        "a remote holds it"
    );
    assert_eq!(read_all(&d.copy, &photo).unwrap(), b"remote only");
    assert!(d.copy.local().exists(&photo).unwrap(), "kept locally");
    nas.set(false);
    assert_eq!(
        read_all(&d.copy, &photo).unwrap(),
        b"remote only",
        "now local"
    );
    assert!(
        d.copy.pending().is_empty(),
        "a fetched copy is not queued back"
    );
}

/// A remote's rotten copy is refused on arrival; the next remote serves.
#[test]
fn a_rotten_remote_copy_is_refused_and_the_next_serves() {
    let good = Switch::new();
    let dir = tempfile::tempdir().unwrap();
    let copy = WorkingCopy::open(
        FsPlace::create(dir.path().join("l")).unwrap(),
        dir.path().join("q"),
    )
    .unwrap()
    .with_remote("rotten", Arc::new(Rotten))
    .unwrap()
    .with_remote("good", Arc::new(good.clone()))
    .unwrap();
    let photo = key_for("media", b"true bytes");
    put_bytes(&good.inner, &photo, b"true bytes").unwrap();
    assert_eq!(read_all(&copy, &photo).unwrap(), b"true bytes");
}

/// With every remote offline, a miss is a miss.
#[test]
fn a_miss_with_every_remote_offline_is_not_found() {
    let (cloud, nas) = (Switch::new(), Switch::new());
    let d = device(&cloud, &nas);
    let photo = key_for("media", b"away");
    put_bytes(&cloud.inner, &photo, b"away").unwrap();
    cloud.set(false);
    nas.set(false);
    assert!(matches!(
        d.copy.get(&photo, None),
        Err(PlaceError::NotFound(_))
    ));
    assert!(!d.copy.exists(&photo).unwrap());
}

/// A pull takes in what a remote holds below a prefix: the ledger trees.
#[test]
fn a_pull_takes_in_a_tree() {
    let (cloud, nas) = (Switch::new(), Switch::new());
    let d = device(&cloud, &nas);
    let a = key_for("staged", b"event a");
    let b = key_for("staged", b"event b");
    let m = key_for("media", b"not this tree");
    for (k, v) in [
        (&a, &b"event a"[..]),
        (&b, b"event b"),
        (&m, b"not this tree"),
    ] {
        put_bytes(&cloud.inner, k, v).unwrap();
    }
    assert_eq!(
        d.copy
            .pull("cloud", &Key::new("staged").unwrap())
            .unwrap()
            .fetched,
        2
    );
    assert!(d.copy.local().exists(&a).unwrap() && d.copy.local().exists(&b).unwrap());
    assert!(!d.copy.local().exists(&m).unwrap());
    assert_eq!(
        d.copy
            .pull("cloud", &Key::new("staged").unwrap())
            .unwrap()
            .fetched,
        0
    );
    assert!(
        d.copy.pending().is_empty(),
        "pulled copies are not queued back"
    );
    assert!(
        d.copy
            .pull("nowhere", &Key::new("staged").unwrap())
            .is_err()
    );
}

/// Eviction drops only the local copy, never what policy keeps, never what
/// is still queued; an evicted object reads through again.
#[test]
fn eviction_is_local_and_careful() {
    let (cloud, nas) = (Switch::new(), Switch::new());
    let d = device(&cloud, &nas);
    let photo = key_for("media", b"big raw");
    let note = key_for("store", b"ledger");
    put_bytes(&d.copy, &photo, b"big raw").unwrap();
    put_bytes(&d.copy, &note, b"ledger").unwrap();

    assert!(d.copy.evict(&photo).is_err(), "still queued");
    d.copy.push();
    assert!(d.copy.evict(&note).is_err(), "the ledger is kept");
    assert!(d.copy.evict(&photo).unwrap());
    assert!(!d.copy.local().exists(&photo).unwrap());
    assert!(cloud.inner.exists(&photo).unwrap(), "the remote copy stays");
    assert!(!d.copy.evict(&photo).unwrap(), "nothing left to evict");
    assert_eq!(
        read_all(&d.copy, &photo).unwrap(),
        b"big raw",
        "reads through again"
    );
}

/// One queue, one keeper: a second working copy on it is turned away.
#[test]
fn a_second_working_copy_on_one_queue_is_refused() {
    let (cloud, nas) = (Switch::new(), Switch::new());
    let dir = tempfile::tempdir().unwrap();
    let first = open(dir.path(), &cloud, &nas);
    assert!(
        WorkingCopy::open(
            FsPlace::create(dir.path().join("local")).unwrap(),
            dir.path().join("queue")
        )
        .is_err()
    );
    drop(first);
    let _again = open(dir.path(), &cloud, &nas);
}

/// A crash mid-line leaves a torn tail; the next queued copy starts on its
/// own line and survives a restart.
#[test]
fn a_torn_queue_tail_does_not_swallow_the_next_copy() {
    let (cloud, nas) = (Switch::new(), Switch::new());
    let dir = tempfile::tempdir().unwrap();
    cloud.set(false);
    nas.set(false);
    {
        let copy = open(dir.path(), &cloud, &nas);
        put_bytes(&copy, &key_for("media", b"one"), b"one").unwrap();
    }
    use std::io::Write;
    std::fs::OpenOptions::new()
        .append(true)
        .open(dir.path().join("queue/queue"))
        .unwrap()
        .write_all(b"+ nas media/ab/to")
        .unwrap();
    {
        let copy = open(dir.path(), &cloud, &nas);
        put_bytes(&copy, &key_for("media", b"two"), b"two").unwrap();
    }
    let copy = open(dir.path(), &cloud, &nas);
    let pending = copy.pending();
    assert_eq!(pending.len(), 4, "{pending:?}");
    assert!(pending.iter().all(|(_, k)| k.named_hash().is_some()));
}

/// An eviction while a put is still writing that key is refused: the put
/// has not queued it yet, but it may be the only copy.
#[test]
fn an_eviction_racing_a_put_is_refused() {
    let (cloud, nas) = (Switch::new(), Switch::new());
    let d = Arc::new(device(&cloud, &nas));
    let photo = key_for("media", b"slow");
    let (release, wait) = std::sync::mpsc::channel::<()>();
    let (started, has_started) = std::sync::mpsc::channel::<()>();

    /// A body that blocks until released.
    struct Slow(
        std::sync::mpsc::Receiver<()>,
        Option<std::sync::mpsc::Sender<()>>,
        bool,
    );
    impl Read for Slow {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            if let Some(s) = self.1.take() {
                let _ = s.send(());
                let _ = self.0.recv();
            }
            if self.2 {
                return Ok(0);
            }
            self.2 = true;
            buf[..4].copy_from_slice(b"slow");
            Ok(4)
        }
    }
    let writer = {
        let d = d.clone();
        let photo = photo.clone();
        std::thread::spawn(move || {
            let mut body = Slow(wait, Some(started), false);
            d.copy
                .put_once(&photo, &Hash::of_content(b"slow"), 4, &mut body)
                .unwrap()
        })
    };
    has_started.recv().unwrap();
    assert!(d.copy.evict(&photo).is_err(), "a put is writing it");
    release.send(()).unwrap();
    writer.join().unwrap();
    assert!(d.copy.local().exists(&photo).unwrap());
    assert_eq!(d.copy.pending().len(), 2);
}

/// A route naming a remote this copy does not have is queued and reported,
/// not dropped — so the write is never evictable as if it were safe.
#[test]
fn a_route_to_an_unknown_remote_is_kept_and_reported() {
    let dir = tempfile::tempdir().unwrap();
    let copy = WorkingCopy::open(
        FsPlace::create(dir.path().join("l")).unwrap(),
        dir.path().join("q"),
    )
    .unwrap()
    .with_route(|_| vec!["r2".into()]);
    let photo = key_for("media", b"typo");
    put_bytes(&copy, &photo, b"typo").unwrap();
    assert_eq!(copy.pending(), vec![("r2".to_owned(), photo.clone())]);
    let report = copy.push();
    assert_eq!((report.pending, report.failed.len()), (1, 1), "{report:?}");
    assert!(copy.evict(&photo).is_err());
    copy.forget_remote("r2").unwrap();
    assert!(copy.pending().is_empty());
    assert!(copy.evict(&photo).unwrap());
}

/// A queued copy whose local object has vanished is reported lost and
/// leaves the queue, rather than failing every push forever.
#[test]
fn a_queued_copy_lost_locally_is_reported_and_dropped() {
    let (cloud, nas) = (Switch::new(), Switch::new());
    let d = device(&cloud, &nas);
    let note = key_for("store", b"vanishing");
    put_bytes(&d.copy, &note, b"vanishing").unwrap();
    use moco_place::LocalRemove;
    d.copy.local().remove(&note).unwrap();
    let report = d.copy.push();
    assert_eq!(report.lost, vec![("cloud".to_owned(), note)]);
    assert!(d.copy.pending().is_empty());
}

/// Once a remote is offline, a push stops asking it.
#[test]
fn a_push_stops_asking_an_offline_remote() {
    let (cloud, nas) = (Switch::new(), Switch::new());
    let d = device(&cloud, &nas);
    for i in 0..5u8 {
        put_bytes(&d.copy, &key_for("store", &[i]), &[i]).unwrap();
    }
    cloud.set(false);
    let report = d.copy.push();
    assert_eq!(report.pending, 5);
    assert!(
        report.failed.is_empty(),
        "offline is not a failure: {report:?}"
    );
}

/// One rotten object does not stop a pull; it is reported, the rest come.
#[test]
fn a_pull_carries_on_past_a_bad_object() {
    let dir = tempfile::tempdir().unwrap();
    let copy = WorkingCopy::open(
        FsPlace::create(dir.path().join("l")).unwrap(),
        dir.path().join("q"),
    )
    .unwrap()
    .with_remote("mixed", Arc::new(Mixed))
    .unwrap();
    let report = copy.pull("mixed", &Key::new("staged").unwrap()).unwrap();
    assert_eq!(report.fetched, 1, "{report:?}");
    assert_eq!(report.failed.len(), 1);
}

/// A remote holding one good and one rotten object under `staged/`.
struct Mixed;

impl Place for Mixed {
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            online: true,
            writable: false,
            cold: false,
        }
    }
    fn put_once(&self, _: &Key, _: &Hash, _: u64, _: &mut dyn Read) -> Result<Put, PlaceError> {
        Err(PlaceError::NotWritable)
    }
    fn get(&self, key: &Key, _: Option<ByteRange>) -> Result<Body, PlaceError> {
        if *key == key_for("staged", b"good") {
            Ok(Body::new(&b"good"[..], 4))
        } else {
            Ok(Body::new(&b"rot"[..], 3))
        }
    }
    fn exists(&self, _: &Key) -> Result<bool, PlaceError> {
        Ok(true)
    }
    fn list(&self, prefix: Option<&Key>) -> Result<Vec<Child>, PlaceError> {
        let good = key_for("staged", b"good");
        let bad = key_for("staged", b"bad");
        Ok(match prefix.map(Key::as_str) {
            Some("staged") => vec![
                Child::Prefix(good.parent().unwrap()),
                Child::Prefix(bad.parent().unwrap()),
            ],
            Some(p) if p == good.parent().unwrap().as_str() => {
                vec![Child::Object { key: good, size: 4 }]
            }
            Some(p) if p == bad.parent().unwrap().as_str() => {
                vec![Child::Object { key: bad, size: 3 }]
            }
            _ => vec![],
        })
    }
}

/// A local disk that cannot take the bytes is that error, not a miss.
#[cfg(unix)]
#[test]
fn a_local_failure_during_read_through_is_not_a_miss() {
    use std::os::unix::fs::PermissionsExt;
    let (cloud, nas) = (Switch::new(), Switch::new());
    let d = device(&cloud, &nas);
    let photo = key_for("media", b"unwritable");
    put_bytes(&cloud.inner, &photo, b"unwritable").unwrap();
    let local = d.copy.local().root().to_path_buf();
    std::fs::set_permissions(&local, std::fs::Permissions::from_mode(0o555)).unwrap();
    let got = d.copy.get(&photo, None);
    std::fs::set_permissions(&local, std::fs::Permissions::from_mode(0o755)).unwrap();
    // Running as root ignores the mode; nothing to check then.
    if got.is_ok() {
        return;
    }
    assert!(!matches!(got, Err(PlaceError::NotFound(_))), "{got:?}");
}

/// A remote's name is one word: it is written in the queue's lines.
#[test]
fn a_remote_name_is_one_word() {
    let dir = tempfile::tempdir().unwrap();
    let copy = WorkingCopy::open(
        FsPlace::create(dir.path().join("l")).unwrap(),
        dir.path().join("q"),
    )
    .unwrap();
    assert!(
        copy.with_remote("my r2", Arc::new(MemPlace::new()))
            .is_err()
    );
}

/// A device with no disk keeps its working copy in memory: the same rules,
/// nothing persisted.
#[test]
fn a_working_copy_can_live_in_memory() {
    let cloud = Switch::new();
    let copy = WorkingCopy::in_memory(MemPlace::new())
        .with_remote("cloud", Arc::new(cloud.clone()))
        .unwrap()
        .with_route(|_| vec!["cloud".into()]);
    let note = key_for("store", b"phone note");
    put_bytes(&copy, &note, b"phone note").unwrap();
    assert_eq!(copy.push().stored, 1);
    assert!(cloud.inner.exists(&note).unwrap());
}
